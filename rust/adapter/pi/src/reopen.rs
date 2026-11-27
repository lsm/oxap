use std::fs::File;
use std::io::Read;
use std::path::Path;

use oxap_adapter::gojson::{reject_duplicate_keys, replace_lone_surrogates};
use oxap_adapter::{Error, UnsupportedControl, CONTROL_UNSATISFIABLE};
use oxap_protocol::FEATURE_OPEN_REOPEN;
use serde_json::value::RawValue;

use crate::native::Members;

const HEADER_LIMIT: usize = 64 << 10;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionBinding {
    pub session_id: String,
    pub session_file: String,
}

pub fn read_binding(raw: &str) -> Result<SessionBinding, Error> {
    load(raw).map_err(|detail| {
        Error::UnsupportedControl(Box::new(UnsupportedControl {
            feature: FEATURE_OPEN_REOPEN.to_owned(),
            reason: CONTROL_UNSATISFIABLE,
            detail: format!("Pi could not load its binding: {detail}"),
            ..UnsupportedControl::default()
        }))
    })
}

fn load(raw: &str) -> Result<SessionBinding, String> {
    let mut binding = SessionBinding::default();
    for (name, value) in members(raw)? {
        let slot = if name.eq_ignore_ascii_case("sessionId") {
            &mut binding.session_id
        } else if name.eq_ignore_ascii_case("sessionFile") {
            &mut binding.session_file
        } else {
            return Err(format!("unknown field {name:?}"));
        };
        assign(slot, &value)?;
    }
    if binding.session_id.is_empty() || !Path::new(&binding.session_file).is_absolute() {
        return Err("binding omitted Pi's session id or absolute file".into());
    }
    let regular = |metadata: std::io::Result<std::fs::Metadata>| match metadata {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err("bound session file is not a regular file".to_owned()),
        Err(failure) => Err(failure.to_string()),
    };
    regular(std::fs::metadata(&binding.session_file))?;
    let file = File::open(&binding.session_file).map_err(|failure| failure.to_string())?;
    regular(file.metadata())?;
    let mut head = Vec::new();
    file.take(HEADER_LIMIT as u64 + 1)
        .read_to_end(&mut head)
        .map_err(|failure| failure.to_string())?;
    let line = match head.iter().position(|byte| *byte == b'\n') {
        Some(end) => &head[..end],
        None if head.len() <= HEADER_LIMIT => &head[..],
        None => return Err("bound session file has no readable header".into()),
    };
    let (mut kind, mut id) = (String::new(), String::new());
    for (name, value) in members(&per_byte_lossy(line))? {
        if name.eq_ignore_ascii_case("type") {
            assign(&mut kind, &value)?;
        } else if name.eq_ignore_ascii_case("id") {
            assign(&mut id, &value)?;
        }
    }
    if kind != "session" || id != binding.session_id {
        return Err("bound session header does not match its recorded id".into());
    }
    Ok(binding)
}

fn members(data: &str) -> Result<Vec<(String, Box<RawValue>)>, String> {
    reject_duplicate_keys(data.as_bytes()).map_err(|failure| failure.to_string())?;
    let Members(pairs) = serde_json::from_str(&replace_lone_surrogates(data))
        .map_err(|failure| failure.to_string())?;
    Ok(pairs)
}

fn assign(slot: &mut String, value: &RawValue) -> Result<(), String> {
    if let Some(text) = serde_json::from_str::<Option<String>>(value.get())
        .map_err(|failure| failure.to_string())?
    {
        *slot = text;
    }
    Ok(())
}

fn per_byte_lossy(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        text.push_str(chunk.valid());
        text.extend(std::iter::repeat_n('\u{FFFD}', chunk.invalid().len()));
    }
    text
}
