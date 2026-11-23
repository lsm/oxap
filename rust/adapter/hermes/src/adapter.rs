use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use crossbeam_channel::{never, unbounded, Sender};
use futures_channel::{mpsc, oneshot};
use oxap_adapter::{
    journal::Journal, refuse_unadvertised_controls, refuse_unadvertised_live_settings,
    refuse_unadvertised_settings, refuse_unadvertised_tool_sources, refuse_unadvertised_tools,
    runtime::Runtime, Adapter as AdapterContract, BoxFuture, Descriptor, Error, EventStream,
    InteractionResolution, JournalDescriptor, NativeListRequest, NativeLister, NativeListing,
    OpenRequest, Recovery, ResumeRequest, Session, SettingsUpdater, SubmitRequest,
};
use oxap_protocol::{
    ActiveRun, CapabilityDescriptor, ContentPartType, EndpointDescriptor, Envelope, EnvelopeType,
    FeatureSupport, MessageRole, MessageSubmitRequest, MessageSubmitResponse, ParticipantId,
    RequestedDeliveryMode, RunCancelResponse, RunId, RunStatus, SessionId,
    SessionSettingsUpdateRequest, SessionSettingsUpdateResponse, SessionState, SessionStatus,
    SupportLevel, FEATURE_OPEN_REOPEN, FEATURE_SESSION_REASONING, MODE_SESSION_LIVE,
    MODE_SESSION_OPEN, PROFILE, VERSION,
};

use crate::{
    gateway_session::{interrupt, set_reasoning, InputCommand, ObservationError, PromptStartError},
    native::METHOD_SESSION_INTERRUPT,
    process::{Process, ProcessConfig},
    GatewayRun, GatewaySession, Observation, RunObservation,
};

const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_JOURNAL_CAPACITY: usize = 256;

pub struct Adapter {
    process: ProcessConfig,
    model: String,
    runtime: Runtime,
    journal_capacity: usize,
}

impl Adapter {
    pub fn new(process: ProcessConfig, model: String) -> Self {
        Self {
            process,
            model,
            runtime: Runtime::default(),
            journal_capacity: DEFAULT_JOURNAL_CAPACITY,
        }
    }

    pub fn with_journal_capacity(mut self, capacity: usize) -> Self {
        self.journal_capacity = if capacity == 0 {
            DEFAULT_JOURNAL_CAPACITY
        } else {
            capacity
        };
        self
    }

    fn descriptor(&self) -> Descriptor {
        let pin = oxap_harness::current("hermes").expect("embedded catalog pins Hermes");
        let features = [
            ("session.open", SupportLevel::NATIVE),
            (FEATURE_OPEN_REOPEN, SupportLevel::NATIVE),
            ("session.state", SupportLevel::EMULATED),
            ("session.message.submit", SupportLevel::DEGRADED),
            ("session.message.delivery.auto", SupportLevel::DEGRADED),
            ("run.streaming", SupportLevel::NATIVE),
            ("run.cancel", SupportLevel::DEGRADED),
            ("run.status", SupportLevel::EMULATED),
            ("run.resume", SupportLevel::DEGRADED),
            ("run.replay", SupportLevel::DEGRADED),
            ("user_input", SupportLevel::NATIVE),
            ("action.tools", SupportLevel::DEGRADED),
        ]
        .into_iter()
        .map(|(feature, level)| {
            (
                feature.into(),
                FeatureSupport {
                    level,
                    ..FeatureSupport::default()
                },
            )
        })
        .chain([(FEATURE_SESSION_REASONING.into(), reasoning_support())])
        .collect();
        Descriptor {
            capabilities: CapabilityDescriptor {
                endpoint: EndpointDescriptor {
                    id: "hermes.gateway".into(),
                    name: "Hermes Gateway Adapter".into(),
                    version: pin.endpoint_version.clone(),
                    adapter: "hermes-tui-gateway".into(),
                },
                protocol_versions: vec![VERSION.into()],
                profiles: vec![PROFILE.into()],
                features,
                ..CapabilityDescriptor::default()
            },
            capability_revision: pin.capability_revision.clone(),
            journal: JournalDescriptor {
                scope: "session".into(),
                persistence: "process_memory".into(),
                replay: SupportLevel::DEGRADED,
                capacity: self.journal_capacity,
            },
            max_active_runs_per_session: 1,
            interactive_gates: true,
            cancellation_target: "session".into(),
            cancellation_implementation: METHOD_SESSION_INTERRUPT.into(),
        }
    }
}

fn reasoning_support() -> FeatureSupport {
    FeatureSupport {
        level: SupportLevel::NATIVE,
        modes: vec![MODE_SESSION_OPEN.into(), MODE_SESSION_LIVE.into()],
        ..FeatureSupport::default()
    }
}

fn background<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> BoxFuture<'static, T> {
    Box::pin(async move {
        let (reply, result) = oneshot::channel();
        std::thread::Builder::new()
            .spawn(move || {
                let _ = reply.send(work());
            })
            .map_err(Error::harness)?;
        result.await.unwrap_or(Err(Error::SessionClosed))
    })
}

impl AdapterContract for Adapter {
    fn probe(&self) -> BoxFuture<'_, Descriptor> {
        Box::pin(async { Ok(self.descriptor()) })
    }

    fn open(&self, request: OpenRequest) -> BoxFuture<'_, Box<dyn Session>> {
        Box::pin(async move {
            refuse_unadvertised_tool_sources(&request, &[])?;
            refuse_unadvertised_tools(&request, &[])?;
            refuse_unadvertised_settings(&request, &self.descriptor().capabilities)?;

            let deadline = Some(Instant::now() + RPC_TIMEOUT);
            let session_id = if request.session_id.as_str().is_empty() {
                self.runtime.new_id("session").into()
            } else {
                request.session_id
            };
            let (gateway, mut state) = if request.reopen {
                GatewaySession::reopen(
                    &self.process,
                    &request.native_session_id,
                    session_id,
                    &self.model,
                    deadline,
                )?
            } else {
                let gateway = GatewaySession::create(
                    &self.process,
                    &self.model,
                    &request.reasoning_level,
                    deadline,
                )?;
                (
                    gateway,
                    SessionState {
                        session_id,
                        status: SessionStatus::IDLE,
                        current_model_id: self.model.clone(),
                        ..SessionState::default()
                    },
                )
            };
            if request.reopen && !request.reasoning_level.is_empty() {
                set_reasoning(
                    gateway.client(),
                    gateway.runtime_id(),
                    &request.reasoning_level,
                    deadline,
                )?;
            }
            state.reasoning_level = request.reasoning_level;
            state.updated_at_ms = self.runtime.now_ms();
            Ok(Box::new(OpenedSession {
                stored_id: gateway.stored_id().to_owned(),
                runtime_id: gateway.runtime_id().to_owned(),
                responder: if request.participant.id.as_str().is_empty() {
                    "user".into()
                } else {
                    request.participant.id
                },
                process: gateway.process.clone(),
                control: Arc::new(Mutex::new(())),
                slot: Arc::new(Mutex::new(SessionSlot {
                    gateway: Some(gateway),
                    state,
                    cancel_accepted: false,
                    input: None,
                    journal: Journal::new(self.journal_capacity),
                })),
                runtime: self.runtime.clone(),
            }) as Box<dyn Session>)
        })
    }

    fn native_lister(&self) -> Option<&dyn NativeLister> {
        Some(self)
    }
}

impl NativeLister for Adapter {
    fn native_list(&self, request: NativeListRequest) -> BoxFuture<'_, Vec<NativeListing>> {
        let process = self.process.clone();
        background(move || {
            crate::native_list::list(&process, request.limit, Instant::now() + RPC_TIMEOUT)
        })
    }
}

struct OpenedSession {
    process: Arc<Process>,
    control: Arc<Mutex<()>>,
    slot: Arc<Mutex<SessionSlot>>,
    stored_id: String,
    runtime_id: String,
    responder: ParticipantId,
    runtime: Runtime,
}

struct SessionSlot {
    gateway: Option<GatewaySession>,
    state: SessionState,
    cancel_accepted: bool,
    input: Option<Sender<InputCommand>>,
    journal: Journal,
}

impl SessionSlot {
    fn require_idle(&self, session_id: &SessionId) -> Result<(), Error> {
        if self.state.status == SessionStatus::CLOSED || self.state.status == SessionStatus::ERROR {
            Err(Error::SessionClosed)
        } else if *session_id != self.state.session_id {
            Err(Error::RunNotFound)
        } else if self.state.status == SessionStatus::IDLE {
            Ok(())
        } else {
            Err(Error::RunActive)
        }
    }

    fn settle(&mut self, mut gateway: Option<GatewaySession>, runtime: &Runtime) {
        if self.state.status == SessionStatus::CLOSED {
            return;
        }
        self.state.active_run_id = None;
        self.state.active_runs.clear();
        self.cancel_accepted = false;
        self.input = None;
        if let Some(gateway) = &mut gateway {
            gateway.input = never();
        }
        self.state.updated_at_ms = runtime.now_ms();
        self.state.status = if gateway.is_some() {
            SessionStatus::IDLE
        } else {
            SessionStatus::ERROR
        };
        self.gateway = gateway;
    }

    fn cancel_receipt(&self, run_id: &RunId) -> Result<RunCancelResponse, Error> {
        if self.state.status == SessionStatus::CLOSED {
            return Err(Error::SessionClosed);
        }
        if self.state.active_run_id.as_ref() != Some(run_id) {
            return Err(Error::RunNotFound);
        }
        Ok(RunCancelResponse {
            session_id: self.state.session_id.clone(),
            run_id: run_id.clone(),
            accepted: true,
            status: RunStatus::CANCELLING,
        })
    }

    fn project_input(&mut self, event: &Envelope) {
        if self.state.status == SessionStatus::CLOSED {
            return;
        }
        let (Some(turn), Some(active)) = (&event.turn_id, self.state.active_runs.first_mut())
        else {
            return;
        };
        if event.envelope_type == EnvelopeType::USER_INPUT_REQUESTED {
            active.pending_interactions.push(turn.clone());
        } else if event.envelope_type == EnvelopeType::USER_INPUT_RESOLVED {
            active
                .pending_interactions
                .retain(|pending| pending != turn);
        } else {
            return;
        }
        (self.state.status, active.status) = if active.pending_interactions.is_empty() {
            (SessionStatus::RUNNING, RunStatus::RUNNING)
        } else {
            (
                SessionStatus::WAITING_FOR_INPUT,
                RunStatus::WAITING_FOR_INPUT,
            )
        };
        active.as_of_sequence = event.sequence;
        self.state.updated_at_ms = event.timestamp_ms.unwrap_or(self.state.updated_at_ms);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

impl OpenedSession {
    fn open_slot(&self) -> Result<MutexGuard<'_, SessionSlot>, Error> {
        let slot = lock(&self.slot);
        if slot.state.status == SessionStatus::CLOSED {
            return Err(Error::SessionClosed);
        }
        Ok(slot)
    }

    fn shutdown(&self) -> Result<(), Error> {
        lock(&self.slot).state.status = SessionStatus::CLOSED;
        self.process.close().map_err(Error::harness)
    }
}

impl Drop for OpenedSession {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl Session for OpenedSession {
    fn state(&self) -> BoxFuture<'_, SessionState> {
        Box::pin(async { Ok(self.open_slot()?.state.clone()) })
    }

    fn native_session_id(&self) -> Option<String> {
        Some(self.stored_id.clone())
    }

    fn settings_updater(&self) -> Option<&dyn SettingsUpdater> {
        Some(self)
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async { self.shutdown() })
    }

    fn submit(&self, submit: SubmitRequest) -> BoxFuture<'_, (MessageSubmitResponse, EventStream)> {
        Box::pin(async move {
            let text = submit_text(&submit.request)?;
            self.open_slot()?.require_idle(&submit.request.session_id)?;
            let slot = self.slot.clone();
            let control = self.control.clone();
            let runtime = self.runtime.clone();
            let responder = self.responder.clone();
            let (reply, admission) = oneshot::channel();
            std::thread::spawn(move || {
                let ordering = lock(&control);
                let (mut gateway, mut state) = {
                    let mut slot = lock(&slot);
                    if let Err(error) = slot.require_idle(&submit.request.session_id) {
                        let _ = reply.send(Err(error));
                        return;
                    }
                    slot.state.status = SessionStatus::QUEUED;
                    slot.state.updated_at_ms = runtime.now_ms();
                    (
                        slot.gateway
                            .take()
                            .expect("idle Hermes session has a gateway"),
                        slot.state.clone(),
                    )
                };
                let (input, commands) = unbounded();
                gateway.input = commands;
                let started = gateway.start_run_classified(
                    &text,
                    state.session_id.clone(),
                    &state.current_model_id,
                    runtime.clone(),
                    Some(Instant::now() + RPC_TIMEOUT),
                );
                drop(ordering);
                let mut run = match started {
                    Ok(run) => run,
                    Err(error) => {
                        let reusable = matches!(error, PromptStartError::Refused(_))
                            && gateway.client().err().is_none();
                        lock(&slot).settle(reusable.then_some(gateway), &runtime);
                        let _ = reply.send(Err(error.into()));
                        return;
                    }
                };
                run.responder = responder;
                let response = run.admission(&state.current_model_id);
                state.status = SessionStatus::RUNNING;
                state.active_run_id = response.run_id.clone();
                state.active_runs = vec![ActiveRun {
                    run_id: response.run_id.clone().expect("admitted run has an ID"),
                    status: RunStatus::RUNNING,
                    relationship: "primary".into(),
                    admitted_submit_requests: (!submit.envelope_id.as_str().is_empty())
                        .then_some(submit.envelope_id)
                        .into_iter()
                        .collect(),
                    ..ActiveRun::default()
                }];
                state.updated_at_ms = runtime.now_ms();
                let started = run.take_started();
                {
                    let mut slot = lock(&slot);
                    if slot.state.status == SessionStatus::CLOSED {
                        return;
                    }
                    slot.state = state;
                    slot.input = Some(input);
                    slot.journal.append(started.clone(), false);
                }
                let (events, stream) = mpsc::unbounded();
                let stream: EventStream = Box::pin(stream);
                let _ = events.unbounded_send(Ok(started));
                let _ = reply.send(Ok((response, stream)));
                let terminal = forward_run(&events, &mut run, &slot).ok().flatten();
                {
                    let mut slot = lock(&slot);
                    if let Some(event) = &terminal {
                        slot.journal.append(event.clone(), true);
                    } else {
                        slot.journal.close();
                    }
                    let reusable = terminal.is_some() && gateway.observed_settlement;
                    slot.settle(reusable.then_some(gateway), &runtime);
                }
                if let Some(event) = terminal {
                    let _ = events.unbounded_send(Ok(event));
                }
            });
            admission.await.unwrap_or(Err(Error::SessionClosed))
        })
    }

    fn resolve(&self, resolution: InteractionResolution) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.open_slot()?
                .input
                .as_ref()
                .ok_or(Error::InteractionNotFound)?
                .send(InputCommand { resolution, reply })
                .map_err(|_| Error::InteractionNotFound)?;
            match result.await {
                Ok(result) => result,
                Err(_) => {
                    drop(self.open_slot()?);
                    Err(Error::InteractionNotFound)
                }
            }
        })
    }

    fn cancel(&self, run_id: RunId) -> BoxFuture<'_, RunCancelResponse> {
        Box::pin(async move {
            {
                let slot = self.open_slot()?;
                let receipt = slot.cancel_receipt(&run_id)?;
                if slot.cancel_accepted {
                    return Ok(receipt);
                }
            }
            let slot = self.slot.clone();
            let control = self.control.clone();
            let process = self.process.clone();
            let runtime_id = self.runtime_id.clone();
            let (reply, result) = oneshot::channel();
            std::thread::Builder::new()
                .spawn(move || {
                    let _control = lock(&control);
                    let outcome = (|| {
                        let receipt = {
                            let slot = lock(&slot);
                            let receipt = slot.cancel_receipt(&run_id)?;
                            if slot.cancel_accepted {
                                return Ok(receipt);
                            }
                            receipt
                        };
                        interrupt(
                            &process.client,
                            &runtime_id,
                            Some(Instant::now() + RPC_TIMEOUT),
                        )?;
                        let mut slot = lock(&slot);
                        if slot.state.active_run_id.as_ref() == Some(&run_id) {
                            slot.cancel_accepted = true;
                        }
                        Ok(receipt)
                    })();
                    let _ = reply.send(outcome);
                })
                .map_err(Error::harness)?;
            result.await.unwrap_or(Err(Error::SessionClosed))
        })
    }

    fn resume(&self, request: ResumeRequest) -> BoxFuture<'_, (Recovery, EventStream)> {
        Box::pin(async move {
            let slot = self.open_slot()?;
            let run = request.run_id.as_str();
            let latest = slot.journal.latest(run).ok_or(Error::RunNotFound)?;
            if request.after_sequence > latest {
                return Err(Error::ReplayCursorFuture);
            }
            let (recovery, stream) =
                slot.journal
                    .resume(slot.state.clone(), run, request.after_sequence, latest);
            if let Some(gap) = recovery.replay_gap {
                return Err(Error::ReplayGap(gap));
            }
            Ok((recovery, stream))
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
                features: [(FEATURE_SESSION_REASONING.into(), reasoning_support())].into(),
                ..CapabilityDescriptor::default()
            };
            refuse_unadvertised_live_settings(&request, &descriptor)?;
            self.open_slot()?.require_idle(&request.session_id)?;
            let (slot, control, process) = (
                self.slot.clone(),
                self.control.clone(),
                self.process.clone(),
            );
            let (runtime_id, runtime) = (self.runtime_id.clone(), self.runtime.clone());
            background(move || {
                let _control = lock(&control);
                lock(&slot).require_idle(&request.session_id)?;
                let mut response = SessionSettingsUpdateResponse {
                    session_id: request.session_id,
                    ..SessionSettingsUpdateResponse::default()
                };
                if !request.reasoning_level.is_empty() {
                    set_reasoning(
                        &process.client,
                        &runtime_id,
                        &request.reasoning_level,
                        Some(Instant::now() + RPC_TIMEOUT),
                    )?;
                }
                let mut slot = lock(&slot);
                slot.require_idle(&response.session_id)?;
                if !request.reasoning_level.is_empty() {
                    response.reasoning_level = request.reasoning_level.clone();
                    response.previous_reasoning_level =
                        std::mem::replace(&mut slot.state.reasoning_level, request.reasoning_level);
                }
                slot.state.updated_at_ms = runtime.now_ms();
                Ok((response, slot.state.clone()))
            })
            .await
        })
    }
}

fn forward_run(
    events: &mpsc::UnboundedSender<Result<Envelope, Error>>,
    run: &mut GatewayRun<'_>,
    slot: &Mutex<SessionSlot>,
) -> Result<Option<Envelope>, Error> {
    let mut resolved: Option<oneshot::Sender<Result<(), Error>>> = None;
    loop {
        match run.observe(None) {
            Ok(Some(RunObservation::Event(event))) => {
                if event.envelope_type == EnvelopeType::RUN_COMPLETED
                    || event.envelope_type == EnvelopeType::RUN_FAILED
                {
                    return Ok(Some(*event));
                }
                {
                    let mut slot = lock(slot);
                    slot.project_input(&event);
                    slot.journal.append((*event).clone(), false);
                }
                let _ = events.unbounded_send(Ok(*event));
                if let Some(reply) = resolved.take() {
                    let _ = reply.send(Ok(()));
                }
            }
            Ok(Some(RunObservation::Other(Observation::Event(_)))) => {}
            Ok(Some(RunObservation::Other(Observation::Request(_)))) => {
                run.queue_failure(
                    "hermes_unsupported_interaction",
                    "unexpected gateway request",
                    "",
                )?;
            }
            Ok(None) => return Ok(None),
            Err(ObservationError::Input(command)) => {
                let result = if lock(slot).state.status == SessionStatus::CLOSED {
                    Err(Error::SessionClosed)
                } else {
                    run.resolve_shared(command.resolution)
                };
                if result.is_ok() {
                    resolved = Some(command.reply);
                } else {
                    let _ = command.reply.send(result);
                }
            }
            Err(error) => {
                run.queue_failure("hermes_invalid_event", &Error::from(error).to_string(), "")?
            }
        }
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
