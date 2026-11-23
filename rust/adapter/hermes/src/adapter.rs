use std::{
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use oxap_adapter::{
    refuse_unadvertised_settings, refuse_unadvertised_tool_sources, refuse_unadvertised_tools,
    runtime::Runtime, Adapter as AdapterContract, BoxFuture, Descriptor, Error, EventStream,
    InteractionResolution, OpenRequest, Recovery, ResumeRequest, Session, SubmitRequest,
};
use oxap_protocol::{
    CapabilityDescriptor, EndpointDescriptor, FeatureSupport, MessageSubmitResponse,
    RunCancelResponse, RunId, SessionState, SessionStatus, SupportLevel, FEATURE_OPEN_REOPEN,
    PROFILE, VERSION,
};

use crate::{process::ProcessConfig, GatewaySession};

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
                gateway: Mutex::new(Some(gateway)),
                state,
            }) as Box<dyn Session>)
        })
    }
}

struct OpenedSession {
    gateway: Mutex<Option<GatewaySession>>,
    stored_id: String,
    state: SessionState,
}

impl OpenedSession {
    fn gateway(&self) -> MutexGuard<'_, Option<GatewaySession>> {
        self.gateway
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn while_open<'a, T: Send + 'a>(&'a self, result: Result<T, Error>) -> BoxFuture<'a, T> {
        Box::pin(async move {
            self.gateway().as_ref().ok_or(Error::SessionClosed)?;
            result
        })
    }
}

impl Session for OpenedSession {
    fn state(&self) -> BoxFuture<'_, SessionState> {
        self.while_open(Ok(self.state.clone()))
    }

    fn native_session_id(&self) -> Option<String> {
        Some(self.stored_id.clone())
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            self.gateway()
                .take()
                .map_or(Ok(()), |gateway| gateway.close())
        })
    }

    fn submit(&self, _: SubmitRequest) -> BoxFuture<'_, (MessageSubmitResponse, EventStream)> {
        self.while_open(Err(Error::UnsupportedInput))
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
