use serde_json::value::RawValue;

use oxap_adapter::gojson::{go_quote, reject_duplicate_keys};

use super::Error;

pub const TYPE_CONTROL_REQUEST: &str = "control_request";
pub const TYPE_CONTROL_RESPONSE: &str = "control_response";
pub const TYPE_CONTROL_CANCEL: &str = "control_cancel_request";
pub const TYPE_USER: &str = "user";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Observation,
    ControlRequest,
    ControlResponse,
    ControlCancel,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ControlResponseEnvelope {
    pub success: bool,
    pub request_id: String,
    pub response: Option<Vec<u8>>,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub kind: Kind,
    pub message_type: String,
    pub subtype: String,
    pub request_id: String,
    pub response: Option<ControlResponseEnvelope>,
    pub raw: Vec<u8>,
}

pub fn user_turn_message(frame: &[u8]) -> Message {
    Message {
        kind: Kind::Observation,
        message_type: TYPE_USER.to_owned(),
        subtype: String::new(),
        request_id: String::new(),
        response: None,
        raw: frame.to_vec(),
    }
}

pub fn control_request_message(request_id: &str, request: &[u8]) -> Message {
    Message {
        kind: Kind::ControlRequest,
        message_type: String::new(),
        subtype: String::new(),
        request_id: request_id.to_owned(),
        response: None,
        raw: request.to_vec(),
    }
}

pub fn control_response_message(response: &[u8]) -> Message {
    Message {
        kind: Kind::ControlResponse,
        message_type: String::new(),
        subtype: String::new(),
        request_id: String::new(),
        response: Some(ControlResponseEnvelope::default()),
        raw: response.to_vec(),
    }
}

pub(super) fn parse_object(data: &[u8]) -> Result<Vec<(String, Box<RawValue>)>, Error> {
    if data.is_empty() || data[0] != b'{' || data[data.len() - 1] != b'}' {
        return Err(Error::invalid("frame must be exactly one JSON object"));
    }
    reject_duplicate_keys(data).map_err(|error| Error::invalid(error.to_string()))?;
    let text = std::str::from_utf8(data).map_err(|_| Error::invalid("frame is not UTF-8"))?;
    crate::native::source::members(text).map_err(|_| Error::invalid("expected one object"))
}

fn member<'a>(object: &'a [(String, Box<RawValue>)], name: &str) -> Option<&'a str> {
    object
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.get())
}

fn string_member(object: &[(String, Box<RawValue>)], name: &str) -> Option<String> {
    crate::native::source::decode(member(object, name)?).ok()
}

fn require_object(
    object: &[(String, Box<RawValue>)],
    name: &str,
) -> Result<Vec<(String, Box<RawValue>)>, Error> {
    let raw = member(object, name).ok_or_else(|| Error::control(format!("{name} is required")))?;
    parse_object(raw.as_bytes())
        .map_err(|error| Error::control(format!("{name} must be one object: {error}")))
}

pub fn parse_message(data: &[u8]) -> Result<Message, Error> {
    let object = parse_object(data)?;
    let mut message = Message {
        kind: Kind::Observation,
        message_type: String::new(),
        subtype: String::new(),
        request_id: String::new(),
        response: None,
        raw: data.to_vec(),
    };
    if member(&object, "type").is_none() {
        return Err(Error::invalid("type is required"));
    }
    message.message_type = match string_member(&object, "type") {
        Some(name) if !name.is_empty() => name,
        _ => return Err(Error::invalid("type must be a non-empty string")),
    };
    if member(&object, "subtype").is_some() {
        message.subtype = match string_member(&object, "subtype") {
            Some(name) if !name.is_empty() => name,
            _ => return Err(Error::invalid("subtype must be a non-empty string")),
        };
    }
    match message.message_type.as_str() {
        TYPE_CONTROL_REQUEST => {
            message.kind = Kind::ControlRequest;
            message.request_id = match string_member(&object, "request_id") {
                Some(id) if !id.is_empty() => id,
                _ => {
                    return Err(Error::control(
                        "control_request requires a non-empty request_id",
                    ))
                }
            };
            let request = require_object(&object, "request")?;
            message.subtype = match string_member(&request, "subtype") {
                Some(name) if !name.is_empty() => name,
                _ => {
                    return Err(Error::control(
                        "control request requires a non-empty subtype",
                    ))
                }
            };
        }
        TYPE_CONTROL_RESPONSE => {
            message.kind = Kind::ControlResponse;
            let response = require_object(&object, "response")?;
            let mut envelope = ControlResponseEnvelope::default();
            let state = match string_member(&response, "subtype") {
                Some(name) if !name.is_empty() => name,
                _ => {
                    return Err(Error::control(
                        "control response requires a non-empty subtype",
                    ))
                }
            };
            envelope.request_id = match string_member(&response, "request_id") {
                Some(id) if !id.is_empty() => id,
                _ => {
                    return Err(Error::control(
                        "control response requires a non-empty request_id",
                    ))
                }
            };
            match state.as_str() {
                "success" => {
                    envelope.success = true;
                    if let Some(payload) = member(&response, "response") {
                        let payload = payload.trim();
                        if payload != "null" {
                            envelope.response = Some(payload.as_bytes().to_vec());
                        }
                    }
                }
                "error" => match string_member(&response, "error") {
                    Some(detail) if !detail.is_empty() => envelope.error = detail,
                    _ => {
                        return Err(Error::control(
                            "error control response requires a non-empty error",
                        ))
                    }
                },
                other => {
                    return Err(Error::control(format!(
                        "control response subtype {} is not success or error",
                        go_quote(other)
                    )))
                }
            }
            message.response = Some(envelope);
        }
        TYPE_CONTROL_CANCEL => {
            message.kind = Kind::ControlCancel;
            message.request_id = match string_member(&object, "request_id") {
                Some(id) if !id.is_empty() => id,
                _ => {
                    return Err(Error::control(
                        "control_cancel_request requires a non-empty request_id",
                    ))
                }
            };
        }
        _ => {}
    }
    Ok(message)
}

pub(super) fn go_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            other if (other as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", other as u32)),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

pub(super) fn compact(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut quoted = false;
    let mut at = 0;
    while at < raw.len() {
        let byte = raw[at];
        if !quoted && matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
            at += 1;
            continue;
        }
        match byte {
            b'"' => {
                quoted = !quoted;
                out.push(byte);
                at += 1;
            }
            b'\\' => {
                let width = 2.min(raw.len() - at);
                out.extend_from_slice(&raw[at..at + width]);
                at += width;
            }
            b'<' => {
                out.extend_from_slice(b"\\u003c");
                at += 1;
            }
            b'>' => {
                out.extend_from_slice(b"\\u003e");
                at += 1;
            }
            b'&' => {
                out.extend_from_slice(b"\\u0026");
                at += 1;
            }
            0xe2 if raw.get(at + 1) == Some(&0x80)
                && raw.get(at + 2).is_some_and(|next| next & !1 == 0xa8) =>
            {
                out.extend_from_slice(if raw[at + 2] == 0xa8 {
                    b"\\u2028".as_slice()
                } else {
                    b"\\u2029".as_slice()
                });
                at += 3;
            }
            _ => {
                out.push(byte);
                at += 1;
            }
        }
    }
    out
}

fn written_value(raw: &[u8]) -> String {
    String::from_utf8_lossy(&compact(raw)).into_owned()
}

fn write_sorted_object(object: &[(String, Box<RawValue>)]) -> String {
    let mut sorted: Vec<&(String, Box<RawValue>)> = object.iter().collect();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    let body: Vec<String> = sorted
        .into_iter()
        .map(|(key, value)| {
            format!(
                "{}:{}",
                go_string(key),
                written_value(value.get().as_bytes())
            )
        })
        .collect();
    format!("{{{}}}", body.join(","))
}

fn write_frame(members: Vec<(&str, String)>) -> String {
    let mut sorted = members;
    sorted.sort_by(|left, right| left.0.cmp(right.0));
    let body: Vec<String> = sorted
        .into_iter()
        .map(|(key, value)| format!("{}:{value}", go_string(key)))
        .collect();
    format!("{{{}}}", body.join(","))
}

fn observation_type(message: &Message) -> Result<(), Error> {
    if message.message_type == TYPE_USER {
        return Ok(());
    }
    Err(Error::invalid(
        "only user frames are written as observations",
    ))
}

impl Message {
    pub fn marshal_json(&self) -> Result<String, Error> {
        let refuse = || Error::invalid("");
        match self.kind {
            Kind::ControlRequest => {
                if self.request_id.is_empty() || self.raw.is_empty() {
                    return Err(refuse());
                }
                let request = parse_object(&self.raw)?;
                if member(&request, "subtype").is_none() {
                    return Err(Error::control("control request requires a subtype"));
                }
                Ok(write_frame(vec![
                    ("type", go_string(TYPE_CONTROL_REQUEST)),
                    ("request_id", go_string(&self.request_id)),
                    ("request", write_sorted_object(&request)),
                ]))
            }
            Kind::ControlResponse => {
                if self.raw.is_empty() {
                    return Err(refuse());
                }
                let response = parse_object(&self.raw)?;
                match string_member(&response, "subtype").as_deref() {
                    Some("success" | "error") => {}
                    _ => {
                        return Err(Error::control(
                            "control response subtype must be success or error",
                        ))
                    }
                }
                match string_member(&response, "request_id") {
                    Some(id) if !id.is_empty() => {}
                    _ => {
                        return Err(Error::control(
                            "control response requires a non-empty request_id",
                        ))
                    }
                }
                Ok(write_frame(vec![
                    ("type", go_string(TYPE_CONTROL_RESPONSE)),
                    ("response", write_sorted_object(&response)),
                ]))
            }
            Kind::Observation => {
                observation_type(self)?;
                let frame = parse_object(&self.raw)?;
                if member(&frame, "message").is_none() {
                    return Err(Error::invalid("user frame requires a message"));
                }
                Ok(write_sorted_object(&frame))
            }
            Kind::ControlCancel => Err(refuse()),
        }
    }
}
