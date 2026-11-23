use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use futures_channel::oneshot;
use oxap_adapter::{
    refuse_unadvertised_controls, refuse_unadvertised_settings, refuse_unadvertised_tool_sources,
    refuse_unadvertised_tools, runtime::Runtime, subscriber::Subscriber,
    Adapter as AdapterContract, BoxFuture, Descriptor, Error, EventStream, InteractionResolution,
    OpenRequest, Recovery, ResumeRequest, Session, SubmitRequest,
};
use oxap_protocol::{
    ActiveRun, CapabilityDescriptor, ContentPartType, EndpointDescriptor, Envelope, EnvelopeType,
    FeatureSupport, MessageRole, MessageSubmitRequest, MessageSubmitResponse,
    RequestedDeliveryMode, RunCancelResponse, RunId, RunStatus, SessionState, SessionStatus,
    SupportLevel, FEATURE_OPEN_REOPEN, PROFILE, VERSION,
};

use crate::{
    gateway_session::PromptStartError,
    process::{Process, ProcessConfig},
    GatewayRun, GatewaySession, Observation, RunObservation,
};

const STREAM_CAPACITY: usize = 256;

pub struct Adapter {
    process: ProcessConfig,
    model: String,
    runtime: Runtime,
}

impl Adapter {
    pub fn new(process: ProcessConfig, model: String) -> Self {
        Self {
            process,
            model,
            runtime: Runtime::default(),
        }
    }

    fn descriptor() -> Descriptor {
        let pin = oxap_harness::current("hermes").expect("embedded catalog pins Hermes");
        let features = [
            ("session.open", SupportLevel::NATIVE),
            (FEATURE_OPEN_REOPEN, SupportLevel::NATIVE),
            ("session.state", SupportLevel::EMULATED),
            ("session.message.submit", SupportLevel::DEGRADED),
            ("session.message.delivery.auto", SupportLevel::DEGRADED),
            ("run.streaming", SupportLevel::NATIVE),
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
            max_active_runs_per_session: 1,
            ..Descriptor::default()
        }
    }
}

impl AdapterContract for Adapter {
    fn probe(&self) -> BoxFuture<'_, Descriptor> {
        Box::pin(async { Ok(Self::descriptor()) })
    }

    fn open(&self, request: OpenRequest) -> BoxFuture<'_, Box<dyn Session>> {
        Box::pin(async move {
            refuse_unadvertised_tool_sources(&request, &[])?;
            refuse_unadvertised_tools(&request, &[])?;
            refuse_unadvertised_settings(&request, &Self::descriptor().capabilities)?;

            let deadline = Some(Instant::now() + Duration::from_secs(30));
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
            state.updated_at_ms = self.runtime.now_ms();
            Ok(Box::new(OpenedSession {
                stored_id: gateway.stored_id().to_owned(),
                process: gateway.process.clone(),
                slot: Arc::new(Mutex::new(SessionSlot {
                    gateway: Some(gateway),
                    state,
                })),
                runtime: self.runtime.clone(),
            }) as Box<dyn Session>)
        })
    }
}

struct OpenedSession {
    process: Arc<Process>,
    slot: Arc<Mutex<SessionSlot>>,
    stored_id: String,
    runtime: Runtime,
}

struct SessionSlot {
    gateway: Option<GatewaySession>,
    state: SessionState,
}

impl SessionSlot {
    fn settle(&mut self, gateway: Option<GatewaySession>, runtime: &Runtime) {
        if self.state.status == SessionStatus::CLOSED {
            return;
        }
        self.state.active_run_id = None;
        self.state.active_runs.clear();
        self.state.updated_at_ms = runtime.now_ms();
        self.state.status = if gateway.is_some() {
            SessionStatus::IDLE
        } else {
            SessionStatus::ERROR
        };
        self.gateway = gateway;
    }
}

fn lock_slot(slot: &Mutex<SessionSlot>) -> MutexGuard<'_, SessionSlot> {
    slot.lock().unwrap_or_else(|poison| poison.into_inner())
}

impl OpenedSession {
    fn open_slot(&self) -> Result<MutexGuard<'_, SessionSlot>, Error> {
        let slot = lock_slot(&self.slot);
        if slot.state.status == SessionStatus::CLOSED {
            return Err(Error::SessionClosed);
        }
        Ok(slot)
    }

    fn while_open<'a, T: Send + 'a>(&'a self, result: Result<T, Error>) -> BoxFuture<'a, T> {
        Box::pin(async move {
            drop(self.open_slot()?);
            result
        })
    }

    fn shutdown(&self) -> Result<(), Error> {
        lock_slot(&self.slot).state.status = SessionStatus::CLOSED;
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

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async { self.shutdown() })
    }

    fn submit(&self, submit: SubmitRequest) -> BoxFuture<'_, (MessageSubmitResponse, EventStream)> {
        Box::pin(async move {
            let text = submit_text(&submit.request)?;
            let (mut gateway, mut state) = {
                let mut slot = self.open_slot()?;
                if submit.request.session_id != slot.state.session_id {
                    return Err(Error::RunNotFound);
                }
                let gateway = slot.gateway.take().ok_or_else(|| {
                    if slot.state.status == SessionStatus::ERROR {
                        Error::SessionClosed
                    } else {
                        Error::RunActive
                    }
                })?;
                slot.state.status = SessionStatus::QUEUED;
                slot.state.updated_at_ms = self.runtime.now_ms();
                (gateway, slot.state.clone())
            };
            let slot = self.slot.clone();
            let runtime = self.runtime.clone();
            let (reply, admission) = oneshot::channel();
            std::thread::spawn(move || {
                let deadline = Some(Instant::now() + Duration::from_secs(30));
                let mut run = match gateway.start_run_classified(
                    &text,
                    state.session_id.clone(),
                    &state.current_model_id,
                    runtime.clone(),
                    deadline,
                ) {
                    Ok(run) => run,
                    Err(error) => {
                        let reusable = matches!(error, PromptStartError::Refused(_))
                            && gateway.client().err().is_none();
                        lock_slot(&slot).settle(reusable.then_some(gateway), &runtime);
                        let _ = reply.send(Err(error.into()));
                        return;
                    }
                };
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
                {
                    let mut slot = lock_slot(&slot);
                    if slot.state.status == SessionStatus::CLOSED {
                        return;
                    }
                    slot.state = state;
                }
                let events = Subscriber::new(STREAM_CAPACITY);
                let _ = reply.send(Ok((response, events.stream())));
                let terminal = forward_run(&events, &mut run);
                let gateway = gateway.observed_settlement.then_some(gateway);
                lock_slot(&slot).settle(gateway, &runtime);
                if let Ok(Some(event)) = terminal {
                    events.offer(event, true);
                }
                events.close();
            });
            admission.await.unwrap_or(Err(Error::SessionClosed))
        })
    }

    fn resolve(&self, _: InteractionResolution) -> BoxFuture<'_, ()> {
        self.while_open(Err(Error::InteractionNotFound))
    }

    fn cancel(&self, _: RunId) -> BoxFuture<'_, RunCancelResponse> {
        self.while_open(Err(Error::RunNotFound))
    }

    fn resume(&self, _: ResumeRequest) -> BoxFuture<'_, (Recovery, EventStream)> {
        self.while_open(Err(Error::RunNotFound))
    }
}

fn forward_run(events: &Subscriber, run: &mut GatewayRun<'_>) -> Result<Option<Envelope>, Error> {
    loop {
        match run.observe(None, false) {
            Ok(Some(RunObservation::Event(event))) => {
                if event.envelope_type == EnvelopeType::RUN_COMPLETED
                    || event.envelope_type == EnvelopeType::RUN_FAILED
                {
                    return Ok(Some(*event));
                }
                events.offer(*event, false);
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
            Err(error) => run.queue_failure("hermes_invalid_event", &error.to_string(), "")?,
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
