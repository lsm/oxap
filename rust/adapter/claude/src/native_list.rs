use std::collections::HashSet;
use std::fs::{self, DirEntry, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use oxap_adapter::NativeListing;
use serde::Deserialize;

const REGISTRY_RECORD_BYTES: u64 = 64 * 1024;

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
    let Ok(entries) = fs::read_dir(home.join("projects").join(project)) else {
        return Vec::new();
    };
    let live: HashSet<_> = live_sessions(home).collect();
    let mut listings: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let native_id = file_stem(&entry, ".jsonl")?;
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
    let mut data = Vec::new();
    File::open(entry.path())
        .ok()?
        .take(REGISTRY_RECORD_BYTES + 1)
        .read_to_end(&mut data)
        .ok()?;
    if data.len() as u64 > REGISTRY_RECORD_BYTES {
        return None;
    }
    let record: RegistryRecord = serde_json::from_slice(&data).ok()?;
    (record.pid > 0 && process_exists(record.pid)).then_some(record.session_id)
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
