use std::time::Instant;

use oxap_adapter::Error;
use oxap_protocol::{ContentPart, ContentPartType, ProtocolError, Usage};

use crate::native::{
    decode_strict, DeltaPayload, MessageCompletePayload, Usage as NativeUsage,
    EVENT_MESSAGE_COMPLETE, EVENT_MESSAGE_DELTA, EVENT_MESSAGE_START, EVENT_REASONING_DELTA,
};
use crate::{GatewaySession, Observation};

#[derive(Debug)]
pub enum PromptObservation {
    Delta(ContentPart),
    Completed { text: String, usage: Option<Usage> },
    Failed(ProtocolError),
    Other(Observation),
}

impl GatewaySession {
    pub fn next_prompt_observation(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<PromptObservation, Error> {
        loop {
            let observation = self.next_observation(deadline)?;
            let Observation::Event(event) = &observation else {
                return Ok(PromptObservation::Other(observation));
            };
            match event.kind.as_str() {
                EVENT_MESSAGE_DELTA | EVENT_REASONING_DELTA => {
                    let delta: DeltaPayload =
                        decode_strict(event.payload_bytes()).map_err(Error::harness)?;
                    if delta.text.is_empty() {
                        continue;
                    }
                    let part = if event.kind == EVENT_MESSAGE_DELTA {
                        ContentPart {
                            part_type: ContentPartType::TEXT,
                            text: delta.text,
                            ..ContentPart::default()
                        }
                    } else {
                        ContentPart {
                            part_type: ContentPartType::REASONING,
                            reasoning: delta.text,
                            ..ContentPart::default()
                        }
                    };
                    return Ok(PromptObservation::Delta(part));
                }
                EVENT_MESSAGE_COMPLETE => {
                    let payload = decode_strict(event.payload_bytes()).map_err(Error::harness)?;
                    return Ok(project_settlement(payload));
                }
                EVENT_MESSAGE_START => {
                    return Ok(failed("hermes_invalid_grammar", "turn opened twice"));
                }
                _ => return Ok(PromptObservation::Other(observation)),
            }
        }
    }
}

fn project_settlement(payload: MessageCompletePayload) -> PromptObservation {
    match payload.status.as_str() {
        "" => failed(
            "hermes_invalid_settlement",
            "child-mirror settlement on the parent stream",
        ),
        "complete" => match payload.usage.map(project_usage).transpose() {
            Ok(usage) => PromptObservation::Completed {
                text: payload.text,
                usage,
            },
            Err(_) => failed("hermes_invalid_event", "invalid settlement"),
        },
        status => {
            let code = payload
                .error_surface
                .as_ref()
                .map(|surface| surface.code.as_str())
                .filter(|code| !code.is_empty())
                .unwrap_or(status);
            let message = if !payload.error.is_empty() {
                payload.error
            } else if !payload.text.is_empty() {
                payload.text
            } else {
                format!("turn {status}")
            };
            failed(format!("hermes_{code}"), message)
        }
    }
}

fn project_usage(native: NativeUsage) -> Result<Usage, std::num::TryFromIntError> {
    let count = |value: Option<i64>| value.map(u64::try_from).transpose();
    Ok(Usage {
        input_tokens: count(native.input)?,
        output_tokens: count(native.output)?,
        total_tokens: count(native.total)?,
        ..Usage::default()
    })
}

fn failed(code: impl Into<String>, message: impl Into<String>) -> PromptObservation {
    PromptObservation::Failed(ProtocolError {
        code: code.into(),
        message: message.into(),
        ..ProtocolError::default()
    })
}
