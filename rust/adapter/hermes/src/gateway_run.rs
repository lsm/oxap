use std::{collections::VecDeque, time::Instant};

use oxap_adapter::{jsonrpc::RequestId, runtime::Runtime, Error};
use oxap_protocol::{
    ActionCallPayload, ContentDeltaPayload, Envelope, EnvelopeId, EnvelopeType, InputOption,
    InputQuestion, InputQuestionKind, InteractionId, Message, MessageContent, MessageId,
    MessageRole, ProtocolError, RunCompletedPayload, RunFailedPayload, RunId, RunStartedPayload,
    RunStatus, RunStatusUpdatedPayload, SessionId, UserInputRequestedPayload,
    UserInputResolutionStatus, UserInputResolvedPayload,
};
use serde::Serialize;
use serde_json::value::{to_raw_value, RawValue};

use crate::gateway_session::ObservationError;
use crate::native::{
    decode_server_request, decode_strict, request_session_id, InterruptResult, ServerRequest,
    ToolCompletePayload, ToolStartPayload, EVENT_TOOL_COMPLETE, EVENT_TOOL_START, REQUEST_APPROVAL,
    REQUEST_CLARIFY, REQUEST_SECRET, REQUEST_SUDO,
};
use crate::rpc::IncomingRequest;
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
    gates: Vec<Gate>,
    terminal: Option<RunStatus>,
}

struct ToolCall {
    native_id: String,
    payload: ActionCallPayload,
    started: Option<EnvelopeId>,
}

struct Gate {
    id: InteractionId,
    requested: EnvelopeId,
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
            gates: Vec::new(),
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
            let observation = match self.session.next_prompt_observation_result(deadline) {
                Ok(observation) => observation,
                Err(ObservationError::Transport(reason)) => {
                    self.queue_failure("hermes_process_exit", &reason, "inferred")?;
                    continue;
                }
                Err(ObservationError::Other(error)) => return Err(error),
            };
            let event = match observation {
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
                    self.settle_children()?;
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
                PromptObservation::Failed(error) => self.fail(error, "")?,
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
                        self.queue_failure("hermes_tool_lifecycle", message, "")?;
                    }
                    continue;
                }
                PromptObservation::Other(Observation::Request(request)) => {
                    if !matches!(
                        request.method.as_str(),
                        REQUEST_APPROVAL | REQUEST_CLARIFY | REQUEST_SUDO | REQUEST_SECRET
                    ) {
                        return Ok(Some(RunObservation::Other(Observation::Request(request))));
                    }
                    self.open_gate(&request)?;
                    continue;
                }
            };
            self.pending.push_back(event);
        }
    }

    fn open_gate(&mut self, request: &IncomingRequest) -> Result<(), Error> {
        let method = request.method.as_str();
        if !matches!(request.id, RequestId::String(_)) {
            return self.queue_failure(
                "hermes_external_activity",
                &format!("{method} request with a non-string id"),
                "",
            );
        }
        let params = request
            .params
            .as_ref()
            .map_or(b"".as_slice(), |raw| raw.get().as_bytes());
        let owner = request_session_id(params);
        if owner != self.session.runtime_id() {
            let message = format!(
                "{method} request for foreign session {}",
                oxap_adapter::gojson::go_quote(&owner)
            );
            return self.queue_failure("hermes_external_activity", &message, "");
        }
        let Ok(Some(request)) = decode_server_request(method, params) else {
            return self.queue_failure(
                "hermes_invalid_event",
                &format!("invalid {method} gate"),
                "",
            );
        };
        let (title, description, questions) = gate_questions(request);
        if questions
            .iter()
            .any(|question| question.options.iter().any(|option| option.id.is_empty()))
        {
            return self.queue_failure(
                "hermes_invalid_event",
                "gate with a question nobody can answer",
                "",
            );
        }
        let id: InteractionId = self.runtime.new_id("interaction").into();
        let mut requested = self.emit(
            EnvelopeType::USER_INPUT_REQUESTED,
            &UserInputRequestedPayload {
                interaction_id: id.clone(),
                requested_by: "hermes.gateway".into(),
                responded_by: "user".into(),
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                title: title.into(),
                description,
                questions,
                allow_cancel: true,
                ..UserInputRequestedPayload::default()
            },
        )?;
        requested.turn_id = Some(id.clone());
        self.gates.push(Gate {
            id: id.clone(),
            requested: requested.id.clone(),
        });
        self.pending.push_back(requested);
        let waiting = self.emit(
            EnvelopeType::RUN_STATUS_UPDATED,
            &RunStatusUpdatedPayload {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                status: RunStatus::WAITING_FOR_INPUT,
                pending_user_input_id: Some(id),
                updated_at_ms: self.runtime.now_ms(),
            },
        )?;
        self.pending.push_back(waiting);
        Ok(())
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

    fn settle_children(&mut self) -> Result<(), Error> {
        for gate in std::mem::take(&mut self.gates) {
            let mut resolved = self.emit(
                EnvelopeType::USER_INPUT_RESOLVED,
                &UserInputResolvedPayload {
                    interaction_id: gate.id.clone(),
                    requested_by: "hermes.gateway".into(),
                    responded_by: "user".into(),
                    session_id: self.session_id.clone(),
                    run_id: self.run_id.clone(),
                    status: UserInputResolutionStatus::CANCELLED,
                    ..UserInputResolvedPayload::default()
                },
            )?;
            resolved.turn_id = Some(gate.id);
            resolved.in_reply_to = Some(gate.requested);
            self.pending.push_back(resolved);
        }
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

    fn fail(&mut self, error: ProtocolError, settled_by: &str) -> Result<Envelope, Error> {
        self.settle_children()?;
        let event = self.emit(
            EnvelopeType::RUN_FAILED,
            &RunFailedPayload {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                error,
                settled_by: settled_by.to_owned(),
                ..RunFailedPayload::default()
            },
        )?;
        self.terminal = Some(RunStatus::FAILED);
        Ok(event)
    }

    fn queue_failure(&mut self, code: &str, message: &str, settled_by: &str) -> Result<(), Error> {
        let event = self.fail(protocol_error(code, message), settled_by)?;
        self.pending.push_back(event);
        Ok(())
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

fn gate_questions(request: ServerRequest) -> (&'static str, String, Vec<InputQuestion>) {
    match request {
        ServerRequest::Approval(request) => {
            let description = if request.command.is_empty() {
                request.description
            } else {
                request.command
            };
            let question =
                input_question("choice".into(), description.clone(), request.choices, false);
            ("Command approval", description, vec![question])
        }
        ServerRequest::Clarify(request) => (
            "Clarification",
            String::new(),
            request
                .questions
                .into_iter()
                .map(|question| {
                    input_question(
                        question.qid,
                        question.question,
                        question.choices,
                        question.multi_select,
                    )
                })
                .collect(),
        ),
        ServerRequest::Sudo(request) => (
            "Password required",
            request.command,
            vec![input_question(
                "password".into(),
                "Enter the sudo password".into(),
                Vec::new(),
                false,
            )],
        ),
        ServerRequest::Secret(request) => (
            "Secret required",
            request.env_var,
            vec![input_question(
                "value".into(),
                request.prompt,
                Vec::new(),
                false,
            )],
        ),
    }
}

fn input_question(
    id: String,
    prompt: String,
    choices: Vec<String>,
    multi_select: bool,
) -> InputQuestion {
    let kind = match (choices.is_empty(), multi_select) {
        (true, _) => InputQuestionKind::TEXT,
        (false, true) => InputQuestionKind::MULTI_CHOICE,
        (false, false) => InputQuestionKind::SINGLE_CHOICE,
    };
    InputQuestion {
        id,
        prompt,
        kind,
        required: true,
        options: choices
            .into_iter()
            .map(|choice| InputOption {
                id: choice.clone(),
                label: choice,
                ..InputOption::default()
            })
            .collect(),
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
