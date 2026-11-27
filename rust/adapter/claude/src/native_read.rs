use std::fs::{self, File};
use std::io::{Seek, SeekFrom};
use std::path::Path;

use oxap_adapter::{gotime::rfc3339_millis, NativeReadRequest, NativeTurn};
use serde_json::Value;

use crate::native_list::{claude_home, directories, project_key, read_window, records};

const SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
const WRAPPERS: &[&str] = &[
    "command-name",
    "command-message",
    "command-args",
    "local-command-stdout",
    "local-command-stderr",
    "local-command-caveat",
    "system-reminder",
    "bash-input",
    "bash-stdout",
    "bash-stderr",
];

pub(crate) fn read(directory: &str, request: &NativeReadRequest) -> Vec<NativeTurn> {
    let native_id = &request.native_id;
    let safe = !native_id.is_empty()
        && native_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    let Some(home) = claude_home().filter(|_| safe) else {
        return Vec::new();
    };
    let name = format!("{native_id}.jsonl");
    let projects = home.join("projects");
    let preferred = projects.join(project_key(directory));
    if !directory.is_empty() && fs::symlink_metadata(&preferred).is_ok_and(|info| info.is_dir()) {
        if let Some(turns) = snapshot(&preferred.join(&name), request.max_turns) {
            return turns;
        }
    }
    let mut others: Vec<_> = directories(&projects).collect();
    others.sort_unstable();
    others
        .into_iter()
        .find_map(|project| snapshot(&project.join(&name), request.max_turns))
        .unwrap_or_default()
}

fn snapshot(path: &Path, max_turns: usize) -> Option<Vec<NativeTurn>> {
    if !fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let start = size.saturating_sub(SNAPSHOT_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let data = read_window(&mut file, size - start)?;
    let complete = if start > 0 {
        data.splitn(2, |&byte| byte == b'\n')
            .nth(1)
            .unwrap_or_default()
    } else {
        &data
    };
    let lines = complete
        .rsplitn(2, |&byte| byte == b'\n')
        .nth(1)
        .unwrap_or_default();
    Some(turns(lines, max_turns))
}

fn turns(data: &[u8], max_turns: usize) -> Vec<NativeTurn> {
    let mut found = Vec::new();
    let mut reply: Option<(String, NativeTurn)> = None;
    for row in records(data) {
        if row["isSidechain"] == true || row["isMeta"] == true {
            continue;
        }
        let message = &row["message"];
        let at_ms = rfc3339_millis(row["timestamp"].as_str().unwrap_or_default());
        match row["type"].as_str() {
            Some("user") => {
                let joined = text(&message["content"], "\n");
                let said = strip_wrappers(&joined);
                if said.is_empty() {
                    continue;
                }
                found.extend(reply.take().map(|(_, turn)| turn));
                found.push(turn("user", said.into(), at_ms));
                if max_turns != 0 && found.len() >= max_turns {
                    found.truncate(max_turns);
                    return found;
                }
            }
            Some("assistant") => {
                let piece = text(&message["content"], "");
                if piece.is_empty() {
                    continue;
                }
                let id = message["id"].as_str().unwrap_or_default();
                match reply.as_mut() {
                    Some((previous_id, turn)) if !id.is_empty() && id == previous_id => {
                        turn.text.push_str(&piece);
                        turn.at_ms = at_ms;
                    }
                    _ => reply = Some((id.into(), turn("assistant", piece, at_ms))),
                }
            }
            _ => {}
        }
    }
    found.extend(reply.map(|(_, turn)| turn));
    found
}

fn turn(role: &str, text: String, at_ms: i64) -> NativeTurn {
    NativeTurn {
        role: role.into(),
        text,
        at_ms,
    }
}

fn text(content: &Value, separator: &str) -> String {
    if let Some(plain) = content.as_str() {
        return plain.to_owned();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| part["type"] == "text")
        .filter_map(|part| part["text"].as_str())
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<_>>()
        .join(separator)
}

fn strip_wrappers(text: &str) -> &str {
    let trim = |byte| matches!(byte, ' ' | '\t' | '\r' | '\n');
    let mut rest = text.trim_matches(trim);
    while let Some((tag, content)) = rest.strip_prefix('<').and_then(|rest| rest.split_once('>')) {
        if !WRAPPERS.contains(&tag) {
            break;
        }
        let Some((_, after)) = content.split_once(&format!("</{tag}>")) else {
            break;
        };
        rest = after.trim_matches(trim);
    }
    rest
}
