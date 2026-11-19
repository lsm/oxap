use std::collections::BTreeMap;
use std::sync::Arc;

use oxap_adapter::memory::{Clock, IdGenerator};
use oxap_adapter::Error;
use oxap_protocol::{
    ContentPart, Envelope, EnvelopeType, MessageId, ProtocolError, RunId, RunStatus, SessionId,
};
use serde_json::Value;

use crate::adapter::MAX_QUEUED;
use crate::CAPABILITY_REVISION;

pub(crate) type Payload = BTreeMap<&'static str, Value>;

#[derive(Clone, Debug, Default)]
pub(crate) struct Run {
    pub(crate) id: RunId,
    pub(crate) message_id: MessageId,
    pub(crate) status: RunStatus,
    pub(crate) terminal: bool,
    next: u64,
    parts: Vec<ContentPart>,
}

pub struct Reducer {
    session_id: SessionId,
    ids: Arc<dyn IdGenerator>,
    clock: Arc<dyn Clock>,
    pub(crate) runs: Vec<Run>,
    pub(crate) current: Option<usize>,
    pub(crate) events: Vec<Envelope>,
}

impl Reducer {
    pub fn new(session_id: SessionId, ids: Arc<dyn IdGenerator>, clock: Arc<dyn Clock>) -> Self {
        Self {
            session_id,
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
        if live > MAX_QUEUED * 2 {
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
            if queued >= MAX_QUEUED * 2 {
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
            run.status = RunStatus::RUNNING;
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

fn json(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).expect("reducer payloads hold only JSON values")
}
