use std::time::Instant;

use oxap_adapter::{runtime::Runtime, Error};
use oxap_protocol::{
    ContentDeltaPayload, Envelope, EnvelopeType, Message, MessageContent, MessageId, MessageRole,
    RunCompletedPayload, RunFailedPayload, RunId, RunStartedPayload, RunStatus, SessionId,
};
use serde::Serialize;

use crate::{native::InterruptResult, GatewaySession, Observation, PromptObservation};

#[derive(Debug)]
pub enum RunObservation {
    Event(Box<Envelope>),
    Other(Observation),
}

pub struct GatewayRun<'a> {
    session: &'a mut GatewaySession,
    runtime: Runtime,
    session_id: SessionId,
    run_id: RunId,
    message_id: MessageId,
    revision: &'static str,
    sequence: u64,
    start: Option<Envelope>,
    terminal: Option<RunStatus>,
}

impl GatewaySession {
    pub fn start_run(
        &mut self,
        text: &str,
        session_id: SessionId,
        model_id: &str,
        runtime: Runtime,
        deadline: Option<Instant>,
    ) -> Result<GatewayRun<'_>, Error> {
        let revision = &oxap_harness::current("hermes")
            .expect("embedded catalog pins Hermes")
            .capability_revision;
        let message_id = runtime.new_id("message").into();
        self.start_prompt(text, deadline)?;
        let mut run = GatewayRun {
            session: self,
            run_id: runtime.new_id("run").into(),
            runtime,
            session_id,
            message_id,
            revision,
            sequence: 0,
            start: None,
            terminal: None,
        };
        run.start = Some(run.emit(
            EnvelopeType::RUN_STARTED,
            &RunStartedPayload {
                session_id: run.session_id.clone(),
                run_id: run.run_id.clone(),
                status: RunStatus::RUNNING,
                model_id: model_id.to_owned(),
                started_at_ms: run.runtime.now_ms(),
                ..RunStartedPayload::default()
            },
        )?);
        Ok(run)
    }
}

impl GatewayRun<'_> {
    pub fn interrupt(&self, deadline: Option<Instant>) -> Result<InterruptResult, Error> {
        if let Some(status) = &self.terminal {
            return Err(Error::RunAlreadyTerminal {
                run_id: Some(self.run_id.clone()),
                status: Some(status.clone()),
            });
        }
        self.session.interrupt(deadline)
    }

    pub fn next_observation(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<RunObservation>, Error> {
        if self.terminal.is_some() {
            return Ok(None);
        }
        let event = if let Some(start) = self.start.take() {
            start
        } else {
            match self.session.next_prompt_observation(deadline)? {
                PromptObservation::Delta(part) => self.emit(
                    EnvelopeType::CONTENT_DELTA,
                    &ContentDeltaPayload {
                        session_id: self.session_id.clone(),
                        run_id: self.run_id.clone(),
                        message_id: Some(self.message_id.clone()),
                        part,
                    },
                )?,
                PromptObservation::Completed { text, usage } => self.emit(
                    EnvelopeType::RUN_COMPLETED,
                    &RunCompletedPayload {
                        session_id: self.session_id.clone(),
                        run_id: self.run_id.clone(),
                        final_response: Message {
                            id: Some(self.message_id.clone()),
                            role: MessageRole::ASSISTANT,
                            content: MessageContent::from_text(&text),
                            ..Message::default()
                        },
                        stop_reason: "completed".to_owned(),
                        usage,
                        ..RunCompletedPayload::default()
                    },
                )?,
                PromptObservation::Failed(error) => self.emit(
                    EnvelopeType::RUN_FAILED,
                    &RunFailedPayload {
                        session_id: self.session_id.clone(),
                        run_id: self.run_id.clone(),
                        error,
                        ..RunFailedPayload::default()
                    },
                )?,
                PromptObservation::Other(other) => return Ok(Some(RunObservation::Other(other))),
            }
        };
        if event.envelope_type == EnvelopeType::RUN_COMPLETED {
            self.terminal = Some(RunStatus::COMPLETED);
        } else if event.envelope_type == EnvelopeType::RUN_FAILED {
            self.terminal = Some(RunStatus::FAILED);
        }
        Ok(Some(RunObservation::Event(Box::new(event))))
    }

    fn emit(&mut self, kind: EnvelopeType, payload: &impl Serialize) -> Result<Envelope, Error> {
        let mut event =
            Envelope::new(kind, self.runtime.new_id("event"), payload).map_err(Error::harness)?;
        self.sequence += 1;
        event.sequence = Some(self.sequence);
        event.timestamp_ms = Some(self.runtime.now_ms());
        event.session_id = Some(self.session_id.clone());
        event.run_id = Some(self.run_id.clone());
        event.capability_revision = Some(self.revision.to_owned());
        Ok(event)
    }
}
