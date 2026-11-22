use std::{sync::Arc, time::Instant};

use crossbeam_channel::{bounded, select_biased, Receiver, RecvTimeoutError, Sender};
use oxap_adapter::gojson::{go_quote, zero};
use oxap_adapter::{Error, UnsupportedControl, CONTROL_UNSATISFIABLE};
use oxap_protocol::{ReasoningLevel, FEATURE_SESSION_REASONING};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::native::{
    decode_strict, Event, InterruptResult, PromptSubmitResult, METHOD_CONFIG_SET,
    METHOD_PROMPT_SUBMIT, METHOD_SESSION_CREATE, METHOD_SESSION_INTERRUPT, SUBMIT_STREAMING,
};
use crate::process::{Process, ProcessConfig};
use crate::rpc::{decode_event, Inbound, IncomingRequest, RpcClient};

const RELAY_CAPACITY: usize = 256;

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

pub struct GatewaySession {
    _stop_relay: Sender<()>,
    process: Process,
    inbound: Receiver<Inbound>,
    created: SessionCreateResult,
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
        let process = Process::start(config, deadline).map_err(Error::harness)?;
        let (stop_relay, stop) = bounded(0);
        let inbound = relay_inbound(&process.client, stop)?;
        let effort = if *reasoning == ReasoningLevel::OFF {
            "none"
        } else {
            reasoning.as_str()
        };
        let created: SessionCreateResult = call(
            &process.client,
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
                &process.client,
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
        Ok(Self {
            _stop_relay: stop_relay,
            process,
            inbound,
            created,
            last_seq: 0,
            observation_error: None,
        })
    }

    pub fn runtime_id(&self) -> &str {
        &self.created.session_id
    }

    pub fn stored_id(&self) -> &str {
        &self.created.stored_session_id
    }

    pub fn client(&self) -> &RpcClient {
        &self.process.client
    }

    pub fn inbound(&self) -> &Receiver<Inbound> {
        &self.inbound
    }

    pub fn next_observation(&mut self, deadline: Option<Instant>) -> Result<Observation, Error> {
        if let Some(error) = &self.observation_error {
            return Err(error.clone());
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
                Err(RecvTimeoutError::Timeout) => {
                    return Err(Error::harness(
                        "hermes adapter: observation deadline exceeded",
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Error::harness(self.client().err().unwrap_or_else(|| {
                        "hermes adapter: observation stream closed".into()
                    })));
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
        let result: PromptSubmitResult = call(
            self.client(),
            METHOD_PROMPT_SUBMIT,
            &PromptSubmitParams {
                session_id: self.runtime_id(),
                text,
            },
            deadline,
        )?;
        if result.status != SUBMIT_STREAMING {
            return Err(Error::harness(format!(
                "hermes adapter: invalid native protocol observation: prompt.submit returned status {}",
                go_quote(&result.status)
            )));
        }
        Ok(())
    }

    pub fn interrupt(&self, deadline: Option<Instant>) -> Result<InterruptResult, Error> {
        call(
            self.client(),
            METHOD_SESSION_INTERRUPT,
            &SessionParams {
                session_id: self.runtime_id(),
            },
            deadline,
        )
    }

    pub fn close(&self) -> Result<(), Error> {
        self.process.close().map_err(Error::harness)
    }
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
