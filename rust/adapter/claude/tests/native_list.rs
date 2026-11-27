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

#[test]
fn claude_projects_native_session_metadata() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "observe_native_metadata",
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
        "metadata consumer: {}",
        output.status
    );
}

#[test]
#[ignore]
fn observe_native_metadata() {
    assert_eq!(std::env::var("NODE_ENV").as_deref(), Ok("test"));
    let home = std::env::var_os("HOME").unwrap();
    let home = Path::new(&home);
    assert!(fs::read_dir(home).unwrap().next().is_none());
    let project = home.join(".claude/projects/-example-work---");
    let desktop =
        home.join("Library/Application Support/Claude/claude-code-sessions/account/workspace");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&desktop).unwrap();

    let custom = |title: &str| json!({"type":"custom-title","customTitle":title});
    let user = |text: &str| json!({"type":"user","message":{"content":text}});
    let listed = [
        ("desktop", format!("{}\n", custom("Transcript title"))),
        ("archived", format!("{}\n", user("Archived transcript"))),
        (
            "short",
            format!(
                "{}\n{}\n{}\n",
                custom("Earlier"),
                custom("Latest"),
                custom("New session")
            ),
        ),
        (
            "tail",
            format!(
                "{}\n{}\n{}\n",
                user("Head fallback"),
                "x".repeat(270_000),
                custom("Tail title")
            ),
        ),
        (
            "head",
            format!(
                "{}\n{}\n{}\n",
                user("Head fallback"),
                custom("Outside the tail"),
                "x".repeat(270_000)
            ),
        ),
        (
            "first",
            format!(
                "{}\n{}\n{}\n",
                user("<command-name>/init</command-name>"),
                user("<unclosed"),
                json!({"type":"user","message":{"content":[{"type":"image"},{"type":"text","text":"<system-reminder>noise</system-reminder>\nfix the parser\nplease"}]}})
            ),
        ),
        (
            "utf8",
            format!("{}\n", user(&format!("{}é!", "a".repeat(119)))),
        ),
        ("empty", "invalid JSON".into()),
    ];
    for (id, body) in listed {
        fs::write(project.join(format!("{id}.jsonl")), body).unwrap();
    }
    fs::write(
        desktop.join("a.json"),
        json!({"cliSessionId":"desktop","sessionId":"old","title":"Old name"}).to_string(),
    )
    .unwrap();
    fs::write(
        desktop.join("z.json"),
        json!({"cliSessionId":"desktop","sessionId":"local_desktop","title":"Desktop title"})
            .to_string(),
    )
    .unwrap();
    fs::write(
        desktop.join("archived.json"),
        json!({"cliSessionId":"archived","sessionId":"local_archived","isArchived":true})
            .to_string(),
    )
    .unwrap();
    fs::write(
        desktop.join("short.json"),
        json!({"cliSessionId":"short","sessionId":"local_short","title":""}).to_string(),
    )
    .unwrap();
    fs::write(
        desktop.join("first.json"),
        format!(
            "{}{}",
            json!({"cliSessionId":"first","title":"Oversized"}),
            " ".repeat(4 * 1024 * 1024)
        ),
    )
    .unwrap();
    symlink(desktop.join("z.json"), desktop.join("zz.json")).unwrap();

    let adapter = Adapter::new(Config {
        tools: ToolPosture::Unrestricted,
        ..Config::default()
    })
    .unwrap();
    let rows = block_on(
        adapter
            .native_lister()
            .unwrap()
            .native_list(NativeListRequest {
                directory: WORKSPACE.into(),
                limit: 30,
            }),
    )
    .unwrap();
    let display = |id| {
        let row = rows.iter().find(|row| row.native_id == id).unwrap();
        (row.title.as_str(), row.link.as_str())
    };
    let observations: Vec<_> = rows
        .iter()
        .map(|row| (&row.native_id, &row.title, &row.link))
        .collect();
    println!(
        "{} native rows (id, title, link) = {observations:?}",
        rows.len()
    );
    assert_eq!(display("tail"), ("Tail title", ""));
    assert_eq!(display("head"), ("Head fallback", ""));
    assert_eq!(display("first"), ("fix the parser", ""));
    assert_eq!(display("utf8"), ("a".repeat(119).as_str(), ""));
    assert_eq!(display("empty"), ("", ""));
    if cfg!(target_os = "macos") {
        let link = "claude://claude.ai/epitaxy/local_desktop";
        assert_eq!(rows.len(), 7);
        assert_eq!(display("desktop"), ("Desktop title", link));
        assert_eq!(
            display("short"),
            ("Latest", "claude://claude.ai/epitaxy/local_short")
        );
        assert!(!rows.iter().any(|row| row.native_id == "archived"));
        let linker = adapter.native_linker().unwrap();
        assert_eq!(linker.native_link("desktop"), link);
        assert_eq!(
            linker.native_link("archived"),
            "claude://claude.ai/epitaxy/local_archived"
        );
        assert_eq!(linker.native_link("unknown"), "");
        println!("standalone links resolve known desktop IDs, including archived sessions");
        fs::write(
            desktop.join("z.json"),
            json!({"cliSessionId":"desktop","sessionId":"local_updated","title":"Updated title"})
                .to_string(),
        )
        .unwrap();
        let cached = linker.native_link("desktop");
        println!("standalone link after Desktop record changed = {cached}");
        assert_eq!(cached, link);
        let fresh = block_on(
            adapter
                .native_lister()
                .unwrap()
                .native_list(NativeListRequest {
                    directory: WORKSPACE.into(),
                    limit: 30,
                }),
        )
        .unwrap();
        assert!(fresh.iter().any(|row| row.native_id == "desktop"
            && row.link == "claude://claude.ai/epitaxy/local_updated"
            && row.title == "Updated title"));
        std::thread::sleep(Duration::from_secs(10));
        let refreshed = linker.native_link("desktop");
        println!("standalone link after cache expiry = {refreshed}");
        assert_eq!(refreshed, "claude://claude.ai/epitaxy/local_updated");
    } else {
        assert_eq!(rows.len(), 8);
        assert_eq!(display("desktop"), ("Transcript title", ""));
        assert_eq!(display("short"), ("Latest", ""));
    }
}
