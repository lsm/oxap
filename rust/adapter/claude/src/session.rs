use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use oxap_adapter::{
    BoxFuture, Error, EventStream, InteractionResolution, Recovery, ResumeRequest, Session,
    SubmitRequest,
};
use oxap_protocol::{MessageSubmitResponse, RunCancelResponse, RunId, SessionState, SessionStatus};
use serde_json::value::to_raw_value;

use crate::{
    native::{self, Observation},
    rpc::{Inbound, Process},
};

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct OpenedSession {
    process: Process,
    state: Arc<Mutex<SessionState>>,
}

impl OpenedSession {
    pub(crate) fn new(process: Process, state: SessionState) -> Result<Self, Error> {
        let state = Arc::new(Mutex::new(state));
        let session = Self { process, state };
        let inbound = session.process.client.inbound();
        let state = session.state.clone();
        std::thread::Builder::new()
            .spawn(move || {
                for frame in inbound {
                    match frame {
                        Inbound::Observation(observation) => {
                            if let Observation::Init(init) = observation.value {
                                let mut state = state.lock().unwrap();
                                state.current_model_id = init.model;
                                if !init.session_id.is_empty() {
                                    bind(&mut state, &init.session_id);
                                }
                            }
                        }
                        Inbound::Control(control) => {
                            let deadline = Some(Instant::now() + INITIALIZE_TIMEOUT);
                            let _ = control.respond_error("claude adapter: no owned run", deadline);
                        }
                        _ => {}
                    }
                }
                state.lock().unwrap().status = SessionStatus::CLOSED;
            })
            .map_err(Error::harness)?;
        Ok(session)
    }

    pub(crate) fn exchange(&self, name: &str, body: String) -> Result<(), Error> {
        let deadline = Some(Instant::now() + INITIALIZE_TIMEOUT);
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
            if state.status == SessionStatus::CLOSED {
                return Err(Error::SessionClosed);
            }
            Ok(state.clone())
        })
    }

    fn native_session_id(&self) -> Option<String> {
        let state = self.state.lock().unwrap();
        serde_json::from_str(state.metadata.get("claude_native_session_id")?.get()).ok()
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            self.state.lock().unwrap().status = SessionStatus::CLOSED;
            self.process.close(None).map_err(Error::harness)
        })
    }

    fn submit(&self, _: SubmitRequest) -> BoxFuture<'_, (MessageSubmitResponse, EventStream)> {
        Box::pin(async { Err(Error::UnsupportedInput) })
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
}

impl Drop for OpenedSession {
    fn drop(&mut self) {
        let _ = self.process.close(None);
    }
}
