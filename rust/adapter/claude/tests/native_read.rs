#![cfg(unix)]

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command;

use futures::executor::block_on;
use oxap_adapter::{Adapter as _, NativeReadRequest};
use oxap_adapter_claude::{Adapter, Config, ToolPosture};

const WORKSPACE: &str = "/example/work.é";

#[test]
fn reads_complete_native_turns_in_transcript_order() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "observe_native_turns",
            "--nocapture",
        ])
        .env_clear()
        .env("HOME", home.path())
        .env("NODE_ENV", "test")
        .output()
        .unwrap();
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "native reader consumer: {}",
        output.status
    );
}

#[test]
#[ignore]
fn observe_native_turns() {
    assert_eq!(std::env::var("NODE_ENV").as_deref(), Ok("test"));
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    assert!(fs::read_dir(&home).unwrap().next().is_none());
    let root = home.join(".claude/projects");
    let project = root.join("-example-work---");
    let fallback = root.join("-elsewhere");
    let later = root.join("-later");
    for dir in [&project, &fallback, &later] {
        fs::create_dir_all(dir).unwrap();
    }
    let rows = [
        r#"{"type":"user","timestamp":"2026-10-06T16:12:33.250-04:00","message":{"content":"<command-name>/prompt</command-name> Hello"}}"#,
        r#"{"type":"user","isMeta":true,"message":{"content":"internal"}}"#,
        r#"{"type":"assistant","message":{"id":"old","content":"interim"}}"#,
        r#"{"type":"assistant","message":{"id":"m","content":[{"type":"thinking","thinking":"hidden"},{"type":"text","text":"Good"}]}}"#,
        r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"tool output"}]}}"#,
        r#"{"type":"assistant","timestamp":"2026-10-06T20:12:34Z","message":{"id":"m","content":[{"type":"text","text":" morning"},{"type":"tool_use","name":"hidden"}]}}"#,
        r#"{"type":"assistant","isSidechain":true,"message":{"content":"child"}}"#,
        r#"{"type":"user","message":{"content":"<local-command-stdout>noise</local-command-stdout>"}}"#,
        "{unparseable}",
        r#"{"type":"user","timestamp":"2026-10-06T20:12:31Z","message":{"content":[{"type":"text","text":""},{"type":"text","text":"<div>HTML</div>"},{"type":"image"},{"type":"text","text":"More"}]}}"#,
        r#"{"type":"assistant","message":{"content":"Final"}}"#,
        r#"{"type":"user","message":{"content":"not committed to a full line"}}"#,
    ];
    fs::write(project.join("session_1.jsonl"), rows.join("\n")).unwrap();
    fs::write(fallback.join("session_1.jsonl"), user("first fallback")).unwrap();
    fs::write(later.join("session_1.jsonl"), user("later fallback")).unwrap();
    fs::write(
        fallback.join("escape.jsonl"),
        user("must stay inside project"),
    )
    .unwrap();
    symlink(fallback.join("escape.jsonl"), project.join("linked.jsonl")).unwrap();

    let mut bounded = File::create(project.join("bounded.jsonl")).unwrap();
    let old = user("outside snapshot");
    bounded.write_all(old.as_bytes()).unwrap();
    bounded.write_all(b"x").unwrap();
    bounded
        .write_all(user("clipped first line").as_bytes())
        .unwrap();
    bounded
        .write_all(user("complete line in tail").as_bytes())
        .unwrap();
    bounded
        .set_len(old.len() as u64 + 1 + 64 * 1024 * 1024)
        .unwrap();
    drop(bounded);

    let adapter = Adapter::new(Config {
        working_directory: Some(WORKSPACE.into()),
        tools: ToolPosture::Unrestricted,
        ..Config::default()
    })
    .unwrap();
    let reader = adapter
        .native_reader()
        .expect("public native transcript reader");
    let read = |id: &str, dir: &str, max_turns| {
        block_on(reader.native_read(NativeReadRequest {
            native_id: id.into(),
            directory: dir.into(),
            max_turns,
        }))
        .unwrap()
        .into_iter()
        .map(|turn| (turn.role, turn.text, turn.at_ms))
        .collect::<Vec<_>>()
    };
    let turns = read("session_1", "", 0);
    println!("Native turns in file order: {turns:?}");
    assert_eq!(
        turns,
        [
            turn("user", "Hello", 1791317553250),
            turn("assistant", "Good morning", 1791317554000),
            turn("user", "<div>HTML</div>\nMore", 1791317551000),
            turn("assistant", "Final", 0),
        ]
    );
    assert_eq!(read("session_1", "", 2), turns[..2]);
    assert_eq!(
        read("session_1", "/later", 0),
        [turn("user", "later fallback", 0)]
    );
    assert_eq!(
        read("session_1", "/absent", 0),
        [turn("user", "first fallback", 0)]
    );
    for id in ["", "absent", "../-elsewhere/escape", "linked"] {
        assert!(read(id, "", 0).is_empty(), "{id}");
    }
    let tail = read("bounded", "", 0);
    println!("Oversized snapshot returned: {tail:?}");
    assert_eq!(tail, [turn("user", "complete line in tail", 0)]);
}

fn user(text: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"type": "user", "message": {"content": text}})
    )
}

fn turn(role: &str, text: &str, at_ms: i64) -> (String, String, i64) {
    (role.to_owned(), text.to_owned(), at_ms)
}
