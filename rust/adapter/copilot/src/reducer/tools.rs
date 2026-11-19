use oxap_adapter::Error;
use oxap_protocol::{EnvelopeId, EnvelopeType, ProtocolError, ToolCallId};
use serde_json::value::RawValue;

use super::{json, native_raw, Payload, Reducer};
use crate::native::{text, Event, Object};

const LIFECYCLE: &str = "copilot_invalid_tool_lifecycle";

#[derive(Clone, Debug, Default)]
pub(crate) struct Tool {
    id: ToolCallId,
    native_id: String,
    name: String,
    started: Option<EnvelopeId>,
    terminal: bool,
}

fn payload(tool: &Tool) -> Payload {
    Payload::from([
        ("tool_call_id", json(&tool.id)),
        ("requested_by", json(&"copilot.headless")),
        ("execution_owner", json(&"copilot-cli")),
        ("name", json(&tool.name)),
    ])
}

fn error(code: &str, message: &str) -> Box<RawValue> {
    json(&ProtocolError {
        code: code.into(),
        message: message.into(),
        ..ProtocolError::default()
    })
}

impl Reducer {
    pub(super) fn observe_tool(&mut self, event: &Event) -> Result<(), Error> {
        let Some(index) = self.current else {
            return Ok(());
        };
        let id = text(&event.data, "toolCallId");
        let found = self.runs[index]
            .tools
            .iter()
            .position(|tool| tool.native_id == id);
        match event.kind.as_str() {
            "tool.execution_start" => {
                let name = text(&event.data, "toolName");
                if id.is_empty() || name.is_empty() || found.is_some() {
                    let message = "a tool start that is unnamed or already started";
                    return self.fail(index, LIFECYCLE, message);
                }
                let tool = Tool {
                    id: self.ids.new_id("tool-call").into(),
                    native_id: id,
                    name,
                    ..Tool::default()
                };
                let mut requested = payload(&tool);
                requested.insert(
                    "arguments_json",
                    native_raw(event.data.get("arguments").map(Box::as_ref)),
                );
                let started = payload(&tool);
                self.runs[index].tools.push(tool);
                let kind = EnvelopeType::ACTION_CALL_REQUESTED;
                let asked = self.append(index, kind, requested, false, None)?;
                let kind = EnvelopeType::ACTION_CALL_STARTED;
                let started = self.append(index, kind, started, false, asked)?;
                if let Some(tool) = self.runs[index].tools.last_mut() {
                    tool.started = started;
                }
                Ok(())
            }
            "tool.execution_partial_result" | "tool.shell_output" => {
                let Some(tool) = found.map(|found| &self.runs[index].tools[found]) else {
                    return Ok(());
                };
                if tool.terminal {
                    return Ok(());
                }
                let mut progress = text(&event.data, "partialOutput");
                if progress.is_empty() {
                    progress = text(&event.data, "text");
                }
                let (mut body, reply) = (payload(tool), tool.started.clone());
                body.insert("progress", json(&progress));
                let kind = EnvelopeType::ACTION_CALL_PROGRESS;
                self.append(index, kind, body, false, reply).map(drop)
            }
            "tool.execution_complete" => self.complete_tool(index, found, &event.data),
            _ => Ok(()),
        }
    }

    fn complete_tool(
        &mut self,
        index: usize,
        found: Option<usize>,
        data: &Object,
    ) -> Result<(), Error> {
        let Some(found) = found else {
            let message = "a tool completion without a start in this run";
            return self.fail(index, LIFECYCLE, message);
        };
        let tool = &mut self.runs[index].tools[found];
        if tool.terminal {
            let message = "a second completion for one tool call";
            return self.fail(index, LIFECYCLE, message);
        }
        tool.terminal = true;
        let (mut body, reply) = (payload(tool), tool.started.clone());
        if data.get("success").map(|raw| raw.get()) == Some("true") {
            body.insert("result", native_raw(data.get("result").map(Box::as_ref)));
            let kind = EnvelopeType::ACTION_CALL_COMPLETED;
            return self.append(index, kind, body, false, reply).map(drop);
        }
        let problem: Object = data
            .get("error")
            .and_then(|raw| serde_json::from_str(raw.get()).ok())
            .unwrap_or_default();
        let mut code = "copilot_tool_failed";
        if text(&problem, "code") == "denied" {
            code = "copilot_tool_denied";
        }
        let mut message = text(&problem, "message");
        if message.is_empty() {
            message = "Copilot reported the tool call failed".into();
        }
        body.insert("error", error(code, &message));
        let kind = EnvelopeType::ACTION_CALL_FAILED;
        self.append(index, kind, body, false, reply).map(drop)
    }

    pub(super) fn settle_tools(&mut self, index: usize) -> Result<(), Error> {
        for found in 0..self.runs[index].tools.len() {
            let tool = &mut self.runs[index].tools[found];
            if tool.terminal {
                continue;
            }
            tool.terminal = true;
            let (mut body, reply) = (payload(tool), tool.started.clone());
            let message = "the Copilot run settled with an unfinished tool call";
            body.insert("error", error("incomplete_tool", message));
            let kind = EnvelopeType::ACTION_CALL_FAILED;
            self.append(index, kind, body, false, reply)?;
        }
        Ok(())
    }
}
