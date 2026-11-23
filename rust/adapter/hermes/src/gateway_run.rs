use std::{collections::VecDeque, sync::Arc, time::Instant};

use oxap_adapter::{
    gojson::go_marshal, index_input_answers, jsonrpc::RequestId, runtime::Runtime, Error,
    InteractionResolution,
};
use oxap_protocol::{
    ActionCallPayload, Admission, ContentDeltaPayload, EffectiveDeliveryMode, Envelope, EnvelopeId,
    EnvelopeType, InputAnswer, InputOption, InputQuestion, InputQuestionKind, InteractionId,
    Message, MessageContent, MessageId, MessageRole, MessageSubmitResponse, ParticipantId,
    ProtocolError, RequestedDeliveryMode, RunCompletedPayload, RunFailedPayload, RunId,
    RunStartedPayload, RunStatus, RunStatusUpdatedPayload, SessionId, UserInputRequestedPayload,
    UserInputResolutionStatus, UserInputResolveRequest, UserInputResolvedPayload,
};
use serde::Serialize;
use serde_json::value::{to_raw_value, RawValue};

use crate::gateway_session::{ObservationError, PromptStartError};
use crate::native::{
    decode_server_request, decode_strict, request_session_id, ApprovalResult, ClarifyAnswersResult,
    InterruptResult, RequestCancelPayload, ServerRequest, ToolCompletePayload, ToolStartPayload,
    ValueResult, EVENT_REQUEST_CANCEL, EVENT_TOOL_COMPLETE, EVENT_TOOL_START, REQUEST_APPROVAL,
    REQUEST_CLARIFY, REQUEST_SECRET, REQUEST_SUDO,
};
use crate::rpc::IncomingRequest;
use crate::{GatewaySession, Observation, PromptObservation};

#[derive(Debug)]
pub enum RunObservation {
    Event(Box<Envelope>),
    Other(Observation),
}

const REQUESTER: &str = "hermes.gateway";
pub struct GatewayRun<'a> {
    session: &'a mut GatewaySession,
    runtime: Runtime,
    session_id: SessionId,
    run_id: RunId,
    message_id: MessageId,
    pub(crate) responder: ParticipantId,
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
    request: Arc<IncomingRequest>,
    id: InteractionId,
    requested: EnvelopeId,
    questions: Vec<InputQuestion>,
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
        self.start_run_classified(text, session_id, model_id, runtime, deadline)
            .map_err(Into::into)
    }

    pub(crate) fn start_run_classified(
        &mut self,
        text: &str,
        session_id: SessionId,
        model_id: &str,
        runtime: Runtime,
        deadline: Option<Instant>,
    ) -> Result<GatewayRun<'_>, PromptStartError> {
        let revision = &oxap_harness::current("hermes")
            .expect("embedded catalog pins Hermes")
            .capability_revision;
        let message_id = runtime.new_id("message").into();
        self.start_prompt_classified(text, deadline)?;
        let mut run = GatewayRun {
            session: self,
            run_id: runtime.new_id("run").into(),
            runtime,
            session_id,
            message_id,
            responder: "user".into(),
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
    pub(crate) fn take_started(&mut self) -> Envelope {
        self.pending
            .pop_front()
            .expect("admitted Hermes run starts with run.started")
    }

    pub(crate) fn admission(&self, model_id: &str) -> MessageSubmitResponse {
        MessageSubmitResponse {
            session_id: self.session_id.clone(),
            accepted: true,
            submission_id: self.message_id.as_str().into(),
            requested_delivery: RequestedDeliveryMode::AUTO,
            effective_delivery: EffectiveDeliveryMode::START,
            delivery_resolution: "session_idle".into(),
            admission: Admission::STARTED,
            run_id: Some(self.run_id.clone()),
            status: RunStatus::RUNNING,
            model_id: model_id.to_owned(),
            message_ids: vec![self.message_id.clone()],
            ..MessageSubmitResponse::default()
        }
    }

    pub fn resolve_input(&mut self, resolution: UserInputResolveRequest) -> Result<(), Error> {
        let index = self
            .gates
            .iter()
            .position(|gate| gate.id == resolution.interaction_id)
            .ok_or(Error::InteractionNotFound)?;
        if resolution.session_id != self.session_id
            || resolution.run_id != self.run_id
            || resolution.requested_by.as_str() != REQUESTER
            || resolution.responded_by != self.responder
        {
            return Err(Error::InvalidResolution);
        }
        self.gates[index].respond(&resolution.answers)?;
        let gate = self.gates.remove(index);
        self.resolve_gate(
            gate,
            UserInputResolutionStatus::SUBMITTED,
            resolution.answers,
        )?;
        self.queue_gate_status()
    }

    pub(crate) fn resolve_shared(
        &mut self,
        resolution: InteractionResolution,
    ) -> Result<(), Error> {
        let mut input = resolution.input.ok_or(Error::InteractionNotFound)?;
        if !self
            .gates
            .iter()
            .any(|gate| gate.id == input.interaction_id)
        {
            return Err(Error::InteractionNotFound);
        }
        if !resolution.run_id.as_str().is_empty() && resolution.run_id != self.run_id {
            return Err(Error::InvalidResolution);
        }
        if input.session_id.as_str().is_empty() {
            input.session_id = self.session_id.clone();
        }
        if input.run_id.as_str().is_empty() {
            input.run_id = self.run_id.clone();
        }
        if input.requested_by.as_str().is_empty() {
            input.requested_by = REQUESTER.into();
        }
        if !resolution.responded_by.as_str().is_empty() {
            input.responded_by = resolution.responded_by;
        } else if input.responded_by.as_str().is_empty() {
            input.responded_by = self.responder.clone();
        }
        self.resolve_input(input)
    }

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
        self.observe(deadline).map_err(Into::into)
    }

    pub(crate) fn observe(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<RunObservation>, ObservationError> {
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
                Err(error) => return Err(error),
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
                        EVENT_REQUEST_CANCEL => {
                            let cancel: RequestCancelPayload =
                                decode_strict(event.payload_bytes()).map_err(Error::harness)?;
                            self.withdraw_gate(&cancel.id)?;
                            continue;
                        }
                        _ => return Ok(Some(RunObservation::Other(Observation::Event(event)))),
                    };
                    if let Some(message) = failure {
                        self.queue_failure("hermes_tool_lifecycle", message, "")?;
                    }
                    continue;
                }
                PromptObservation::Other(Observation::Request(request)) => {
                    self.open_gate(request)?;
                    continue;
                }
            };
            self.pending.push_back(event);
        }
    }

    fn open_gate(&mut self, request: Arc<IncomingRequest>) -> Result<(), Error> {
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
        let Ok(Some(decoded)) = decode_server_request(method, params) else {
            return self.queue_failure(
                "hermes_invalid_event",
                &format!("invalid {method} gate"),
                "",
            );
        };
        let (title, description, questions) = gate_questions(decoded);
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
                requested_by: REQUESTER.into(),
                responded_by: self.responder.clone(),
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                title: title.into(),
                description,
                questions: questions.clone(),
                allow_cancel: true,
                ..UserInputRequestedPayload::default()
            },
        )?;
        requested.turn_id = Some(id.clone());
        self.gates.push(Gate {
            request,
            id,
            requested: requested.id.clone(),
            questions,
        });
        self.pending.push_back(requested);
        self.queue_gate_status()
    }

    fn withdraw_gate(&mut self, native_id: &str) -> Result<(), Error> {
        if let Some(index) = self
            .gates
            .iter()
            .position(|gate| matches!(&gate.request.id, RequestId::String(id) if id == native_id))
        {
            let gate = self.gates.remove(index);
            self.cancel_gate(gate)?;
            self.queue_gate_status()?;
        }
        Ok(())
    }

    fn queue_gate_status(&mut self) -> Result<(), Error> {
        let pending_user_input_id = self.gates.last().map(|gate| gate.id.clone());
        let status = if pending_user_input_id.is_some() {
            RunStatus::WAITING_FOR_INPUT
        } else {
            RunStatus::RUNNING
        };
        let event = self.emit(
            EnvelopeType::RUN_STATUS_UPDATED,
            &RunStatusUpdatedPayload {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                status,
                pending_user_input_id,
                updated_at_ms: self.runtime.now_ms(),
            },
        )?;
        self.pending.push_back(event);
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

    fn cancel_gate(&mut self, gate: Gate) -> Result<(), Error> {
        self.resolve_gate(gate, UserInputResolutionStatus::CANCELLED, Vec::new())
    }

    fn resolve_gate(
        &mut self,
        gate: Gate,
        status: UserInputResolutionStatus,
        answers: Vec<InputAnswer>,
    ) -> Result<(), Error> {
        let mut resolved = self.emit(
            EnvelopeType::USER_INPUT_RESOLVED,
            &UserInputResolvedPayload {
                interaction_id: gate.id.clone(),
                requested_by: REQUESTER.into(),
                responded_by: self.responder.clone(),
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                status,
                answers,
            },
        )?;
        resolved.turn_id = Some(gate.id);
        resolved.in_reply_to = Some(gate.requested);
        self.pending.push_back(resolved);
        Ok(())
    }

    fn settle_children(&mut self) -> Result<(), Error> {
        for gate in std::mem::take(&mut self.gates) {
            self.cancel_gate(gate)?;
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

    pub(crate) fn queue_failure(
        &mut self,
        code: &str,
        message: &str,
        settled_by: &str,
    ) -> Result<(), Error> {
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

impl Gate {
    fn respond(&self, answers: &[InputAnswer]) -> Result<(), Error> {
        if answers.len() != self.questions.len() {
            return Err(Error::InvalidResolution);
        }
        let indexed = index_input_answers(&self.questions, answers)?;
        let result = match self.request.method.as_str() {
            REQUEST_APPROVAL => self.request.respond(&ApprovalResult {
                choice: indexed["choice"].selected_option_ids[0].clone(),
            }),
            REQUEST_CLARIFY => {
                let mut result = ClarifyAnswersResult::default();
                for question in &self.questions {
                    let answer = indexed[question.id.as_str()];
                    let value = match question.kind.as_str() {
                        "text" => answer.text.clone(),
                        "multi_choice" => go_marshal(
                            &serde_json::to_string(&answer.selected_option_ids)
                                .map_err(Error::harness)?,
                        ),
                        _ => answer.selected_option_ids[0].clone(),
                    };
                    result.answers.insert(question.id.clone(), value);
                }
                self.request.respond(&result)
            }
            REQUEST_SUDO | REQUEST_SECRET => self.request.respond(&ValueResult {
                value: answers[0].text.clone(),
            }),
            _ => unreachable!("only Hermes input requests create gates"),
        };
        result.map_err(Error::harness)
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
