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
    ActiveRun, Admission as RunAdmission, ContentPartType, EffectiveDeliveryMode, Envelope,
    EnvelopeType, MessageId, MessageRole, MessageSubmitRequest, MessageSubmitResponse,
    RequestedDeliveryMode, RunCancelResponse, RunId, RunStartedPayload, RunStatus, SessionState,
    SessionStatus, ToolsListRequest, FEATURE_TOOLS_LIST,
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
    buffered: Vec<ObservationMessage>,
}

struct ActiveTurn {
    uuid: String,
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
    fn observe(&mut self, observation: ObservationMessage) {
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
            turn.buffered.push(observation);
            if corroborates {
                if let Some(signal) = turn.corroborated.take() {
                    let _ = signal.send(());
                }
            }
        } else if self
            .active
            .as_ref()
            .is_none_or(|turn| Echo::corroborates(&observation, &turn.uuid).is_some())
        {
            self.apply(observation);
        }
    }

    fn apply(&mut self, observation: ObservationMessage) {
        if let Observation::Init(init) = observation.value {
            self.catalog.observe(&init);
            self.session.current_model_id = init.model;
            if !init.session_id.is_empty() {
                bind(&mut self.session, &init.session_id);
            }
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
        if self.session.status == SessionStatus::CLOSED {
            events.close();
        } else {
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
                events: events.clone(),
            });
        }
        for observation in turn.buffered {
            self.apply(observation);
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
        std::thread::Builder::new()
            .spawn(move || {
                for frame in inbound {
                    match frame {
                        Inbound::Observation(observation) => {
                            state.lock().unwrap().observe(observation);
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
                if state
                    .pending
                    .as_ref()
                    .is_some_and(|turn| turn.corroborated.is_some())
                {
                    state.pending.take();
                }
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
