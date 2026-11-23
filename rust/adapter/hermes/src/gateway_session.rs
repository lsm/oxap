use std::{collections::VecDeque, sync::Arc, time::Instant};

use crossbeam_channel::{bounded, select_biased, Receiver, RecvTimeoutError, Sender};
use oxap_adapter::gojson::{go_quote, zero};
use oxap_adapter::{Error, UnsupportedControl, CONTROL_UNSATISFIABLE};
use oxap_protocol::{
    ReasoningLevel, RecoveryMetadata, SessionId, SessionState, SessionStatus, FEATURE_OPEN_REOPEN,
    FEATURE_SESSION_REASONING,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::native::{
    decode_strict, Event, InterruptResult, MessageCompletePayload, PromptSubmitResult,
    SessionInfoPayload, EVENT_MESSAGE_COMPLETE, EVENT_MESSAGE_START, METHOD_CONFIG_SET,
    METHOD_PROMPT_SUBMIT, METHOD_SESSION_CREATE, METHOD_SESSION_INTERRUPT, METHOD_SESSION_RESUME,
    SUBMIT_STREAMING,
};
use crate::process::{Process, ProcessConfig};
use crate::rpc::{decode_event, Fault, Inbound, IncomingRequest, RpcClient};

const RELAY_CAPACITY: usize = 256;
const SESSION_NOT_FOUND: i64 = 4007;

#[derive(Serialize)]
struct SessionCreateParams<'a> {
    #[serde(skip_serializing_if = "str::is_empty")]
    model: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    reasoning_effort: &'a str,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SessionCreateResult {
    #[serde(deserialize_with = "zero")]
    session_id: String,
    #[serde(deserialize_with = "zero")]
    stored_session_id: String,
    #[serde(rename = "message_count", deserialize_with = "zero")]
    _message_count: i64,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SessionResumeResult {
    #[serde(deserialize_with = "zero")]
    session_id: String,
    info: Option<Box<RawValue>>,
    auto_continue: Option<Box<RawValue>>,
    #[serde(deserialize_with = "zero")]
    running: bool,
    #[serde(deserialize_with = "zero")]
    status: String,
}

#[derive(Serialize)]
struct ConfigSetParams<'a> {
    session_id: &'a str,
    key: &'static str,
    value: &'a str,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ConfigSetResult {
    #[serde(rename = "key", deserialize_with = "zero")]
    _key: String,
    #[serde(rename = "value", deserialize_with = "zero")]
    _value: String,
}

#[derive(Serialize)]
struct PromptSubmitParams<'a> {
    session_id: &'a str,
    text: &'a str,
}

#[derive(Serialize)]
struct SessionParams<'a> {
    session_id: &'a str,
}

#[derive(Debug)]
pub enum Observation {
    Event(Event),
    Request(Arc<IncomingRequest>),
}

pub(crate) enum ObservationError {
    Transport(String),
    Other(Error),
}

impl From<Error> for ObservationError {
    fn from(error: Error) -> Self {
        Self::Other(error)
    }
}

impl From<ObservationError> for Error {
    fn from(error: ObservationError) -> Self {
        match error {
            ObservationError::Transport(reason) => Self::harness(reason),
            ObservationError::Other(error) => error,
        }
    }
}

#[derive(Debug)]
pub(crate) enum PromptStartError {
    Refused(Error),
    Uncertain(Error),
}

impl From<Error> for PromptStartError {
    fn from(error: Error) -> Self {
        Self::Uncertain(error)
    }
}

impl From<PromptStartError> for Error {
    fn from(error: PromptStartError) -> Self {
        match error {
            PromptStartError::Refused(error) | PromptStartError::Uncertain(error) => error,
        }
    }
}

pub struct GatewaySession {
    _stop_relay: Sender<()>,
    pub(crate) process: Arc<Process>,
    pub(crate) observed_settlement: bool,
    inbound: Receiver<Inbound>,
    buffered_observations: VecDeque<Observation>,
    runtime_id: String,
    stored_id: String,
    last_seq: i64,
    observation_error: Option<Error>,
}

impl GatewaySession {
    pub fn create(
        config: &ProcessConfig,
        model: &str,
        reasoning: &ReasoningLevel,
        deadline: Option<Instant>,
    ) -> Result<Self, Error> {
        let effort = if *reasoning == ReasoningLevel::OFF {
            "none"
        } else {
            reasoning.as_str()
        };
        let (session, ()) = Self::bind(config, deadline, |client| {
            let created: SessionCreateResult = call(
                client,
                METHOD_SESSION_CREATE,
                &SessionCreateParams {
                    model,
                    reasoning_effort: effort,
                },
                deadline,
            )?;
            if created.session_id.is_empty() || created.stored_session_id.is_empty() {
                return Err(Error::harness(
                    "hermes adapter: invalid native protocol observation",
                ));
            }
            if !effort.is_empty() {
                let _: ConfigSetResult = call(
                    client,
                    METHOD_CONFIG_SET,
                    &ConfigSetParams {
                        session_id: &created.session_id,
                        key: "reasoning",
                        value: effort,
                    },
                    deadline,
                )
                .map_err(|error| {
                    Error::UnsupportedControl(Box::new(UnsupportedControl {
                        feature: FEATURE_SESSION_REASONING.into(),
                        reason: CONTROL_UNSATISFIABLE,
                        field: "reasoning_level".into(),
                        detail: format!("Hermes refused the reasoning level: {error}"),
                        ..UnsupportedControl::default()
                    }))
                })?;
            }
            Ok((created.session_id, created.stored_session_id, ()))
        })?;
        Ok(session)
    }

    pub fn reopen(
        config: &ProcessConfig,
        stored_id: &str,
        session_id: SessionId,
        configured_model: &str,
        deadline: Option<Instant>,
    ) -> Result<(Self, SessionState), Error> {
        if stored_id.trim().is_empty() {
            return Err(reopen_refusal("the binding names no stored session"));
        }
        let (session, info) = Self::bind(config, deadline, |client| {
            let resumed = resume_stored(client, stored_id, deadline)?;
            Ok((resumed.session_id, stored_id.to_owned(), resumed.info))
        })
        .map_err(|error| {
            if deadline.is_some_and(|at| Instant::now() >= at) {
                Error::DeadlineExceeded
            } else {
                reopen_refusal(error)
            }
        })?;
        let current_model_id = info
            .and_then(|info| decode_strict::<SessionInfoPayload>(info.get().as_bytes()).ok())
            .map(|info| info.model)
            .filter(|model| !model.is_empty())
            .unwrap_or_else(|| configured_model.to_owned());
        let state = SessionState {
            session_id,
            status: SessionStatus::IDLE,
            current_model_id,
            recovery: Some(RecoveryMetadata {
                recovered: true,
                reason: "Hermes restored the bound conversation and reports the model it last ran under; OAP runs and cursors remain process-local".into(),
                ..RecoveryMetadata::default()
            }),
            ..SessionState::default()
        };
        Ok((session, state))
    }

    fn bind<T>(
        config: &ProcessConfig,
        deadline: Option<Instant>,
        load: impl FnOnce(&RpcClient) -> Result<(String, String, T), Error>,
    ) -> Result<(Self, T), Error> {
        let process = Process::start(config, deadline).map_err(Error::harness)?;
        let (stop_relay, stop) = bounded(0);
        let inbound = relay_inbound(&process.client, stop)?;
        let (runtime_id, stored_id, result) = load(&process.client)?;
        let session = Self {
            _stop_relay: stop_relay,
            process: Arc::new(process),
            observed_settlement: false,
            inbound,
            buffered_observations: VecDeque::new(),
            runtime_id,
            stored_id,
            last_seq: 0,
            observation_error: None,
        };
        Ok((session, result))
    }

    pub fn runtime_id(&self) -> &str {
        &self.runtime_id
    }

    pub fn stored_id(&self) -> &str {
        &self.stored_id
    }

    pub fn client(&self) -> &RpcClient {
        &self.process.client
    }

    pub fn inbound(&self) -> &Receiver<Inbound> {
        &self.inbound
    }

    pub fn next_observation(&mut self, deadline: Option<Instant>) -> Result<Observation, Error> {
        self.next_observation_result(deadline).map_err(Into::into)
    }

    pub(crate) fn next_observation_result(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Observation, ObservationError> {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(observation_timeout().into());
        }
        if let Some(observation) = self.buffered_observations.pop_front() {
            return Ok(observation);
        }
        self.read_observation(deadline)
    }

    fn read_observation(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Observation, ObservationError> {
        if let Some(error) = &self.observation_error {
            return Err(error.clone().into());
        }
        loop {
            let inbound = match deadline {
                Some(deadline) if Instant::now() >= deadline => Err(RecvTimeoutError::Timeout),
                Some(deadline) => self.inbound.recv_deadline(deadline),
                None => self
                    .inbound
                    .recv()
                    .map_err(|_| RecvTimeoutError::Disconnected),
            };
            match inbound {
                Ok(Inbound::Notification(notification)) => {
                    let event = decode_event(&notification.method, notification.params.as_deref())
                        .map_err(Error::harness)?;
                    if event.session_id.is_empty() {
                        continue;
                    }
                    self.accept_event(&event)?;
                    return Ok(Observation::Event(event));
                }
                Ok(Inbound::Request(request)) => return Ok(Observation::Request(request)),
                Ok(Inbound::Barrier(ack)) => drop(ack),
                Err(RecvTimeoutError::Timeout) => return Err(observation_timeout().into()),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(ObservationError::Transport(
                        self.client()
                            .err()
                            .unwrap_or_else(|| "hermes adapter: observation stream closed".into()),
                    ));
                }
            }
        }
    }

    fn accept_event(&mut self, event: &Event) -> Result<(), Error> {
        let reason = if event.session_id != self.runtime_id() {
            format!("event for foreign session {}", go_quote(&event.session_id))
        } else if event.seq - 1 != self.last_seq {
            format!("non-contiguous seq {} after {}", event.seq, self.last_seq)
        } else {
            self.last_seq = event.seq;
            return Ok(());
        };
        let error = Error::harness(format!(
            "hermes adapter: invalid native protocol observation: {reason}"
        ));
        self.observation_error = Some(error.clone());
        Err(error)
    }

    pub fn submit_prompt(&self, text: &str, deadline: Option<Instant>) -> Result<(), Error> {
        self.submit_prompt_receipt(text, deadline)
            .map_err(Into::into)
    }

    fn submit_prompt_receipt(
        &self,
        text: &str,
        deadline: Option<Instant>,
    ) -> Result<(), PromptStartError> {
        let params = serde_json::value::to_raw_value(&PromptSubmitParams {
            session_id: self.runtime_id(),
            text,
        })
        .map_err(Error::harness)?;
        let result = self
            .client()
            .call(METHOD_PROMPT_SUBMIT, Some(params), deadline)
            .map_err(|error| match error.fault {
                Fault::Remote { .. } => PromptStartError::Refused(Error::harness(error)),
                _ => PromptStartError::Uncertain(Error::harness(error)),
            })?;
        let result: PromptSubmitResult =
            decode_strict(result.get().as_bytes()).map_err(|error| {
                Error::harness(format!("decode {METHOD_PROMPT_SUBMIT} response: {error}"))
            })?;
        if result.status == SUBMIT_STREAMING {
            return Ok(());
        }
        let error = Error::harness(format!(
            "hermes adapter: invalid native protocol observation: prompt.submit returned status {}",
            go_quote(&result.status)
        ));
        match result.status.as_str() {
            "queued" | "steered" | "redirected" => Err(PromptStartError::Refused(error)),
            _ => Err(PromptStartError::Uncertain(error)),
        }
    }

    pub fn start_prompt(&mut self, text: &str, deadline: Option<Instant>) -> Result<Event, Error> {
        self.start_prompt_classified(text, deadline)
            .map_err(Into::into)
    }

    pub(crate) fn start_prompt_classified(
        &mut self,
        text: &str,
        deadline: Option<Instant>,
    ) -> Result<Event, PromptStartError> {
        self.observed_settlement = false;
        self.submit_prompt_receipt(text, deadline)?;
        loop {
            let observation = self.read_observation(deadline).map_err(Error::from)?;
            let Observation::Event(event) = observation else {
                self.buffered_observations.push_back(observation);
                continue;
            };
            if event.kind == EVENT_MESSAGE_START {
                return Ok(event);
            }
            let parent_settlement = if event.kind == EVENT_MESSAGE_COMPLETE {
                let payload: MessageCompletePayload =
                    decode_strict(event.payload_bytes()).map_err(Error::harness)?;
                !payload.status.is_empty()
            } else {
                false
            };
            self.buffered_observations
                .push_back(Observation::Event(event));
            if parent_settlement {
                return Err(Error::harness(
                    "hermes adapter: invalid native protocol observation: settlement before the turn opened",
                ).into());
            }
        }
    }

    pub fn interrupt(&self, deadline: Option<Instant>) -> Result<InterruptResult, Error> {
        interrupt(self.client(), self.runtime_id(), deadline)
    }

    pub fn close(&self) -> Result<(), Error> {
        self.process.close().map_err(Error::harness)
    }
}

pub(crate) fn interrupt(
    client: &RpcClient,
    session_id: &str,
    deadline: Option<Instant>,
) -> Result<InterruptResult, Error> {
    call(
        client,
        METHOD_SESSION_INTERRUPT,
        &SessionParams { session_id },
        deadline,
    )
}

fn resume_stored(
    client: &RpcClient,
    stored_id: &str,
    deadline: Option<Instant>,
) -> Result<SessionResumeResult, Error> {
    let params = serde_json::value::to_raw_value(&SessionParams {
        session_id: stored_id,
    })
    .map_err(Error::harness)?;
    let result = client
        .call(METHOD_SESSION_RESUME, Some(params), deadline)
        .map_err(|error| match &error.fault {
            Fault::Remote { object, .. } if object.code == SESSION_NOT_FOUND => Error::harness(
                format!("Hermes has no stored session {}", go_quote(stored_id)),
            ),
            _ => Error::harness(error),
        })?;
    let resumed: SessionResumeResult =
        decode_strict(result.get().as_bytes()).map_err(Error::harness)?;
    if resumed.session_id.is_empty() {
        return Err(Error::harness("Hermes omitted the reloaded session id"));
    }
    if resumed.auto_continue.is_some() {
        return Err(Error::harness(
            "Hermes would restart the turn a crash interrupted, which a reopen must not do; \
             open it in Hermes to settle the turn, or wait for Hermes to retire the marker",
        ));
    }
    if resumed.running || (!resumed.status.is_empty() && resumed.status != "idle") {
        let status = if resumed.running {
            "running"
        } else {
            &resumed.status
        };
        return Err(Error::harness(format!(
            "Hermes reattached the stored session while it was {status}"
        )));
    }
    Ok(resumed)
}

fn reopen_refusal(error: impl std::fmt::Display) -> Error {
    Error::UnsupportedControl(Box::new(UnsupportedControl {
        feature: FEATURE_OPEN_REOPEN.into(),
        reason: CONTROL_UNSATISFIABLE,
        detail: format!("Hermes could not reload its binding: {error}"),
        ..UnsupportedControl::default()
    }))
}

fn observation_timeout() -> Error {
    Error::harness("hermes adapter: observation deadline exceeded")
}

fn call<T: DeserializeOwned + Default>(
    client: &RpcClient,
    method: &str,
    params: &impl Serialize,
    deadline: Option<Instant>,
) -> Result<T, Error> {
    let params = serde_json::value::to_raw_value(params).map_err(Error::harness)?;
    let result = client
        .call(method, Some(params), deadline)
        .map_err(Error::harness)?;
    decode_strict(result.get().as_bytes())
        .map_err(|error| Error::harness(format!("decode {method} response: {error}")))
}

fn relay_inbound(client: &RpcClient, stop: Receiver<()>) -> Result<Receiver<Inbound>, Error> {
    let inbound = client.inbound();
    let read_done = client.read_done();
    let (out, relayed) = bounded(RELAY_CAPACITY);
    std::thread::Builder::new()
        .name("hermes-session-relay".into())
        .spawn(move || loop {
            select_biased! {
                recv(stop) -> _ => return,
                recv(inbound) -> message => match message {
                    Ok(Inbound::Barrier(ack)) => drop(ack),
                    Ok(message) => select_biased! {
                        recv(stop) -> _ => return,
                        send(out, message) -> sent => {
                            if sent.is_err() {
                                return;
                            }
                        }
                    },
                    Err(_) => return,
                },
                recv(read_done) -> _ => return,
            }
        })
        .map_err(Error::harness)?;
    Ok(relayed)
}
