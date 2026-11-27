use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures_channel::oneshot;
use oxap_adapter::validate_input_answer;
use oxap_adapter::{
    journal::Journal, refuse_unadvertised_controls, refuse_unadvertised_live_settings,
    runtime::Runtime, BoxFuture, Error, EventStream, InteractionResolution, Recovery,
    ResumeRequest, Session, SettingsUpdater, SubmitRequest, ToolCatalog, ToolLister,
    UnsupportedControl, CONTROL_UNSATISFIABLE,
};
use oxap_adapter::{CallResolution, CallResolver};
use oxap_protocol::{
    ActionCallPayload, ActiveRun, Admission as RunAdmission, ContentDeltaPayload, ContentPart,
    ContentPartType, EffectiveDeliveryMode, Envelope, EnvelopeId, EnvelopeType, Message,
    MessageContent, MessageId, MessageRole, MessageSubmitRequest, MessageSubmitResponse,
    ProtocolError, RequestedDeliveryMode, RunCancelResponse, RunCancelledPayload,
    RunCompletedPayload, RunFailedPayload, RunId, RunStartedPayload, RunStatus, SessionId,
    SessionState, SessionStatus, ToolsListRequest, Usage, FEATURE_TOOLS_LIST, SETTLED_BY_INFERRED,
};
use oxap_protocol::{
    ActionCallResolveDetails, ActionCallResolveResponse, ResolveReason, RESOLVE_ARM_ACKNOWLEDGE,
};
use oxap_protocol::{
    CapabilityDescriptor, CompactionId, CompactionOutcome, CompactionReason, InputAnswer,
    InputOption, InputQuestion, RunCompactionEndedPayload, RunCompactionStartedPayload,
    RunStatusUpdatedPayload, SessionSettingsUpdateRequest, SessionSettingsUpdateResponse,
    ToolChoice, UserInputRequestedPayload, UserInputResolvedPayload, FEATURE_TOOL_SELECTION,
};
use serde::Deserialize;
use serde_json::value::{to_raw_value, RawValue};

use crate::{
    adapter::{native_uuid, pin, Adapter, ENDPOINT_ID},
    catalog::{self, Catalog},
    native::{self, ControlRequest, Observation},
    rpc::{
        ClientError, Inbound, IncomingControl, ObservationMessage, Process, DEFAULT_QUEUE_CAPACITY,
    },
};

const RPC_TIMEOUT: Duration = Duration::from_secs(60);
const REFUSED_BY_POLICY: &str = "refused_by_policy";
const NO_MCP_CALL: &str = r#"{"code":-32602,"message":"no provided call matches this tools/call"}"#;
const MCP_CANCELLED: &str = r#"{"code":-32603,"message":"the call was cancelled"}"#;
fn deadline() -> Option<Instant> {
    Some(Instant::now() + RPC_TIMEOUT)
}
type Admission = Result<(MessageSubmitResponse, EventStream), Error>;
type CancelResult = Result<RunCancelResponse, Error>;

pub(crate) struct OpenedSession {
    process: Process,
    state: Arc<Mutex<SessionData>>,
    expand_prompts: bool,
    operation: Mutex<()>,
    runtime: Runtime,
}

struct SessionData {
    session: SessionState,
    catalog: Catalog,
    journal: Journal,
    pending: Option<PendingTurn>,
    active: Option<ActiveTurn>,
    interrupt: Option<Interrupt>,
    settled_runs: HashMap<RunId, RunStatus>,
    tool_calls: HashMap<String, (RunId, usize)>,
    settled_calls: Vec<ObservedTool>,
    participant: oxap_protocol::ParticipantId,
    unusable: bool,
}

struct Interrupt {
    waiters: Vec<oneshot::Sender<CancelResult>>,
    terminal: Option<Box<native::ResultFrame>>,
}

struct PendingTurn {
    uuid: String,
    model: String,
    corroborated: Option<oneshot::Sender<()>>,
    buffered: Vec<TurnFrame>,
    policy: RunToolPolicy,
}

enum TurnFrame {
    Observation(Observation, bool),
    Permission(Arc<IncomingControl>),
    Mcp(Arc<IncomingControl>),
}

impl PendingTurn {
    fn has_terminal(&self) -> bool {
        let mut settlement = RunSettlement::default();
        self.buffered.iter().any(|entry| match entry {
            TurnFrame::Observation(frame, corroborates) => settlement.observe(frame, *corroborates),
            TurnFrame::Permission(_) | TurnFrame::Mcp(_) => false,
        })
    }
}

#[derive(Default)]
struct RunSettlement {
    children: HashSet<String>,
    queued_work: Option<bool>,
}

impl RunSettlement {
    fn observe(&mut self, observation: &Observation, corroborates: bool) -> bool {
        match observation {
            Observation::Result(frame) if corroborates => {
                self.queued_work = Some(frame.queued_turn_count.unwrap_or_default() > 0);
            }
            Observation::TaskStarted(frame)
                if matches!(frame.task_type.as_str(), "local_agent" | "local_workflow") =>
            {
                self.children.insert(frame.task_id.clone());
            }
            Observation::TaskNotification(frame) => {
                self.children.remove(&frame.task_id);
            }
            Observation::TaskUpdated(frame) if frame.terminal() => {
                self.children.remove(&frame.task_id);
            }
            Observation::SessionState(frame) if frame.state == "idle" => {
                return self.queued_work.is_some();
            }
            _ => return false,
        }
        self.queued_work == Some(false) && self.children.is_empty()
    }
}

struct ActiveTurn {
    uuid: String,
    run_id: RunId,
    message_id: MessageId,
    cancel_accepted: bool,
    tools: Vec<ObservedTool>,
    gates: Vec<PermissionGate>,
    policy: RunToolPolicy,
    settlement: RunSettlement,
    deferred: Option<Box<native::ResultFrame>>,
    compaction: Option<CompactionId>,
}

#[derive(Default)]
struct RunToolPolicy {
    choice: Option<ToolChoice>,
    denied: HashSet<String>,
}

impl RunToolPolicy {
    fn excludes(&self, name: &str) -> bool {
        self.choice
            .as_ref()
            .is_some_and(|choice| !choice.permits(name, &[], false))
    }

    fn deny(
        &mut self,
        control: &IncomingControl,
        name: &str,
        native_id: &str,
        encode: impl FnOnce(&str) -> Result<String, serde_json::Error>,
    ) -> bool {
        if !self.excludes(name) {
            return false;
        }
        let reason = format!("{name} is excluded by this run's tool_choice");
        let reply = encode(&reason)
            .and_then(RawValue::from_string)
            .expect("policy denial serializes");
        if control.respond(Some(&reply), deadline()).is_ok() && !native_id.is_empty() {
            self.denied.insert(native_id.into());
        }
        true
    }
}

struct ObservedTool {
    native_id: String,
    payload: ActionCallPayload,
    progress: ToolProgress,
}

enum ToolProgress {
    Native(Option<EnvelopeId>),
    Provided(Option<Box<ProvidedCall>>),
}

#[derive(Default)]
struct ProvidedCall {
    requested: EnvelopeId,
    started: Option<EnvelopeId>,
    settlement: Option<EnvelopeId>,
    held: Option<(Arc<IncomingControl>, Box<RawValue>)>,
    reply: Option<String>,
}

impl ObservedTool {
    fn cancel_provided(&mut self, runtime: &Runtime) -> Option<Envelope> {
        let ToolProgress::Provided(Some(call)) = &mut self.progress else {
            return None;
        };
        if call.settlement.is_some() {
            return None;
        }
        let kind = EnvelopeType::ACTION_CALL_CANCELLED;
        let event = tool_event(kind, &self.payload, call.started.clone(), runtime);
        call.settlement = Some(event.id.clone());
        if let Some((control, id)) = call.held.take() {
            send_mcp(&control, &id, "error", MCP_CANCELLED);
        }
        Some(event)
    }
}

fn send_mcp(control: &IncomingControl, id: &RawValue, key: &str, payload: &str) {
    let response = catalog::reply(id, key, payload).expect("MCP reply serializes");
    let _ = control.respond(response.as_deref(), deadline());
}

struct PermissionGate {
    control: Arc<IncomingControl>,
    payload: UserInputRequestedPayload,
    request_id: EnvelopeId,
}

impl PermissionGate {
    fn resolved(self, answers: Vec<InputAnswer>, runtime: &Runtime) -> Envelope {
        let request = self.payload;
        let status = if answers.is_empty() {
            "cancelled"
        } else {
            "submitted"
        };
        let payload = UserInputResolvedPayload {
            interaction_id: request.interaction_id,
            requested_by: request.requested_by,
            responded_by: request.responded_by,
            session_id: request.session_id,
            run_id: request.run_id,
            status: status.into(),
            answers,
        };
        let id = runtime.new_id("event");
        let mut event = Envelope::new(EnvelopeType::USER_INPUT_RESOLVED, id, &payload).unwrap();
        event.turn_id = Some(payload.interaction_id);
        event.in_reply_to = Some(self.request_id);
        event
    }
}

fn tool_event(
    kind: EnvelopeType,
    payload: &ActionCallPayload,
    reply: Option<EnvelopeId>,
    runtime: &Runtime,
) -> Envelope {
    let mut event =
        Envelope::new(kind, runtime.new_id("event"), payload).expect("tool payload serializes");
    event.tool_call_id = Some(payload.tool_call_id.clone());
    event.in_reply_to = reply;
    event
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
    fn unavailable(&self) -> bool {
        self.unusable || self.session.status == SessionStatus::CLOSED
    }

    fn require_idle(&self, session_id: &SessionId) -> Result<(), Error> {
        if self.unavailable() {
            return Err(Error::SessionClosed);
        }
        if self.session.session_id != *session_id {
            return Err(Error::RunNotFound);
        }
        if self.pending.is_some()
            || self.interrupt.is_some()
            || self.session.status != SessionStatus::IDLE
        {
            return Err(Error::RunActive);
        }
        Ok(())
    }

    fn interrupt_has_terminal(&self) -> bool {
        self.interrupt
            .as_ref()
            .is_some_and(|interrupt| interrupt.terminal.is_some())
    }

    fn finish_interrupt(&mut self, outcome: CancelResult, runtime: &Runtime) {
        let interrupt = self.interrupt.take().expect("interrupt is reserved");
        if let Some(turn) = &mut self.active {
            turn.cancel_accepted = outcome.is_ok();
        }
        for waiter in interrupt.waiters {
            let _ = waiter.send(outcome.clone());
        }
        if let Some(frame) = interrupt.terminal {
            self.publish_result(frame, runtime);
        }
    }

    fn route_control(&mut self, control: Arc<IncomingControl>, runtime: &Runtime) {
        let deadline = Some(Instant::now() + RPC_TIMEOUT);
        let rejection = match &control.value {
            ControlRequest::HookCallback(hook)
                if hook.callback_id == native::TOOL_SELECTION_HOOK
                    && hook.input.hook_event_name == native::HOOK_PRE_TOOL_USE =>
            {
                let settled = self.interrupt_has_terminal();
                let policy = if let Some(turn) = &mut self.pending {
                    (!turn.has_terminal()).then_some(&mut turn.policy)
                } else {
                    self.active
                        .as_mut()
                        .filter(|_| !settled)
                        .map(|turn| &mut turn.policy)
                };
                let input = &hook.input;
                let denied = policy.is_some_and(|policy| {
                    policy.deny(
                        &control,
                        self.catalog.policy_name(&input.tool_name),
                        &input.tool_use_id,
                        native::deny_tool,
                    )
                });
                if !denied {
                    let _ = control.respond(None, deadline);
                }
                return;
            }
            ControlRequest::HookCallback(_) => "claude adapter: unregistered hook callback",
            ControlRequest::CanUseTool(_) => {
                if let Some(turn) = self
                    .pending
                    .as_mut()
                    .filter(|turn| turn.corroborated.is_none())
                {
                    if turn.buffered.len() == DEFAULT_QUEUE_CAPACITY {
                        "claude adapter: permission control queue full"
                    } else {
                        turn.buffered.push(TurnFrame::Permission(control));
                        return;
                    }
                } else {
                    self.apply_permission(control, runtime);
                    return;
                }
            }
            ControlRequest::McpMessage(request)
                if request.server_name == catalog::SDK_SERVER
                    && !self.catalog.provided.is_empty() =>
            {
                let raw = request.message.as_deref().map_or("null", RawValue::get);
                let message: catalog::Message =
                    native::source::decode_object(raw).unwrap_or_default();
                if let (Some(id), "tools/call") = (&message.id, message.method.as_str()) {
                    if let Some(turn) = &mut self.pending {
                        if turn.buffered.len() == DEFAULT_QUEUE_CAPACITY {
                            return send_mcp(&control, id, "error", NO_MCP_CALL);
                        }
                        turn.buffered.push(TurnFrame::Mcp(control));
                    } else {
                        let native_id = message.params["_meta"]["claudecode/toolUseId"].as_str();
                        self.receive_mcp(control, id.clone(), native_id.unwrap_or(""));
                    }
                    return;
                }
                let _ = match catalog::discover(&self.catalog.provided, message) {
                    Ok(body) => control.respond(body.as_deref(), deadline),
                    Err(error) => control.respond_error(&error.to_string(), deadline),
                };
                return;
            }
            ControlRequest::McpMessage(_) => "claude adapter: no such SDK MCP server",
            _ => "claude adapter: no owned run",
        };
        let _ = control.respond_error(rejection, deadline);
    }

    fn receive_mcp(&mut self, control: Arc<IncomingControl>, id: Box<RawValue>, native_id: &str) {
        let active = self.active.iter_mut().flat_map(|turn| &mut turn.tools);
        let tool = active
            .chain(&mut self.settled_calls)
            .find(|tool| tool.native_id == native_id);
        if let Some(ToolProgress::Provided(Some(call))) = tool.map(|tool| &mut tool.progress) {
            if let Some(result) = call.reply.take() {
                return send_mcp(&control, &id, "result", &result);
            }
            if call.held.is_none() && call.settlement.is_none() {
                call.held = Some((control, id));
                return;
            }
        }
        send_mcp(&control, &id, "error", NO_MCP_CALL);
    }

    fn apply_permission(&mut self, control: Arc<IncomingControl>, runtime: &Runtime) {
        let settled = self.interrupt_has_terminal();
        let Some(turn) = self.active.as_mut().filter(|_| !settled) else {
            let message = "claude adapter: permission ask outside an owned run";
            let _ = control.respond_error(message, deadline());
            return;
        };
        let ControlRequest::CanUseTool(ask) = &control.value else {
            unreachable!()
        };
        if turn.policy.deny(
            &control,
            self.catalog.policy_name(&ask.tool_name),
            &ask.tool_use_id,
            |reason| native::permission_deny(reason, false),
        ) {
            return;
        }
        let tool_call_id = self
            .tool_calls
            .get(&ask.tool_use_id)
            .filter(|(run, _)| *run == turn.run_id)
            .map(|(_, index)| turn.tools[*index].payload.tool_call_id.clone());
        let name = &ask.tool_name;
        let display = |value: &str| (!value.is_empty()).then(|| value.to_owned());
        let option = |id: &str, label: &str| InputOption {
            id: id.into(),
            label: label.into(),
            ..Default::default()
        };
        let payload = UserInputRequestedPayload {
            interaction_id: runtime.new_id("interaction").into(),
            requested_by: ENDPOINT_ID.into(),
            responded_by: display(self.participant.as_str())
                .unwrap_or_else(|| "user".into())
                .into(),
            session_id: self.session.session_id.clone(),
            run_id: turn.run_id.clone(),
            tool_call_id,
            title: display(&ask.title).unwrap_or_else(|| format!("Use {name}")),
            description: display(&ask.description).unwrap_or_else(|| ask.decision_reason.clone()),
            questions: vec![InputQuestion {
                id: "decision".into(),
                prompt: format!("{name} {}", ask.input.as_ref().unwrap().get()),
                kind: "single_choice".into(),
                required: true,
                options: vec![option("allow", "Allow"), option("deny", "Deny")],
            }],
            allow_cancel: true,
            ..Default::default()
        };
        let id = runtime.new_id("event");
        let mut event = Envelope::new(EnvelopeType::USER_INPUT_REQUESTED, id, &payload).unwrap();
        event.turn_id = Some(payload.interaction_id.clone());
        event.tool_call_id = payload.tool_call_id.clone();
        turn.gates.push(PermissionGate {
            control,
            payload,
            request_id: event.id.clone(),
        });
        self.append_event(event, false, runtime);
        self.interaction_status(runtime);
    }

    fn resolve_gate(
        &mut self,
        resolution: InteractionResolution,
        runtime: &Runtime,
    ) -> Result<(), Error> {
        let input = resolution.input.filter(|_| !self.interrupt_has_terminal());
        let answer = input.ok_or(Error::InteractionNotFound)?;
        let turn = self.active.as_mut().ok_or(Error::InteractionNotFound)?;
        let index = turn
            .gates
            .iter()
            .position(|gate| gate.payload.interaction_id == answer.interaction_id)
            .ok_or(Error::InteractionNotFound)?;
        let gate = &turn.gates[index];
        let request = &gate.payload;
        let responders = [resolution.responded_by, answer.responded_by];
        let differs = |given: &str, expected: &str| !given.is_empty() && given != expected;
        let responder = request.responded_by.as_str();
        if !responders.contains(&request.responded_by)
            || responders.iter().any(|id| differs(id.as_str(), responder))
        {
            return Err(Error::WrongResponder);
        }
        let run = request.run_id.as_str();
        if differs(resolution.run_id.as_str(), run)
            || differs(answer.run_id.as_str(), run)
            || differs(answer.session_id.as_str(), request.session_id.as_str())
            || differs(answer.requested_by.as_str(), ENDPOINT_ID)
        {
            return Err(Error::InvalidResolution);
        }
        let [decision] = answer.answers.as_slice() else {
            return Err(Error::InvalidResolution);
        };
        validate_input_answer(&request.questions[0], decision)?;
        let ControlRequest::CanUseTool(ask) = &gate.control.value else {
            unreachable!()
        };
        let body = if decision.selected_option_ids[0] == "allow" {
            native::permission_allow(ask.input.as_deref())
        } else {
            native::permission_deny("Denied by the operator", false)
        }
        .and_then(serde_json::value::RawValue::from_string)
        .map_err(Error::harness)?;
        gate.control
            .respond(Some(&body), deadline())
            .map_err(|error| match error {
                ClientError::ReverseResolved => Error::InteractionNotFound,
                _ => Error::harness(error),
            })?;
        self.finish_gate(index, answer.answers, runtime);
        Ok(())
    }

    fn withdraw_gate(&mut self, request_id: &str, runtime: &Runtime) {
        if let Some(turn) = &mut self.pending {
            turn.buffered.retain(|frame| {
                !matches!(frame, TurnFrame::Permission(control) | TurnFrame::Mcp(control) if control.id == request_id)
            });
        }
        if let Some(turn) = &self.active {
            if let Some(index) = turn
                .gates
                .iter()
                .position(|gate| gate.control.id == request_id)
            {
                self.finish_gate(index, Vec::new(), runtime);
            }
        }
    }

    fn finish_gate(&mut self, index: usize, answers: Vec<InputAnswer>, runtime: &Runtime) {
        let gate = self.active.as_mut().unwrap().gates.remove(index);
        let event = gate.resolved(answers, runtime);
        self.append_event(event, false, runtime);
        self.interaction_status(runtime);
    }

    fn interaction_status(&mut self, runtime: &Runtime) {
        let turn = self.active.as_ref().unwrap();
        let pending = turn
            .tools
            .iter()
            .rev()
            .find(|tool| matches!(&tool.progress, ToolProgress::Provided(Some(call)) if call.settlement.is_none()))
            .and_then(|tool| tool.payload.interaction_id.clone())
            .or_else(|| {
                turn.gates
                    .last()
                    .map(|gate| gate.payload.interaction_id.clone())
            });
        let run = &mut self.session.active_runs[0];
        run.status = if pending.is_some() {
            RunStatus::WAITING_FOR_INPUT
        } else {
            RunStatus::RUNNING
        };
        self.session.status = run.status.as_str().into();
        let payload = RunStatusUpdatedPayload {
            session_id: self.session.session_id.clone(),
            run_id: run.run_id.clone(),
            status: run.status.clone(),
            pending_user_input_id: pending,
            updated_at_ms: runtime.now_ms(),
        };
        let id = runtime.new_id("event");
        let event = Envelope::new(EnvelopeType::RUN_STATUS_UPDATED, id, &payload).unwrap();
        self.append_event(event, false, runtime);
    }

    fn observe(&mut self, observation: ObservationMessage, runtime: &Runtime) {
        if let Some(turn) = &mut self.pending {
            let Some(corroborates) = Echo::corroborates(&observation, &turn.uuid) else {
                return;
            };
            if matches!(observation.value, Observation::Unknown { .. }) && !corroborates {
                return;
            }
            if turn.buffered.len() == DEFAULT_QUEUE_CAPACITY {
                if !turn.has_terminal() {
                    self.pending.take();
                    self.unusable = true;
                }
                return;
            }
            turn.buffered
                .push(TurnFrame::Observation(observation.value, corroborates));
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
        if self.interrupt_has_terminal() {
            return;
        }
        let Some(turn) = &mut self.active else { return };
        let session_id = &self.session.session_id;
        let can_settle = turn.settlement.observe(&observation, corroborates);
        let event = match observation {
            Observation::Status(frame) if frame.compacting() => {
                if turn.compaction.is_some() {
                    return;
                }
                let id: CompactionId = runtime.new_id("compaction").into();
                turn.compaction = Some(id.clone());
                let payload = RunCompactionStartedPayload {
                    session_id: session_id.clone(),
                    run_id: turn.run_id.clone(),
                    compaction_id: id,
                    reason: CompactionReason::THRESHOLD,
                    ..Default::default()
                };
                Some(
                    Envelope::new(
                        EnvelopeType::RUN_COMPACTION_STARTED,
                        runtime.new_id("event"),
                        &payload,
                    )
                    .expect("compaction start payload serializes"),
                )
            }
            Observation::Status(frame) => {
                let Some(result) = frame.compact_result else {
                    return;
                };
                let outcome = if result == native::COMPACT_RESULT_SUCCESS {
                    Ok(CompactionOutcome::COMPLETED)
                } else {
                    Err(ProtocolError {
                        code: "claude_compaction_failed".into(),
                        message: frame
                            .compact_error
                            .filter(|message| !message.is_empty())
                            .unwrap_or_else(|| {
                                format!("the CLI reported its compaction as {result}")
                            }),
                        ..Default::default()
                    })
                };
                turn.end_compaction(session_id, outcome, runtime)
            }
            Observation::SystemNotice(frame)
                if frame.subtype == native::SYSTEM_COMPACT_BOUNDARY =>
            {
                turn.end_compaction(session_id, Ok(CompactionOutcome::COMPLETED), runtime)
            }
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
                Some(
                    Envelope::new(EnvelopeType::CONTENT_DELTA, runtime.new_id("event"), &delta)
                        .expect("delta payload serializes"),
                )
            }
            Observation::Result(frame) if corroborates => {
                turn.deferred = Some(frame);
                None
            }
            Observation::Assistant(frame) => {
                return self.apply_tools(frame.message.content.unwrap_or_default(), true, runtime);
            }
            Observation::User(frame) => {
                return self.apply_tools(frame.blocks().unwrap_or_default(), false, runtime);
            }
            _ => None,
        };
        if can_settle {
            let frame = turn
                .deferred
                .take()
                .expect("settlement requires a matching result");
            self.publish_result(frame, runtime);
        } else if let Some(event) = event {
            self.publish(event, runtime);
        }
    }

    fn publish_result(&mut self, frame: Box<native::ResultFrame>, runtime: &Runtime) {
        let turn = self
            .active
            .as_ref()
            .expect("settlement requires an active run");
        if frame.cancelled() && !turn.cancel_accepted {
            if let Some(interrupt) = &mut self.interrupt {
                interrupt.terminal = Some(frame);
                return;
            }
        }
        let event = turn.terminal(&self.session.session_id, &frame, runtime.new_id("event"));
        self.publish(event, runtime);
    }

    fn apply_tools(
        &mut self,
        blocks: Vec<native::ContentBlock>,
        assistant: bool,
        runtime: &Runtime,
    ) {
        for block in blocks {
            let outcome = match (assistant, block.kind.as_str()) {
                (true, "tool_use") if !block.id.is_empty() => self.start_tool(block, runtime),
                (false, "tool_result") if !block.tool_use_id.is_empty() => {
                    self.finish_tool(block, runtime)
                }
                _ => continue,
            };
            if let Err(message) = outcome {
                let payload = RunFailedPayload {
                    session_id: self.session.session_id.clone(),
                    run_id: self.active.as_ref().unwrap().run_id.clone(),
                    error: ProtocolError {
                        code: "claude_tool_lifecycle".into(),
                        message: message.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let event =
                    Envelope::new(EnvelopeType::RUN_FAILED, runtime.new_id("event"), &payload)
                        .expect("tool lifecycle failure serializes");
                self.publish(event, runtime);
                return;
            }
        }
    }

    fn start_tool(
        &mut self,
        block: native::ContentBlock,
        runtime: &Runtime,
    ) -> Result<(), &'static str> {
        if block.name.is_empty() {
            return Err("tool call without a name");
        }
        if self.tool_calls.contains_key(&block.id) {
            return Err("duplicate tool call");
        }
        let turn = self.active.as_mut().unwrap();
        self.tool_calls
            .insert(block.id.clone(), (turn.run_id.clone(), turn.tools.len()));
        let provided = self.catalog.provided_tool(&block.name);
        let mut payload = ActionCallPayload {
            session_id: self.session.session_id.clone(),
            run_id: turn.run_id.clone(),
            tool_call_id: runtime.new_id("tool-call").into(),
            requested_by: Some(ENDPOINT_ID.into()),
            execution_owner: "claude-code".into(),
            source: self.catalog.attribution_for(&block.name).into(),
            name: block.name,
            arguments_json: Some(block.input.unwrap_or_else(|| to_raw_value(&()).unwrap())),
            ..Default::default()
        };
        if let Some(provided) = provided {
            let pending = !turn.policy.excludes(&provided.name);
            payload.name.clone_from(&provided.name);
            payload.source.clone_from(&provided.source);
            payload.execution_owner = provided.execution_owner.clone();
            payload.responded_by = Some(provided.execution_owner.clone());
            let requested = pending.then(|| {
                payload.interaction_id = Some(runtime.new_id("interaction").into());
                tool_event(EnvelopeType::ACTION_CALL_REQUESTED, &payload, None, runtime)
            });
            payload.arguments_json = None;
            turn.tools.push(ObservedTool {
                native_id: block.id,
                payload,
                progress: ToolProgress::Provided(requested.as_ref().map(|event| {
                    Box::new(ProvidedCall {
                        requested: event.id.clone(),
                        ..Default::default()
                    })
                })),
            });
            if let Some(event) = requested {
                self.append_event(event, false, runtime);
                self.interaction_status(runtime);
            }
            return Ok(());
        }
        let requested = tool_event(EnvelopeType::ACTION_CALL_REQUESTED, &payload, None, runtime);
        payload.arguments_json = None;
        let started = tool_event(
            EnvelopeType::ACTION_CALL_STARTED,
            &payload,
            Some(requested.id.clone()),
            runtime,
        );
        turn.tools.push(ObservedTool {
            native_id: block.id,
            payload,
            progress: ToolProgress::Native(Some(started.id.clone())),
        });
        self.append_event(requested, false, runtime);
        self.append_event(started, false, runtime);
        Ok(())
    }

    fn finish_tool(
        &mut self,
        block: native::ContentBlock,
        runtime: &Runtime,
    ) -> Result<(), &'static str> {
        let Some((run, index)) = self.tool_calls.get(&block.tool_use_id) else {
            return Err("unmatched tool completion");
        };
        let turn = self.active.as_mut().unwrap();
        if *run != turn.run_id {
            return Ok(());
        }
        let tool = &mut turn.tools[*index];
        let denied = turn.policy.denied.remove(&block.tool_use_id);
        let ToolProgress::Native(started) = &mut tool.progress else {
            if let Some(event) = tool.cancel_provided(runtime) {
                self.append_event(event, false, runtime);
                self.interaction_status(runtime);
            }
            return Ok(());
        };
        let started = started.take().ok_or("unmatched tool completion")?;
        let mut payload = tool.payload.clone();
        let kind = if block.is_error == Some(true) {
            let message = block
                .content
                .as_deref()
                .and_then(|raw| native::source::decode::<String>(raw.get()).ok())
                .filter(|message| !message.is_empty())
                .unwrap_or_else(|| "tool call failed".into());
            payload.error = Some(ProtocolError {
                code: if denied {
                    REFUSED_BY_POLICY
                } else {
                    "claude_tool_error"
                }
                .into(),
                message,
                ..Default::default()
            });
            EnvelopeType::ACTION_CALL_FAILED
        } else {
            payload.result = Some(match block.content {
                Some(raw) if raw.get().starts_with('"') => {
                    let text =
                        native::source::decode::<String>(raw.get()).expect("raw JSON string");
                    to_raw_value(&text).unwrap()
                }
                Some(raw) if raw.get() != "null" => raw,
                _ => to_raw_value("").unwrap(),
            });
            EnvelopeType::ACTION_CALL_COMPLETED
        };
        self.append_event(
            tool_event(kind, &payload, Some(started), runtime),
            false,
            runtime,
        );
        Ok(())
    }

    fn append_event(&mut self, mut event: Envelope, terminal: bool, runtime: &Runtime) {
        let turn = self
            .active
            .as_ref()
            .expect("publishing requires an active run");
        let sequence = self.session.active_runs[0].as_of_sequence.get_or_insert(1);
        *sequence += 1;
        self.session.transcript_cursor = sequence.to_string();
        self.session.updated_at_ms = runtime.now_ms();
        event.sequence = Some(*sequence);
        event.timestamp_ms = Some(self.session.updated_at_ms);
        event.session_id = Some(self.session.session_id.clone());
        event.run_id = Some(turn.run_id.clone());
        event.capability_revision = Some(pin().capability_revision.clone());
        self.journal.append(event, terminal);
    }

    fn publish(&mut self, event: Envelope, runtime: &Runtime) {
        let settled = match event.envelope_type.as_str() {
            "run.completed" => Some(RunStatus::COMPLETED),
            "run.failed" => Some(RunStatus::FAILED),
            "run.cancelled" => Some(RunStatus::CANCELLED),
            _ => None,
        };
        if let Some(status) = settled {
            let turn = self.active.as_mut().unwrap();
            let run_id = turn.run_id.clone();
            let denied = std::mem::take(&mut turn.policy.denied);
            for mut tool in std::mem::take(&mut turn.tools) {
                if let Some(event) = tool.cancel_provided(runtime) {
                    self.append_event(event, false, runtime);
                }
                if matches!(tool.progress, ToolProgress::Provided(Some(_))) {
                    self.settled_calls.push(tool);
                } else if let ToolProgress::Native(Some(started)) = tool.progress {
                    let mut payload = tool.payload;
                    let kind = if denied.contains(&tool.native_id) {
                        payload.error = Some(ProtocolError {
                            code: REFUSED_BY_POLICY.into(),
                            message: "refused by the run's tool_choice".into(),
                            ..Default::default()
                        });
                        EnvelopeType::ACTION_CALL_FAILED
                    } else {
                        EnvelopeType::ACTION_CALL_CANCELLED
                    };
                    let settled = tool_event(kind, &payload, Some(started), runtime);
                    self.append_event(settled, false, runtime);
                }
            }
            for gate in std::mem::take(&mut self.active.as_mut().unwrap().gates) {
                let _ = gate
                    .control
                    .respond_error("claude adapter: run settled", deadline());
                self.append_event(gate.resolved(Vec::new(), runtime), false, runtime);
            }
            let turn = self.active.as_mut().unwrap();
            if turn.compaction.is_some() {
                let outcome = if status == RunStatus::CANCELLED {
                    Ok(CompactionOutcome::CANCELLED)
                } else {
                    Err(ProtocolError {
                        code: "claude_compaction_unreported".into(),
                        message: "the run ended before the CLI reported how its compaction ended"
                            .into(),
                        ..Default::default()
                    })
                };
                if let Some(ended) = turn.end_compaction(&self.session.session_id, outcome, runtime)
                {
                    self.append_event(ended, false, runtime);
                }
            }
            self.append_event(event, true, runtime);
            self.settled_runs.insert(run_id, status);
            self.active = None;
            self.session.status = SessionStatus::IDLE;
            self.session.active_run_id = None;
            self.session.active_runs.clear();
        } else {
            self.append_event(event, false, runtime);
        }
    }

    fn disconnect(&mut self, error: Option<ClientError>, runtime: &Runtime) {
        if let Some(turn) = self
            .active
            .as_ref()
            .filter(|_| !self.interrupt_has_terminal())
        {
            let message = match error {
                Some(ClientError::Transport(detail)) if detail == "EOF" => {
                    "io: read/write on closed pipe".into()
                }
                Some(error) => error.to_string(),
                None => "io: read/write on closed pipe".into(),
            };
            let payload = RunFailedPayload {
                session_id: self.session.session_id.clone(),
                run_id: turn.run_id.clone(),
                error: ProtocolError {
                    code: "claude_process_exit".into(),
                    message,
                    ..Default::default()
                },
                settled_by: SETTLED_BY_INFERRED.into(),
                ..Default::default()
            };
            let failure =
                Envelope::new(EnvelopeType::RUN_FAILED, runtime.new_id("event"), &payload)
                    .expect("disconnect payload serializes");
            self.publish(failure, runtime);
        }
        if !self.pending.as_ref().is_some_and(PendingTurn::has_terminal) {
            self.pending = None;
        }
        self.unusable = true;
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
        let stream = self.journal.follow(run_id.as_str(), 0);
        self.journal.append(start, false);
        self.session.status = SessionStatus::RUNNING;
        self.session.transcript_cursor = "1".into();
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
            cancel_accepted: false,
            tools: Vec::new(),
            gates: Vec::new(),
            policy: turn.policy,
            settlement: RunSettlement::default(),
            deferred: None,
            compaction: None,
        });
        for entry in turn.buffered {
            match entry {
                TurnFrame::Observation(frame, corroborates) => {
                    self.apply(frame, corroborates, runtime)
                }
                TurnFrame::Permission(control) => self.apply_permission(control, runtime),
                TurnFrame::Mcp(control) => self.route_control(control, runtime),
            }
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
            stream,
        ))
    }

    fn release_pending(&mut self, uuid: &str, retire: bool) {
        if self.pending.as_ref().is_some_and(|turn| turn.uuid == uuid) {
            self.pending.take();
            if retire {
                self.unusable = true;
            }
        }
    }
}

impl ActiveTurn {
    fn end_compaction(
        &mut self,
        session_id: &SessionId,
        outcome: Result<CompactionOutcome, ProtocolError>,
        runtime: &Runtime,
    ) -> Option<Envelope> {
        let (outcome, error) = match outcome {
            Ok(outcome) => (outcome, None),
            Err(error) => (CompactionOutcome::FAILED, Some(error)),
        };
        let payload = RunCompactionEndedPayload {
            session_id: session_id.clone(),
            run_id: self.run_id.clone(),
            compaction_id: self.compaction.take()?,
            outcome,
            error,
            ..Default::default()
        };
        Some(
            Envelope::new(
                EnvelopeType::RUN_COMPACTION_ENDED,
                runtime.new_id("event"),
                &payload,
            )
            .expect("compaction end payload serializes"),
        )
    }

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
        let aborted = frame.cancelled();
        let mut event = if aborted && self.cancel_accepted {
            let cancelled = RunCancelledPayload {
                session_id: session.clone(),
                run_id: self.run_id.clone(),
                reason: format!(
                    "interrupt confirmed by terminal_reason {}",
                    frame.terminal_reason
                ),
                usage,
                duration_ms,
                ..Default::default()
            };
            Envelope::new(EnvelopeType::RUN_CANCELLED, id, &cancelled)
        } else if !aborted && (max_turns || (success && !frame.is_error)) {
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
            let reason = if aborted || (success && !frame.terminal_reason.is_empty()) {
                &frame.terminal_reason
            } else {
                &frame.subtype
            };
            let code = match frame.api_error_status {
                Some(status) if !aborted => format!("claude_api_{status}"),
                _ => format!("claude_{reason}"),
            };
            let message = if !frame.errors.is_empty() {
                frame.errors.join("; ")
            } else if !frame.result.trim().is_empty() {
                frame.result.trim().into()
            } else if !success && !frame.subtype.is_empty() {
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
        let total_cost = frame
            .total_cost_usd
            .map(oxap_adapter::gojson::go_float)
            .and_then(|cost| cost.parse::<serde_json::Number>().ok());
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
        if let Some(output) = frame
            .structured_output
            .as_ref()
            .filter(|raw| raw.get() != "null")
        {
            event.extensions.insert(
                "com.anthropic.claude-code.structured_output".into(),
                output.clone(),
            );
        }
        event
    }
}

impl OpenedSession {
    pub(crate) fn new(
        process: Process,
        state: SessionState,
        participant: oxap_protocol::ParticipantId,
        expand_prompts: bool,
        journal_capacity: usize,
        runtime: Runtime,
        provided: Vec<oxap_protocol::ToolDefinition>,
    ) -> Result<Self, Error> {
        let mut catalog = Catalog::default();
        catalog.provided = provided;
        let state = Arc::new(Mutex::new(SessionData {
            session: state,
            catalog,
            journal: Journal::new(journal_capacity),
            pending: None,
            active: None,
            interrupt: None,
            settled_runs: HashMap::new(),
            tool_calls: HashMap::new(),
            settled_calls: Vec::new(),
            participant,
            unusable: false,
        }));
        let session = Self {
            process,
            state,
            expand_prompts,
            operation: Mutex::new(()),
            runtime,
        };
        let client = session.process.client.clone();
        let inbound = client.inbound();
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
                            state.lock().unwrap().route_control(control, &runtime);
                        }
                        Inbound::Cancel(notice) => {
                            state
                                .lock()
                                .unwrap()
                                .withdraw_gate(&notice.request_id, &runtime);
                        }
                        _ => {}
                    }
                }
                state.lock().unwrap().disconnect(client.err(), &runtime);
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
        let choice = request.tool_choice_policy().map_err(|error| {
            Error::UnsupportedControl(Box::new(UnsupportedControl {
                feature: FEATURE_TOOL_SELECTION.into(),
                reason: CONTROL_UNSATISFIABLE,
                detail: error.to_string(),
                ..Default::default()
            }))
        })?;
        let uuid = format!("turn-{}", native_uuid()?);
        native::validate_turn_uuid(&uuid).map_err(Error::harness)?;
        let frame =
            native::new_user_turn(&uuid, &text, !self.expand_prompts).map_err(Error::harness)?;
        let (signal, corroborated) = oneshot::channel();
        {
            let _operation = self.operation.lock().unwrap();
            let mut state = self.state.lock().unwrap();
            state.require_idle(&request.session_id)?;
            state.pending = Some(PendingTurn {
                uuid: uuid.clone(),
                model: state.session.current_model_id.clone(),
                corroborated: Some(signal),
                buffered: Vec::new(),
                policy: RunToolPolicy {
                    choice,
                    ..Default::default()
                },
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

    async fn cancel_turn(&self, run_id: RunId) -> CancelResult {
        let (waiter, receipt) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            if state.session.status == SessionStatus::CLOSED {
                return Err(Error::SessionClosed);
            }
            let mut response = RunCancelResponse {
                session_id: state.session.session_id.clone(),
                run_id,
                accepted: true,
                status: RunStatus::CANCELLING,
            };
            if let Some(status) = state.settled_runs.get(&response.run_id) {
                if *status != RunStatus::CANCELLED {
                    return Err(Error::RunAlreadyTerminal {
                        run_id: Some(response.run_id),
                        status: Some(status.clone()),
                    });
                }
                response.status = status.clone();
                return Ok(response);
            }
            let turn = state
                .active
                .as_ref()
                .filter(|turn| turn.run_id == response.run_id)
                .ok_or(Error::RunNotFound)?;
            if turn.cancel_accepted {
                return Ok(response);
            }
            if let Some(interrupt) = &mut state.interrupt {
                interrupt.waiters.push(waiter);
            } else {
                let request = native::interrupt_request().map_err(Error::harness)?;
                state.interrupt = Some(Interrupt {
                    waiters: vec![waiter],
                    terminal: None,
                });
                let client = self.process.client.clone();
                let session_state = self.state.clone();
                let runtime = self.runtime.clone();
                if let Err(error) = std::thread::Builder::new().spawn(move || {
                    let outcome = (|| {
                        let deadline = Some(Instant::now() + RPC_TIMEOUT);
                        let body = client
                            .call(request.as_bytes(), deadline)
                            .map_err(Error::harness)?;
                        if let Some(body) = body {
                            native::decode_interrupt_result(&body).map_err(Error::harness)?;
                        }
                        Ok(response)
                    })();
                    session_state
                        .lock()
                        .unwrap()
                        .finish_interrupt(outcome, &runtime);
                }) {
                    state.interrupt = None;
                    return Err(Error::harness(error));
                }
            }
        }
        receipt.await.map_err(|_| Error::SessionClosed)?
    }
}

fn submit_text(request: &MessageSubmitRequest) -> Result<String, Error> {
    refuse_unadvertised_controls(request, &[FEATURE_TOOL_SELECTION])?;
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
            if state.unavailable() {
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
                let _operation = self.operation.lock().unwrap();
                let mut state = self.state.lock().unwrap();
                if state.pending.is_some() || state.active.is_some() {
                    return Err(Error::RunActive);
                }
                state.session.status = SessionStatus::CLOSED;
                state.journal.close();
            }
            self.process.close(None).map_err(Error::harness)
        })
    }

    fn submit(&self, submit: SubmitRequest) -> BoxFuture<'_, (MessageSubmitResponse, EventStream)> {
        Box::pin(self.submit_turn(submit.request))
    }

    fn resolve(&self, resolution: InteractionResolution) -> BoxFuture<'_, ()> {
        Box::pin(async {
            let mut state = self.state.lock().unwrap();
            state.resolve_gate(resolution, &self.runtime)
        })
    }

    fn cancel(&self, run_id: RunId) -> BoxFuture<'_, RunCancelResponse> {
        Box::pin(self.cancel_turn(run_id))
    }

    fn resume(&self, request: ResumeRequest) -> BoxFuture<'_, (Recovery, EventStream)> {
        Box::pin(async move {
            let state = self.state.lock().unwrap();
            if state.session.status == SessionStatus::CLOSED {
                return Err(Error::SessionClosed);
            }
            let run = request.run_id.as_str();
            let latest = state.journal.latest(run).ok_or(Error::RunNotFound)?;
            if request.after_sequence > latest {
                return Err(Error::ReplayCursorFuture);
            }
            let (recovery, stream) =
                state
                    .journal
                    .resume(state.session.clone(), run, request.after_sequence, latest);
            if let Some(gap) = recovery.replay_gap {
                return Err(Error::ReplayGap(gap));
            }
            Ok((recovery, stream))
        })
    }

    fn call_resolver(&self) -> Option<&dyn CallResolver> {
        (!self.state.lock().unwrap().catalog.provided.is_empty()).then_some(self)
    }

    fn settings_updater(&self) -> Option<&dyn SettingsUpdater> {
        Some(self)
    }

    fn tool_lister(&self) -> Option<&dyn ToolLister> {
        Some(self)
    }
}

impl CallResolver for OpenedSession {
    fn resolve_call(&self, resolution: CallResolution) -> BoxFuture<'_, ActionCallResolveResponse> {
        Box::pin(async move {
            let state = &mut *self.state.lock().unwrap();
            let (runtime, request) = (&self.runtime, resolution.request);
            let mut answer = ActionCallResolveResponse {
                session_id: request.session_id.clone(),
                run_id: request.run_id.clone(),
                tool_call_id: request.tool_call_id.clone(),
                interaction_id: request.interaction_id.clone(),
                reason: ResolveReason::UNKNOWN_INTERACTION,
                ..Default::default()
            };
            let active = state.active.iter_mut().flat_map(|turn| &mut turn.tools);
            let tool = active.chain(&mut state.settled_calls).find(|tool| {
                let payload = &tool.payload;
                payload.interaction_id.as_ref() == Some(&request.interaction_id)
                    && payload.run_id == request.run_id
                    && payload.session_id == request.session_id
            });
            let Some(tool) = tool else { return Ok(answer) };
            let ToolProgress::Provided(Some(call)) = &mut tool.progress else {
                return Ok(answer);
            };
            let payload = &tool.payload;
            let arm = request.arm();
            let acknowledges = arm == Some(RESOLVE_ARM_ACKNOWLEDGE);
            answer.reason = if payload.requested_by.as_ref() != Some(&request.requested_by)
                || payload.responded_by.as_ref() != Some(&request.responded_by)
                || payload.tool_call_id != request.tool_call_id
            {
                ResolveReason::WRONG_RESPONDER
            } else if arm.is_none() {
                return Ok(answer);
            } else if let Some(id) = &call.settlement {
                let settlement_id = Some(id.clone());
                answer.details = Some(ActionCallResolveDetails { settlement_id });
                ResolveReason::ALREADY_RESOLVED
            } else if acknowledges && call.started.is_some() {
                ResolveReason::REPEATED_ACKNOWLEDGEMENT
            } else {
                ResolveReason::default()
            };
            answer.accepted = answer.reason.is_empty();
            if !answer.accepted {
                return Ok(answer);
            }
            let mut payload = payload.clone();
            payload.request_id = Some(resolution.request_id);
            let started = call.started.is_none().then(|| {
                let kind = EnvelopeType::ACTION_CALL_STARTED;
                let event = tool_event(kind, &payload, Some(call.requested.clone()), runtime);
                call.started = Some(event.id.clone());
                event
            });
            let settled = (!acknowledges).then(|| {
                payload.error = request.error;
                payload.result = request.result;
                let (kind, text) = if let Some(error) = &payload.error {
                    (EnvelopeType::ACTION_CALL_FAILED, error.message.clone())
                } else {
                    let raw = payload.result.as_ref().unwrap().get();
                    let text = serde_json::from_str(raw).unwrap_or_else(|_| raw.into());
                    (EnvelopeType::ACTION_CALL_COMPLETED, text)
                };
                let content = serde_json::json!([{"type": "text", "text": text}]);
                let is_error = kind == EnvelopeType::ACTION_CALL_FAILED;
                let result = serde_json::json!({"content": content, "isError": is_error});
                if let Some((control, id)) = call.held.take() {
                    send_mcp(&control, &id, "result", &result.to_string());
                } else {
                    call.reply = Some(result.to_string());
                }
                let event = tool_event(kind, &payload, call.started.clone(), runtime);
                call.settlement = Some(event.id.clone());
                event
            });
            if let Some(event) = started {
                state.append_event(event, false, runtime);
            }
            if let Some(event) = settled {
                state.append_event(event, false, runtime);
                state.interaction_status(runtime);
            }
            Ok(answer)
        })
    }
}

impl SettingsUpdater for OpenedSession {
    fn update_settings(
        &self,
        request: SessionSettingsUpdateRequest,
    ) -> BoxFuture<'_, (SessionSettingsUpdateResponse, SessionState)> {
        Box::pin(async move {
            let descriptor = CapabilityDescriptor {
                features: Adapter::features(),
                ..Default::default()
            };
            refuse_unadvertised_live_settings(&request, &descriptor)?;
            let flags = native::settings_flags(
                request.reasoning_level.clone(),
                request.compaction_policy.as_ref(),
                true,
            )?;
            let _operation = self.operation.lock().unwrap();
            let session_id = &request.session_id;
            self.state.lock().unwrap().require_idle(session_id)?;
            let body = native::apply_flag_settings_request(&flags).map_err(Error::harness)?;
            self.exchange("apply_flag_settings", body)?;
            let mut data = self.state.lock().unwrap();
            let state = &mut data.session;
            let mut response = SessionSettingsUpdateResponse {
                session_id: state.session_id.clone(),
                ..Default::default()
            };
            if !request.reasoning_level.is_empty() {
                response.previous_reasoning_level =
                    std::mem::replace(&mut state.reasoning_level, request.reasoning_level);
                response.reasoning_level = state.reasoning_level.clone();
            }
            if let Some(policy) = request.compaction_policy {
                response.previous_compaction_policy = state.compaction_policy.replace(policy);
                response.compaction_policy = state.compaction_policy.clone();
            }
            state.updated_at_ms = self.runtime.now_ms();
            Ok((response, state.clone()))
        })
    }
}

impl ToolLister for OpenedSession {
    fn tools(&self, request: ToolsListRequest) -> BoxFuture<'_, ToolCatalog> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            if state.unavailable() {
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
