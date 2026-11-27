#![cfg(unix)]

use std::fs::{self, File, FileTimes};
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use futures::executor::block_on;
use oxap_adapter::{Adapter as _, Error, NativeListRequest, OpenRequest};
use oxap_adapter_claude::{Adapter, Config, ToolPosture};
use serde_json::json;

const WORKSPACE: &str = "/example/work.é";
const LIVE: &str = "11111111-1111-4111-8111-111111111111";
const LIVE_WITHOUT_TRANSCRIPT: &str = "22222222-2222-4222-8222-222222222222";

#[test]
fn claude_lists_workspace_transcripts_and_refuses_live_adoption() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "observe_native_list", "--nocapture"])
        .env_clear()
        .env("HOME", home.path())
        .env("NODE_ENV", "test")
        .output()
        .unwrap();
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "listing consumer: {}",
        output.status
    );
}

#[test]
#[ignore]
fn observe_native_list() {
    assert_eq!(std::env::var("NODE_ENV").as_deref(), Ok("test"));
    let home = std::env::var_os("HOME").unwrap();
    let home = Path::new(&home);
    assert!(fs::read_dir(home).unwrap().next().is_none());
    let store = home.join(".claude");
    let project = store.join("projects/-example-work---");
    let other = store.join("projects/-other");
    let registry = store.join("sessions");
    for dir in [&project, &other, &registry] {
        fs::create_dir_all(dir).unwrap();
    }
    for (file, time) in [
        ("stale.jsonl", 30),
        ("beta.jsonl", 20),
        ("alpha.jsonl", 20),
        ("old.jsonl", 10),
        ("notes.txt", 40),
    ] {
        transcript(&project.join(file), time);
    }
    transcript(&project.join(format!("{LIVE}.jsonl")), 50);
    transcript(&other.join("unrelated.jsonl"), 60);
    fs::create_dir(project.join("folder.jsonl")).unwrap();
    symlink(other.join("unrelated.jsonl"), project.join("linked.jsonl")).unwrap();
    for (file, session_id, pid) in [
        ("live.json", LIVE, json!(std::process::id())),
        (
            "not-written.json",
            LIVE_WITHOUT_TRANSCRIPT,
            json!(std::process::id()),
        ),
        ("dead.json", "stale", json!(i32::MAX)),
        ("group.json", "beta", json!(-1)),
        (
            "malformed.json",
            "alpha",
            json!(std::process::id().to_string()),
        ),
    ] {
        fs::write(
            registry.join(file),
            json!({"sessionId": session_id, "pid": pid}).to_string(),
        )
        .unwrap();
    }
    let mut oversized = json!({"sessionId": "old", "pid": std::process::id()}).to_string();
    oversized.push_str(&" ".repeat(64 * 1024));
    fs::write(registry.join("oversized.json"), oversized).unwrap();

    let adapter = Adapter::new(Config {
        working_directory: Some(WORKSPACE.into()),
        tools: ToolPosture::Unrestricted,
        ..Config::default()
    })
    .unwrap();
    let lister = adapter
        .native_lister()
        .expect("public native session lister");
    let list = |directory: &str, limit| {
        block_on(lister.native_list(NativeListRequest {
            directory: directory.into(),
            limit,
        }))
        .unwrap()
    };
    let rows = list("", 50);
    let observed: Vec<_> = rows
        .iter()
        .map(|entry| (entry.native_id.as_str(), entry.updated_at_ms, entry.running))
        .collect();
    println!("workspace rows (id, updated ms, running) = {observed:?}");
    assert_eq!(
        observed,
        [
            (LIVE, 50_000, true),
            ("stale", 30_000, false),
            ("alpha", 20_000, false),
            ("beta", 20_000, false),
            ("old", 10_000, false),
        ]
    );
    assert!(rows.iter().all(|row| row.directory == WORKSPACE));
    assert_eq!(list("", 2), rows[..2]);
    assert!(list("", 0).is_empty());
    assert!(list("/missing", 3).is_empty());
    let other = list("/other", 3);
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].directory, "/other");
    assert_eq!(other[0].native_id, "unrelated");
    for native_session_id in [LIVE, LIVE_WITHOUT_TRANSCRIPT] {
        let opened = block_on(adapter.open(OpenRequest {
            reopen: true,
            adopted: true,
            native_session_id: native_session_id.into(),
            ..OpenRequest::default()
        }));
        assert!(matches!(opened, Err(Error::RunActive)));
    }
    println!("live adoption refused for both a listed and a not-yet-written transcript");

    fs::remove_file(registry.join("live.json")).unwrap();
    assert!(!list("", 1)[0].running);
    println!("without a live registry record, the first transcript is idle");
}

fn transcript(path: &Path, seconds: u64) {
    File::create(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
        .unwrap();
}
