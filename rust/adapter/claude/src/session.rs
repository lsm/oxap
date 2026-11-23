use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures_channel::oneshot;
use oxap_adapter::{
    refuse_unadvertised_controls, runtime::Runtime, subscriber::Subscriber, BoxFuture, Error,
    EventStream, InteractionResolution, Recovery, ResumeRequest, Session, SubmitRequest,
    ToolCatalog, ToolLister,
};
use oxap_protocol::{
    ActiveRun, Admission as RunAdmission, ContentDeltaPayload, ContentPart, ContentPartType,
    EffectiveDeliveryMode, Envelope, EnvelopeType, Message, MessageContent, MessageId, MessageRole,
    MessageSubmitRequest, MessageSubmitResponse, ProtocolError, RequestedDeliveryMode,
    RunCancelResponse, RunCompletedPayload, RunFailedPayload, RunId, RunStartedPayload, RunStatus,
    SessionId, SessionState, SessionStatus, ToolsListRequest, Usage, FEATURE_TOOLS_LIST,
};
use serde::Deserialize;
use serde_json::value::to_raw_value;

use crate::{
    adapter::{native_uuid, pin},
    catalog::Catalog,
    native::{self, Observation},
    rpc::{ClientError, Inbound, ObservationMessage, Process, DEFAULT_QUEUE_CAPACITY},
};

const RPC_TIMEOUT: Duration = Duration::from_secs(60);
type Admission = Result<(MessageSubmitResponse, EventStream), Error>;

pub(crate) struct OpenedSession {
    process: Process,
    state: Arc<Mutex<SessionData>>,
    expand_prompts: bool,
    runtime: Runtime,
}

struct SessionData {
    session: SessionState,
    catalog: Catalog,
    pending: Option<PendingTurn>,
    active: Option<ActiveTurn>,
}

struct PendingTurn {
    uuid: String,
    model: String,
    corroborated: Option<oneshot::Sender<()>>,
    buffered: Vec<(Observation, bool)>,
}

struct ActiveTurn {
    uuid: String,
    run_id: RunId,
    message_id: MessageId,
    events: Subscriber,
}

#[derive(Deserialize)]
struct Echo {
    #[serde(default, deserialize_with = "oxap_adapter::gojson::zero")]
    user_message_uuid: String,
    #[serde(default, deserialize_with = "oxap_adapter::gojson::strings")]
    user_message_uuids: Vec<String>,
    parent_tool_use_id: Option<String>,
    origin: Option<native::Origin>,
}

struct AbandonPending<'a> {
    state: &'a Mutex<SessionData>,
    uuid: &'a str,
}

impl Drop for AbandonPending<'_> {
    fn drop(&mut self) {
        self.state.lock().unwrap().release_pending(self.uuid, true);
    }
}

impl Echo {
    fn corroborates(observation: &ObservationMessage, uuid: &str) -> Option<bool> {
        let echo: Self = native::source::decode(&String::from_utf8_lossy(&observation.raw)).ok()?;
        if echo.parent_tool_use_id.is_some()
            || echo.origin.is_some_and(|origin| origin.kind != "human")
        {
            return None;
        }
        if echo.user_message_uuid == uuid || echo.user_message_uuids.iter().any(|id| id == uuid) {
            return Some(true);
        }
        if !echo.user_message_uuid.is_empty() || !echo.user_message_uuids.is_empty() {
            return None;
        }
        Some(matches!(&observation.value, Observation::Status(status) if status.compacting()))
    }
}

impl SessionData {
    fn observe(&mut self, observation: ObservationMessage, runtime: &Runtime) {
        if let Some(turn) = &mut self.pending {
            let Some(corroborates) = Echo::corroborates(&observation, &turn.uuid) else {
                return;
            };
            if matches!(observation.value, Observation::Unknown { .. }) && !corroborates {
                return;
            }
            if turn.buffered.len() == DEFAULT_QUEUE_CAPACITY {
                self.pending.take();
                self.session.status = SessionStatus::CLOSED;
                return;
            }
            turn.buffered.push((observation.value, corroborates));
            if corroborates {
                if let Some(signal) = turn.corroborated.take() {
                    let _ = signal.send(());
                }
            }
        } else if let Some(corroborates) = self.active.as_ref().map_or(Some(false), |turn| {
            Echo::corroborates(&observation, &turn.uuid)
        }) {
            self.apply(observation.value, corroborates, runtime);
        }
    }

    fn apply(&mut self, observation: Observation, corroborates: bool, runtime: &Runtime) {
        if let Observation::Init(init) = observation {
            self.catalog.observe(&init);
            self.session.current_model_id = init.model;
            if !init.session_id.is_empty() {
                bind(&mut self.session, &init.session_id);
            }
            return;
        }
        let Some(turn) = &mut self.active else { return };
        let session_id = &self.session.session_id;
        let mut event = match observation {
            Observation::StreamEvent(frame) => {
                let Some((kind, text)) = frame.stream_delta() else {
                    return;
                };
                let mut part = ContentPart::default();
                if kind == "thinking" {
                    part.part_type = ContentPartType::REASONING;
                    part.reasoning = text;
                } else {
                    part.part_type = ContentPartType::TEXT;
                    part.text = text;
                }
                let delta = ContentDeltaPayload {
                    session_id: session_id.clone(),
                    run_id: turn.run_id.clone(),
                    message_id: Some(turn.message_id.clone()),
                    part,
                };
                let id = runtime.new_id("event");
                Envelope::new(EnvelopeType::CONTENT_DELTA, id, &delta)
                    .expect("delta payload serializes")
            }
            Observation::Result(frame)
                if corroborates
                    && !frame.cancelled()
                    && frame.queued_turn_count.unwrap_or(0) <= 0 =>
            {
                turn.terminal(session_id, &frame, runtime.new_id("event"))
            }
            _ => return,
        };
        let sequence = self.session.active_runs[0].as_of_sequence.get_or_insert(1);
        *sequence += 1;
        self.session.updated_at_ms = runtime.now_ms();
        event.sequence = Some(*sequence);
        event.timestamp_ms = Some(self.session.updated_at_ms);
        event.session_id = Some(session_id.clone());
        event.run_id = Some(turn.run_id.clone());
        event.capability_revision = Some(pin().capability_revision.clone());
        let terminal = event.envelope_type == EnvelopeType::RUN_COMPLETED
            || event.envelope_type == EnvelopeType::RUN_FAILED;
        turn.events.offer(event, terminal);
        if terminal {
            self.active = None;
            self.session.status = SessionStatus::IDLE;
            self.session.active_run_id = None;
            self.session.active_runs.clear();
        }
    }

    fn start(&mut self, runtime: &Runtime) -> Admission {
        let turn = self.pending.take().ok_or(Error::SessionClosed)?;
        let session_id = self.session.session_id.clone();
        let message_id: MessageId = runtime.new_id("message").into();
        let run_id: RunId = runtime.new_id("run").into();
        let now = runtime.now_ms();
        let mut start = Envelope::new(
            EnvelopeType::RUN_STARTED,
            runtime.new_id("event"),
            &RunStartedPayload {
                session_id: session_id.clone(),
                run_id: run_id.clone(),
                status: RunStatus::RUNNING,
                model_id: turn.model.clone(),
                started_at_ms: now,
                ..Default::default()
            },
        )
        .map_err(Error::harness)?;
        start.session_id = Some(session_id.clone());
        start.run_id = Some(run_id.clone());
        start.sequence = Some(1);
        start.timestamp_ms = Some(now);
        start.capability_revision = Some(pin().capability_revision.clone());
        let events = Subscriber::new(DEFAULT_QUEUE_CAPACITY);
        events.offer(start, false);
        self.session.status = SessionStatus::RUNNING;
        self.session.updated_at_ms = now;
        self.session.active_run_id = Some(run_id.clone());
        self.session.active_runs = vec![ActiveRun {
            run_id: run_id.clone(),
            status: RunStatus::RUNNING,
            relationship: "primary".into(),
            as_of_sequence: Some(1),
            ..Default::default()
        }];
        self.active = Some(ActiveTurn {
            uuid: turn.uuid,
            run_id: run_id.clone(),
            message_id: message_id.clone(),
            events: events.clone(),
        });
        for (observation, corroborates) in turn.buffered {
            self.apply(observation, corroborates, runtime);
        }
        Ok((
            MessageSubmitResponse {
                session_id,
                accepted: true,
                submission_id: message_id.as_str().into(),
                requested_delivery: RequestedDeliveryMode::AUTO,
                effective_delivery: EffectiveDeliveryMode::START,
                delivery_resolution: "session_idle".into(),
                admission: RunAdmission::STARTED,
                run_id: Some(run_id),
                status: RunStatus::RUNNING,
                model_id: turn.model,
                message_ids: vec![message_id],
                ..Default::default()
            },
            events.stream(),
        ))
    }

    fn release_pending(&mut self, uuid: &str, retire: bool) {
        if self.pending.as_ref().is_some_and(|turn| turn.uuid == uuid) {
            self.pending.take();
            if retire {
                self.session.status = SessionStatus::CLOSED;
            }
        }
    }
}

impl ActiveTurn {
    fn terminal(&self, session: &SessionId, frame: &native::ResultFrame, id: String) -> Envelope {
        let [input, output, cache_read, cache_write] = [
            frame.usage.input_tokens,
            frame.usage.output_tokens,
            frame.usage.cache_read_input_tokens,
            frame.usage.cache_creation_input_tokens,
        ]
        .map(|count| count.and_then(|count| u64::try_from(count).ok()));
        let usage = Some(Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: cache_read,
            cache_write_input_tokens: cache_write,
            total_tokens: [input, output, cache_read, cache_write]
                .into_iter()
                .try_fold(0_u64, |sum, count| sum.checked_add(count?)),
            ..Default::default()
        });
        let duration_ms = frame.duration_ms.max(0);
        let max_turns = frame.max_turns();
        let success = frame.subtype == native::RESULT_SUCCESS;
        let mut event = if max_turns || (success && !frame.is_error) {
            let stop_reason = if max_turns {
                "max_turns"
            } else {
                [
                    frame.terminal_reason.as_str(),
                    frame.stop_reason.as_deref().unwrap_or_default(),
                ]
                .into_iter()
                .find(|reason| !reason.is_empty())
                .unwrap_or("completed")
            };
            let completed = RunCompletedPayload {
                session_id: session.clone(),
                run_id: self.run_id.clone(),
                final_response: Message {
                    id: Some(self.message_id.clone()),
                    role: MessageRole::ASSISTANT,
                    content: MessageContent::from_text(&frame.result),
                    ..Default::default()
                },
                stop_reason: stop_reason.into(),
                usage,
                duration_ms,
                ..Default::default()
            };
            Envelope::new(EnvelopeType::RUN_COMPLETED, id, &completed)
        } else {
            let reason = if success && !frame.terminal_reason.is_empty() {
                &frame.terminal_reason
            } else {
                &frame.subtype
            };
            let code = frame.api_error_status.map_or_else(
                || format!("claude_{reason}"),
                |status| format!("claude_api_{status}"),
            );
            let message = if !frame.errors.is_empty() {
                frame.errors.join("; ")
            } else if !frame.result.trim().is_empty() {
                frame.result.trim().into()
            } else if !success {
                frame.subtype.clone()
            } else if let Some(status) = frame.api_error_status {
                format!("API error (HTTP {status})")
            } else {
                "unknown error".into()
            };
            let failed = RunFailedPayload {
                session_id: session.clone(),
                run_id: self.run_id.clone(),
                error: ProtocolError {
                    code,
                    message,
                    ..Default::default()
                },
                usage,
                duration_ms,
                ..Default::default()
            };
            Envelope::new(EnvelopeType::RUN_FAILED, id, &failed)
        }
        .expect("terminal payload serializes");
        let total_cost = frame.total_cost_usd.and_then(|cost| {
            oxap_adapter::gojson::go_float(cost)
                .parse::<serde_json::Number>()
                .ok()
        });
        let mut cost = serde_json::json!({
            "num_turns": frame.num_turns,
            "total_cost_usd": total_cost,
            "cache_read_input_tokens": frame.usage.cache_read_input_tokens,
            "cache_creation_input_tokens": frame.usage.cache_creation_input_tokens,
        });
        let cost = cost.as_object_mut().expect("cost is an object");
        cost.retain(|_, value| !value.is_null());
        if !cost.is_empty() {
            event.extensions.insert(
                "com.anthropic.claude-code.cost".into(),
                to_raw_value(cost).expect("cost serializes"),
            );
        }
        event
    }
}

impl OpenedSession {
    pub(crate) fn new(
        process: Process,
        state: SessionState,
        expand_prompts: bool,
        runtime: Runtime,
    ) -> Result<Self, Error> {
        let state = Arc::new(Mutex::new(SessionData {
            session: state,
            catalog: Catalog::default(),
            pending: None,
            active: None,
        }));
        let session = Self {
            process,
            state,
            expand_prompts,
            runtime,
        };
        let inbound = session.process.client.inbound();
        let state = session.state.clone();
        let runtime = session.runtime.clone();
        std::thread::Builder::new()
            .spawn(move || {
                for frame in inbound {
                    match frame {
                        Inbound::Observation(observation) => {
                            state.lock().unwrap().observe(observation, &runtime);
                        }
                        Inbound::Control(control) => {
                            let deadline = Some(Instant::now() + RPC_TIMEOUT);
                            let _ = control.respond_error("claude adapter: no owned run", deadline);
                        }
                        _ => {}
                    }
                }
                let mut state = state.lock().unwrap();
                state.session.status = SessionStatus::CLOSED;
                if let Some(turn) = state.active.take() {
                    turn.events.close();
                }
                state.pending.take();
            })
            .map_err(Error::harness)?;
        Ok(session)
    }

    pub(crate) fn exchange(&self, name: &str, body: String) -> Result<(), Error> {
        let deadline = Some(Instant::now() + RPC_TIMEOUT);
        let result = self
            .process
            .client
            .call(body.as_bytes(), deadline)
            .map_err(|error| Error::harness(format!("claude adapter: {name} failed: {error}")))?;
        if let Some(body) = result {
            native::source::decode_object::<Option<serde::de::IgnoredAny>>(
                &String::from_utf8_lossy(&body),
            )
            .map_err(|error| {
                Error::harness(format!(
                    "claude adapter: {name} failed: decode control response: {error}"
                ))
            })?;
        }
        Ok(())
    }

    async fn submit_turn(&self, request: MessageSubmitRequest) -> Admission {
        let text = submit_text(&request)?;
        let uuid = format!("turn-{}", native_uuid()?);
        native::validate_turn_uuid(&uuid).map_err(Error::harness)?;
        let frame =
            native::new_user_turn(&uuid, &text, !self.expand_prompts).map_err(Error::harness)?;
        let (signal, corroborated) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            if state.session.status == SessionStatus::CLOSED {
                return Err(Error::SessionClosed);
            }
            if state.session.session_id != request.session_id {
                return Err(Error::RunNotFound);
            }
            if state.pending.is_some() || state.session.status != SessionStatus::IDLE {
                return Err(Error::RunActive);
            }
            state.pending = Some(PendingTurn {
                uuid: uuid.clone(),
                model: state.session.current_model_id.clone(),
                corroborated: Some(signal),
                buffered: Vec::new(),
            });
        }

        let _abandon = AbandonPending {
            state: &self.state,
            uuid: &uuid,
        };
        let (written, write_result) = oneshot::channel();
        let client = self.process.client.clone();
        let state = self.state.clone();
        let turn_uuid = uuid.clone();
        let worker = std::thread::Builder::new().spawn(move || {
            let result = client.write_user(frame.as_bytes(), Some(Instant::now() + RPC_TIMEOUT));
            if let Err(error) = &result {
                state
                    .lock()
                    .unwrap()
                    .release_pending(&turn_uuid, *error != ClientError::WriteQueue);
            }
            let _ = written.send(result.map_err(Error::harness));
        });
        if let Err(error) = worker {
            self.state.lock().unwrap().release_pending(&uuid, false);
            return Err(Error::harness(error));
        }
        write_result.await.map_err(|_| Error::SessionClosed)??;
        corroborated.await.map_err(|_| Error::SessionClosed)?;
        let mut state = self.state.lock().unwrap();
        state.start(&self.runtime)
    }
}

fn submit_text(request: &MessageSubmitRequest) -> Result<String, Error> {
    refuse_unadvertised_controls(request, &[])?;
    let [message] = request.messages.as_slice() else {
        return Err(Error::InvalidSubmission);
    };
    if request.session_id.as_str().is_empty()
        || message.role != MessageRole::USER
        || (!request.delivery.is_empty() && request.delivery != RequestedDeliveryMode::AUTO)
    {
        return Err(Error::InvalidSubmission);
    }
    if let Some(text) = message.content.text() {
        return Ok(text);
    }
    if message.content.raw().is_some_and(|raw| raw.get() == "null") {
        return Ok(String::new());
    }
    let parts = message.content.parts().ok_or(Error::InvalidSubmission)?;
    if parts
        .iter()
        .any(|part| part.part_type != ContentPartType::TEXT)
    {
        return Err(Error::InvalidSubmission);
    }
    Ok(parts
        .into_iter()
        .map(|part| part.text)
        .collect::<Vec<_>>()
        .join("\n"))
}

pub(crate) fn bind(state: &mut SessionState, id: &str) {
    state.metadata.insert(
        "claude_native_session_id".into(),
        to_raw_value(id).expect("string serializes to JSON"),
    );
}

impl Session for OpenedSession {
    fn state(&self) -> BoxFuture<'_, SessionState> {
        Box::pin(async {
            let state = self.state.lock().unwrap();
            if state.session.status == SessionStatus::CLOSED {
                return Err(Error::SessionClosed);
            }
            Ok(state.session.clone())
        })
    }

    fn native_session_id(&self) -> Option<String> {
        let state = self.state.lock().unwrap();
        let metadata = &state.session.metadata;
        serde_json::from_str(metadata.get("claude_native_session_id")?.get()).ok()
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            {
                let mut state = self.state.lock().unwrap();
                if state.pending.is_some() || state.active.is_some() {
                    return Err(Error::RunActive);
                }
                state.session.status = SessionStatus::CLOSED;
            }
            self.process.close(None).map_err(Error::harness)
        })
    }

    fn submit(&self, submit: SubmitRequest) -> BoxFuture<'_, (MessageSubmitResponse, EventStream)> {
        Box::pin(self.submit_turn(submit.request))
    }

    fn resolve(&self, _: InteractionResolution) -> BoxFuture<'_, ()> {
        Box::pin(async { Err(Error::InteractionNotFound) })
    }

    fn cancel(&self, _: RunId) -> BoxFuture<'_, RunCancelResponse> {
        Box::pin(async { Err(Error::RunNotFound) })
    }

    fn resume(&self, _: ResumeRequest) -> BoxFuture<'_, (Recovery, EventStream)> {
        Box::pin(async { Err(Error::RunNotFound) })
    }

    fn tool_lister(&self) -> Option<&dyn ToolLister> {
        Some(self)
    }
}

impl ToolLister for OpenedSession {
    fn tools(&self, request: ToolsListRequest) -> BoxFuture<'_, ToolCatalog> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            if state.session.status == SessionStatus::CLOSED {
                return Err(Error::SessionClosed);
            }
            let id = request.session_id.as_ref();
            let id = id.filter(|id| !id.as_str().is_empty());
            if id.is_some_and(|id| *id != state.session.session_id) {
                return Err(Error::RunNotFound);
            }
            if !request.allows_degraded(FEATURE_TOOLS_LIST) {
                return Err(Error::DegradedControl(FEATURE_TOOLS_LIST.into()));
            }
            Ok(ToolCatalog {
                revision: pin().capability_revision.clone(),
                tools: state.catalog.list(id.cloned()),
            })
        })
    }
}

impl Drop for OpenedSession {
    fn drop(&mut self) {
        let _ = self.process.close(None);
    }
}
