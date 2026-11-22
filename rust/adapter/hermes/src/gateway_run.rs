use std::{collections::VecDeque, time::Instant};

use oxap_adapter::{runtime::Runtime, Error};
use oxap_protocol::{
    ActionCallPayload, ContentDeltaPayload, Envelope, EnvelopeId, EnvelopeType, Message,
    MessageContent, MessageId, MessageRole, ProtocolError, RunCompletedPayload, RunFailedPayload,
    RunId, RunStartedPayload, RunStatus, SessionId,
};
use serde::Serialize;
use serde_json::value::{to_raw_value, RawValue};

use crate::native::{
    decode_strict, InterruptResult, ToolCompletePayload, ToolStartPayload, EVENT_TOOL_COMPLETE,
    EVENT_TOOL_START,
};
use crate::{GatewaySession, Observation, PromptObservation};

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
    pending: VecDeque<Envelope>,
    tools: Vec<ToolCall>,
    terminal: Option<RunStatus>,
}

struct ToolCall {
    native_id: String,
    payload: ActionCallPayload,
    started: Option<EnvelopeId>,
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
            pending: VecDeque::new(),
            tools: Vec::new(),
            terminal: None,
        };
        let start = run.emit(
            EnvelopeType::RUN_STARTED,
            &RunStartedPayload {
                session_id: run.session_id.clone(),
                run_id: run.run_id.clone(),
                status: RunStatus::RUNNING,
                model_id: model_id.to_owned(),
                started_at_ms: run.runtime.now_ms(),
                ..RunStartedPayload::default()
            },
        )?;
        run.pending.push_back(start);
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
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(RunObservation::Event(Box::new(event))));
            }
            if self.terminal.is_some() {
                return Ok(None);
            }
            let event = match self.session.next_prompt_observation(deadline)? {
                PromptObservation::Delta(part) => self.emit(
                    EnvelopeType::CONTENT_DELTA,
                    &ContentDeltaPayload {
                        session_id: self.session_id.clone(),
                        run_id: self.run_id.clone(),
                        message_id: Some(self.message_id.clone()),
                        part,
                    },
                )?,
                PromptObservation::Completed { text, usage } => {
                    self.settle_tools()?;
                    let event = self.emit(
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
                    )?;
                    self.terminal = Some(RunStatus::COMPLETED);
                    event
                }
                PromptObservation::Failed(error) => self.fail(error)?,
                PromptObservation::Other(Observation::Event(event)) => {
                    let failure = match event.kind.as_str() {
                        EVENT_TOOL_START => self.start_tool(
                            decode_strict(event.payload_bytes()).map_err(Error::harness)?,
                        )?,
                        EVENT_TOOL_COMPLETE => self.complete_tool(
                            decode_strict(event.payload_bytes()).map_err(Error::harness)?,
                        )?,
                        _ => return Ok(Some(RunObservation::Other(Observation::Event(event)))),
                    };
                    if let Some(message) = failure {
                        let event = self.fail(protocol_error("hermes_tool_lifecycle", message))?;
                        self.pending.push_back(event);
                    }
                    continue;
                }
                PromptObservation::Other(other) => return Ok(Some(RunObservation::Other(other))),
            };
            self.pending.push_back(event);
        }
    }

    fn start_tool(&mut self, native: ToolStartPayload) -> Result<Option<&'static str>, Error> {
        if self
            .tools
            .iter()
            .any(|tool| tool.native_id == native.tool_id)
        {
            return Ok(Some("duplicate tool call"));
        }
        let arguments = native
            .args
            .as_ref()
            .map(|raw| serde_json::from_str::<serde_json::Value>(raw.get()))
            .transpose()
            .map_err(Error::harness)?
            .unwrap_or_default();
        let mut payload = ActionCallPayload {
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            tool_call_id: self.runtime.new_id("tool-call").into(),
            requested_by: Some("hermes.gateway".into()),
            execution_owner: "hermes".into(),
            name: native.name,
            arguments_json: Some(to_raw_value(&arguments).map_err(Error::harness)?),
            ..ActionCallPayload::default()
        };
        let requested = self.queue_action(EnvelopeType::ACTION_CALL_REQUESTED, &payload, None)?;
        payload.arguments_json = None;
        let started =
            self.queue_action(EnvelopeType::ACTION_CALL_STARTED, &payload, Some(requested))?;
        self.tools.push(ToolCall {
            native_id: native.tool_id,
            payload,
            started: Some(started),
        });
        Ok(None)
    }

    fn complete_tool(
        &mut self,
        native: ToolCompletePayload,
    ) -> Result<Option<&'static str>, Error> {
        let Some(tool) = self
            .tools
            .iter_mut()
            .find(|tool| tool.native_id == native.tool_id)
        else {
            return Ok(Some("unmatched tool completion"));
        };
        let Some(started) = tool.started.take() else {
            return Ok(Some("unmatched tool completion"));
        };
        let mut payload = tool.payload.clone();
        payload.result = Some(native.result.unwrap_or_else(null_value));
        self.queue_action(EnvelopeType::ACTION_CALL_COMPLETED, &payload, Some(started))?;
        Ok(None)
    }

    fn settle_tools(&mut self) -> Result<(), Error> {
        for ToolCall {
            mut payload,
            started,
            ..
        } in std::mem::take(&mut self.tools)
        {
            if let Some(started) = started {
                payload.error = Some(protocol_error(
                    "incomplete_tool",
                    "turn settled with an unfinished hermes tool",
                ));
                self.queue_action(EnvelopeType::ACTION_CALL_FAILED, &payload, Some(started))?;
            }
        }
        Ok(())
    }

    fn fail(&mut self, error: ProtocolError) -> Result<Envelope, Error> {
        self.settle_tools()?;
        let event = self.emit(
            EnvelopeType::RUN_FAILED,
            &RunFailedPayload {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                error,
                ..RunFailedPayload::default()
            },
        )?;
        self.terminal = Some(RunStatus::FAILED);
        Ok(event)
    }

    fn queue_action(
        &mut self,
        kind: EnvelopeType,
        payload: &ActionCallPayload,
        in_reply_to: Option<EnvelopeId>,
    ) -> Result<EnvelopeId, Error> {
        let mut event = self.emit(kind, payload)?;
        event.tool_call_id = Some(payload.tool_call_id.clone());
        event.in_reply_to = in_reply_to;
        let id = event.id.clone();
        self.pending.push_back(event);
        Ok(id)
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

fn null_value() -> Box<RawValue> {
    RawValue::from_string("null".to_owned()).expect("null is valid JSON")
}

fn protocol_error(code: &str, message: &str) -> ProtocolError {
    ProtocolError {
        code: code.to_owned(),
        message: message.to_owned(),
        ..ProtocolError::default()
    }
}
