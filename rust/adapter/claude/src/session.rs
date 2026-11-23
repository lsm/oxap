use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures_channel::oneshot;
use oxap_adapter::{
    refuse_unadvertised_controls, BoxFuture, Error, EventStream, InteractionResolution, Recovery,
    ResumeRequest, Session, SubmitRequest, ToolCatalog, ToolLister,
};
use oxap_protocol::{
    ContentPartType, MessageRole, MessageSubmitRequest, MessageSubmitResponse,
    RequestedDeliveryMode, RunCancelResponse, RunId, SessionState, SessionStatus, ToolsListRequest,
    FEATURE_TOOLS_LIST,
};
use serde_json::value::to_raw_value;

use crate::{
    adapter::{native_uuid, pin},
    catalog::Catalog,
    native::{self, Observation},
    rpc::{ClientError, Inbound, Process},
};

const RPC_TIMEOUT: Duration = Duration::from_secs(60);
type Admission = Result<(MessageSubmitResponse, EventStream), Error>;

pub(crate) struct OpenedSession {
    process: Process,
    state: Arc<Mutex<SessionData>>,
    expand_prompts: bool,
}

struct SessionData {
    session: SessionState,
    catalog: Catalog,
    pending: Option<PendingTurn>,
}

struct PendingTurn {
    uuid: String,
    admission: oneshot::Sender<Admission>,
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

impl SessionData {
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
    ) -> Result<Self, Error> {
        let state = Arc::new(Mutex::new(SessionData {
            session: state,
            catalog: Catalog::default(),
            pending: None,
        }));
        let session = Self {
            process,
            state,
            expand_prompts,
        };
        let inbound = session.process.client.inbound();
        let state = session.state.clone();
        std::thread::Builder::new()
            .spawn(move || {
                for frame in inbound {
                    match frame {
                        Inbound::Observation(observation) => {
                            if let Observation::Init(init) = observation.value {
                                let mut state = state.lock().unwrap();
                                state.catalog.observe(&init);
                                state.session.current_model_id = init.model;
                                if !init.session_id.is_empty() {
                                    bind(&mut state.session, &init.session_id);
                                }
                            }
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
                if let Some(turn) = state.pending.take() {
                    let _ = turn.admission.send(Err(Error::SessionClosed));
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
        let (reply, admission) = oneshot::channel();
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
                admission: reply,
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
        admission.await.unwrap_or(Err(Error::SessionClosed))
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
                if state.pending.is_some() {
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
