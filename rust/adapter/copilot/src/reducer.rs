use std::collections::BTreeMap;
use std::sync::Arc;

use oxap_adapter::memory::{Clock, IdGenerator};
use oxap_adapter::Error;
use oxap_protocol::{
    ContentPart, ContentPartType, Envelope, EnvelopeType, MessageId, ProtocolError, RunId,
    RunStatus, SessionId,
};
use serde_json::Value;

use crate::adapter::MAX_QUEUED;
use crate::native::{text, Event};
use crate::usage::UsageState;
use crate::CAPABILITY_REVISION;

pub(crate) type Payload = BTreeMap<&'static str, Value>;

#[derive(Clone, Debug, Default)]
pub(crate) struct Run {
    pub(crate) id: RunId,
    pub(crate) message_id: MessageId,
    pub(crate) native_message: String,
    pub(crate) interaction: String,
    pub(crate) status: RunStatus,
    pub(crate) started: bool,
    pub(crate) terminal: bool,
    next: u64,
    parts: Vec<ContentPart>,
    usage: UsageState,
}

pub struct Reducer {
    session_id: SessionId,
    native_id: String,
    ids: Arc<dyn IdGenerator>,
    clock: Arc<dyn Clock>,
    pub(crate) runs: Vec<Run>,
    pub(crate) current: Option<usize>,
    pub(crate) events: Vec<Envelope>,
}

impl Reducer {
    pub fn new(
        session_id: SessionId,
        native_id: String,
        ids: Arc<dyn IdGenerator>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            session_id,
            native_id,
            ids,
            clock,
            runs: Vec::new(),
            current: None,
            events: Vec::new(),
        }
    }

    fn live(&self) -> usize {
        self.runs.iter().filter(|run| !run.terminal).count()
    }

    pub fn admit(&mut self, queue: bool) -> Result<usize, Error> {
        let live = self.live();
        if live > MAX_QUEUED {
            return Err(Error::RunActive);
        }
        let mut status = RunStatus::RUNNING;
        if queue || live > 0 {
            status = RunStatus::QUEUED;
            let queued = self
                .runs
                .iter()
                .filter(|run| !run.terminal && run.status == RunStatus::QUEUED)
                .count();
            if queued >= MAX_QUEUED {
                return Err(Error::RunActive);
            }
        }
        self.runs.push(Run {
            id: self.ids.new_id("run").into(),
            message_id: self.ids.new_id("message").into(),
            status,
            next: 1,
            ..Run::default()
        });
        Ok(self.runs.len() - 1)
    }

    pub fn observe(&mut self, event: &Event) -> Result<(), Error> {
        if event.session_id != self.native_id {
            if let Some(current) = self.current {
                let message = "an event for another Copilot session reached this one";
                return self.fail(current, "copilot_foreign_session", message);
            }
            return Ok(());
        }
        let interaction = text(&event.data, "interactionId");
        let foreign = self
            .current
            .is_none_or(|current| self.runs[current].interaction != interaction);
        if event.kind != "user.message" && !interaction.is_empty() && foreign {
            return Ok(());
        }
        match event.kind.as_str() {
            "user.message" => self.start(event),
            "assistant.message_delta" => {
                let Some(current) = self.current else {
                    return Ok(());
                };
                let delta = text(&event.data, "deltaContent");
                if delta.is_empty() {
                    return Ok(());
                }
                let payload = Payload::from([
                    ("message_id", json(&self.runs[current].message_id)),
                    ("part", json(&text_part(delta))),
                ]);
                self.emit(current, EnvelopeType::CONTENT_DELTA, payload, false)
            }
            "assistant.message" => {
                let content = text(&event.data, "content");
                if let (Some(current), false) = (self.current, content.is_empty()) {
                    self.runs[current].parts.push(text_part(content));
                }
                Ok(())
            }
            "session.idle" => {
                let Some(current) = self.current else {
                    return Ok(());
                };
                if event.data.get("aborted").map(|raw| raw.get()) == Some("true") {
                    let message =
                        "Copilot ended the busy period as aborted, without a cancel for this run";
                    return self.fail(current, "copilot_aborted", message);
                }
                self.complete(current)
            }
            "session.error" => {
                let Some(current) = self.current else {
                    return Ok(());
                };
                let mut message = text(&event.data, "message");
                if message.is_empty() {
                    message = "Copilot reported a session error".into();
                }
                self.fail(current, "copilot_session_error", &message)
            }
            "assistant.usage" => {
                if let Some(current) = self.current {
                    self.runs[current].usage.observe(&event.data);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn start(&mut self, event: &Event) -> Result<(), Error> {
        let id = text(&event.data, "messageId");
        if id.is_empty() {
            return Ok(());
        }
        let Some(index) = self
            .runs
            .iter()
            .position(|run| !run.started && !run.terminal && run.native_message == id)
        else {
            return Ok(());
        };
        if let Some(current) = self.current {
            self.complete(current)?;
        }
        let run = &mut self.runs[index];
        run.interaction = text(&event.data, "interactionId");
        run.started = true;
        run.status = RunStatus::RUNNING;
        self.current = Some(index);
        let payload = Payload::from([("status", json(&RunStatus::RUNNING))]);
        self.emit(index, EnvelopeType::RUN_STARTED, payload, false)
    }

    pub fn complete(&mut self, index: usize) -> Result<(), Error> {
        let run = &self.runs[index];
        let content = if run.parts.is_empty() {
            Value::String(String::new())
        } else {
            json(&run.parts)
        };
        let response =
            serde_json::json!({ "id": run.message_id, "role": "assistant", "content": content });
        let payload = Payload::from([
            ("final_response", response),
            ("stop_reason", Value::from("end_turn")),
        ]);
        self.emit(index, EnvelopeType::RUN_COMPLETED, payload, true)
    }

    pub fn fail(&mut self, index: usize, code: &str, message: &str) -> Result<(), Error> {
        let error = ProtocolError {
            code: code.into(),
            message: message.into(),
            ..ProtocolError::default()
        };
        let payload = Payload::from([("error", json(&error))]);
        self.emit(index, EnvelopeType::RUN_FAILED, payload, true)
    }

    pub(crate) fn emit(
        &mut self,
        index: usize,
        kind: EnvelopeType,
        mut payload: Payload,
        terminal: bool,
    ) -> Result<(), Error> {
        if self.runs[index].terminal {
            return Ok(());
        }
        let run = &self.runs[index];
        payload.insert("session_id", json(&self.session_id));
        payload.insert("run_id", json(&run.id));
        if terminal && run.usage.measured {
            payload.insert("usage", json(&run.usage.portable()));
        }
        let mut envelope = Envelope::new(kind.clone(), self.ids.new_id("event"), &payload)
            .map_err(Error::harness)?;
        envelope.sequence = Some(run.next);
        envelope.timestamp_ms = Some(self.clock.now_ms());
        envelope.session_id = Some(self.session_id.clone());
        envelope.run_id = Some(run.id.clone());
        envelope.capability_revision = Some(CAPABILITY_REVISION.clone());
        let run = &mut self.runs[index];
        run.next += 1;
        if terminal {
            envelope.extensions = run.usage.extensions();
            run.terminal = true;
            run.status = if kind == EnvelopeType::RUN_FAILED {
                RunStatus::FAILED
            } else {
                RunStatus::COMPLETED
            };
            if self.current == Some(index) {
                self.current = None;
            }
        }
        self.events.push(envelope);
        Ok(())
    }
}

#[cfg(test)]
mod tests;

fn text_part(text: String) -> ContentPart {
    ContentPart {
        part_type: ContentPartType::TEXT,
        text,
        ..ContentPart::default()
    }
}

fn json(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).expect("reducer payloads hold only JSON values")
}
