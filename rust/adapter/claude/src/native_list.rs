use std::collections::{HashMap, HashSet};
use std::fs::{self, DirEntry, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use oxap_adapter::NativeListing;
use serde::Deserialize;
use serde_json::Value;

const REGISTRY_RECORD_BYTES: u64 = 64 * 1024;
const DESKTOP_RECORD_BYTES: u64 = 4 * 1024 * 1024;
const TRANSCRIPT_HEAD_BYTES: u64 = 256 * 1024;
const TRANSCRIPT_TAIL_BYTES: u64 = 64 * 1024;
const USER_TITLE_BYTES: usize = 120;

pub(crate) fn claude_home() -> Option<PathBuf> {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(variable)
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".claude"))
}

pub(crate) fn list(home: &Path, directory: &str, limit: usize) -> Vec<NativeListing> {
    if directory.is_empty() || limit == 0 {
        return Vec::new();
    }
    let project: String = directory
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || byte == b'-' {
                char::from(byte)
            } else {
                '-'
            }
        })
        .collect();
    let project = home.join("projects").join(project);
    let Ok(entries) = fs::read_dir(&project) else {
        return Vec::new();
    };
    let live: HashSet<_> = live_sessions(home).collect();
    let desktop = desktop_records(home);
    let mut listings: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let native_id = file_stem(&entry, ".jsonl")?;
            if desktop
                .get(&native_id)
                .is_some_and(|record| record.archived)
            {
                return None;
            }
            let updated_at_ms = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .map_or(0, unix_ms);
            let running = live.contains(&native_id);
            Some(NativeListing {
                native_id,
                directory: directory.into(),
                updated_at_ms,
                running,
                ..NativeListing::default()
            })
        })
        .collect();
    listings.sort_unstable_by(|a, b| {
        b.updated_at_ms
            .cmp(&a.updated_at_ms)
            .then_with(|| a.native_id.cmp(&b.native_id))
    });
    listings.truncate(limit);
    for row in &mut listings {
        if let Some(record) = desktop.get(&row.native_id) {
            row.title = record.title.clone();
            row.link = app_link(&record.local_id);
        }
        if row.title.is_empty() {
            row.title = transcript_title(&project.join(format!("{}.jsonl", row.native_id)))
                .unwrap_or_default();
        }
    }
    listings
}

pub(crate) fn live_sessions(home: &Path) -> impl Iterator<Item = String> {
    fs::read_dir(home.join("sessions"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(live_session)
}

#[derive(Deserialize)]
struct RegistryRecord {
    #[serde(rename = "sessionId")]
    session_id: String,
    pid: i32,
}

fn live_session(entry: DirEntry) -> Option<String> {
    if !entry.file_name().to_str()?.ends_with(".json") || !entry.file_type().ok()?.is_file() {
        return None;
    }
    let data = read_limited(&entry.path(), REGISTRY_RECORD_BYTES)?;
    let record: RegistryRecord = serde_json::from_slice(&data).ok()?;
    (record.pid > 0 && process_exists(record.pid)).then_some(record.session_id)
}

fn read_limited(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let data = read_window(&mut File::open(path).ok()?, limit + 1)?;
    (data.len() as u64 <= limit).then_some(data)
}

fn read_window(file: &mut File, bytes: u64) -> Option<Vec<u8>> {
    let mut data = Vec::new();
    file.take(bytes).read_to_end(&mut data).ok()?;
    Some(data)
}

struct DesktopRecord {
    title: String,
    archived: bool,
    local_id: String,
}

pub(crate) fn link(home: &Path, native_id: &str) -> String {
    desktop_records(home)
        .get(native_id)
        .map_or_else(String::new, |record| app_link(&record.local_id))
}

fn app_link(local_id: &str) -> String {
    match local_id {
        "" => String::new(),
        id => format!("claude://claude.ai/epitaxy/{id}"),
    }
}

fn desktop_records(claude_home: &Path) -> HashMap<String, DesktopRecord> {
    let mut records = HashMap::new();
    if !cfg!(target_os = "macos") {
        return records;
    }
    let Some(home) = claude_home.parent() else {
        return records;
    };
    let root = home.join("Library/Application Support/Claude/claude-code-sessions");
    let mut files: Vec<_> = directories(&root)
        .flat_map(|account| directories(&account))
        .flat_map(|workspace| fs::read_dir(workspace).into_iter().flatten().flatten())
        .filter(|entry| file_stem(entry, ".json").is_some())
        .map(|entry| entry.path())
        .collect();
    files.sort_unstable();
    for path in files {
        let Some(record) = read_limited(&path, DESKTOP_RECORD_BYTES)
            .and_then(|data| serde_json::from_slice::<Value>(&data).ok())
        else {
            continue;
        };
        if let Some(id) = record["cliSessionId"].as_str() {
            records.insert(
                id.into(),
                DesktopRecord {
                    title: record["title"].as_str().unwrap_or_default().into(),
                    archived: record["isArchived"].as_bool().unwrap_or_default(),
                    local_id: record["sessionId"].as_str().unwrap_or_default().into(),
                },
            );
        }
    }
    records
}

fn directories(path: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
}

fn transcript_title(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    if size > TRANSCRIPT_HEAD_BYTES {
        file.seek(SeekFrom::Start(size - TRANSCRIPT_TAIL_BYTES - 1))
            .ok()?;
        let tail = read_window(&mut file, TRANSCRIPT_TAIL_BYTES + 1)?;
        let suffix = tail.splitn(2, |&byte| byte == b'\n').nth(1);
        if let Some(title) = suffix.and_then(custom_title) {
            return Some(title);
        }
        file.rewind().ok()?;
    }
    let head = read_window(&mut file, size.min(TRANSCRIPT_HEAD_BYTES))?;
    if size <= TRANSCRIPT_HEAD_BYTES {
        return custom_title(&head).or_else(|| first_user_title(&head));
    }
    first_user_title(&head)
}

fn first_user_title(head: &[u8]) -> Option<String> {
    records(head).find_map(|row| {
        if row["type"].as_str()? != "user" {
            return None;
        }
        let content = &row["message"]["content"];
        let text = content.as_str().or_else(|| {
            content
                .as_array()?
                .iter()
                .find_map(|part| part["text"].as_str())
        })?;
        let text = without_leading_tags(text);
        if text.is_empty() || text.starts_with('<') {
            return None;
        }
        let line = text.split('\n').next()?;
        Some(line[..line.floor_char_boundary(USER_TITLE_BYTES)].to_owned())
    })
}

fn records(data: &[u8]) -> impl DoubleEndedIterator<Item = Value> + '_ {
    data.split(|&byte| byte == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
}

fn custom_title(data: &[u8]) -> Option<String> {
    records(data).rev().find_map(|row| {
        let title = row["customTitle"].as_str()?;
        (row["type"].as_str()? == "custom-title" && !title.is_empty() && title != "New session")
            .then(|| title.to_owned())
    })
}

fn without_leading_tags(text: &str) -> &str {
    let mut rest = text.trim_matches([' ', '\t', '\r', '\n']);
    while let Some(tag) = rest.strip_prefix('<') {
        let Some(end) = tag.find(['>', ' ', '\n']) else {
            break;
        };
        let name = &tag[..end];
        if name.is_empty() || name.starts_with('/') || name.len() + 3 > 128 {
            break;
        }
        let Some((_, after)) = tag.split_once(&format!("</{name}>")) else {
            break;
        };
        rest = after.trim_matches([' ', '\t', '\r', '\n']);
    }
    rest
}

fn file_stem(entry: &DirEntry, suffix: &str) -> Option<String> {
    if !entry.file_type().ok()?.is_file() {
        return None;
    }
    entry
        .file_name()
        .to_str()?
        .strip_suffix(suffix)
        .map(str::to_owned)
}

fn unix_ms(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as i64,
        Err(before) => -(before.duration().as_nanos().div_ceil(1_000_000) as i64),
    }
}

#[cfg(unix)]
fn process_exists(raw: i32) -> bool {
    use rustix::{io::Errno, process};
    let Some(pid) = process::Pid::from_raw(raw) else {
        return false;
    };
    matches!(process::test_kill_process(pid), Ok(()) | Err(Errno::PERM))
}

#[cfg(not(unix))]
fn process_exists(_: i32) -> bool {
    true
}
