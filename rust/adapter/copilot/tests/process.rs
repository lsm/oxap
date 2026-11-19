#![cfg(unix)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::RecvTimeoutError;
use oxap_adapter_copilot::client::Error;
use oxap_adapter_copilot::process::{Process, ProcessConfig};
use serde_json::Value;

const WAIT: Duration = Duration::from_secs(5);
const CHILD: &str = r#"
import json, os, sys, time
source, sink = sys.stdin.buffer, sys.stdout.buffer
def read():
    length = None
    while True:
        line = source.readline()
        if not line:
            sys.exit(0)
        if line == b"\r\n":
            break
        name, _, value = line.partition(b":")
        if name.strip().lower() == b"content-length":
            length = int(value.strip())
    return json.loads(source.read(length))
def write(body):
    data = json.dumps(body).encode()
    sink.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
    sink.flush()
while True:
    message = read()
    if message["method"] == "stall":
        while True:
            time.sleep(3600)
    if message["method"] == "malformed":
        write({"jsonrpc": "1.0"})
        continue
    result = {"protocolVersion": 3}
    if message["method"] == "home":
        result["home"] = os.environ.get("HOME", "")
        result["path"] = os.environ.get("PATH", "")
    write({"jsonrpc": "2.0", "id": message["id"], "result": result})
"#;

fn python() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH is set");
    std::env::split_paths(&path)
        .map(|dir| dir.join("python3"))
        .find(|candidate| candidate.is_file())
        .expect("the synthetic Copilot child needs python3 on PATH")
}

fn synthetic(home: &std::path::Path) -> Process {
    Process::start(&ProcessConfig {
        path: python().display().to_string(),
        args: vec!["-c".into(), CHILD.into()],
        dir: home.display().to_string(),
        env: vec![format!("HOME={}", home.display())],
        ..ProcessConfig::default()
    })
    .unwrap()
}

fn home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oxap-copilot-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn reaped(process: &Process) -> bool {
    process.done().recv_timeout(WAIT) == Err(RecvTimeoutError::Disconnected)
}

#[test]
fn the_child_sees_only_its_listed_environment_and_is_reaped_on_close() {
    assert!(std::env::var_os("PATH").is_some_and(|path| !path.is_empty()));
    let dir = home("environment");
    let process = synthetic(&dir);
    let result = process
        .client
        .call("home", &serde_json::json!({}), Some(Instant::now() + WAIT))
        .unwrap()
        .unwrap();
    let result: Value = serde_json::from_str(result.get()).unwrap();
    assert_eq!(result["home"], dir.display().to_string());
    assert_eq!(result["path"], "");
    process.close();
    assert!(reaped(&process));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_deadline_on_a_stalled_call_kills_and_reaps_the_child() {
    let dir = home("stall");
    let process = synthetic(&dir);
    let deadline = Some(Instant::now() + Duration::from_millis(100));
    let failure = process
        .client
        .call("stall", &serde_json::json!({}), deadline)
        .unwrap_err();
    assert_eq!(failure, Error::DeadlineExceeded);
    assert!(reaped(&process), "the stalled child was not reaped");
    assert_eq!(process.wait_error().as_deref(), Some("signal: killed"));
    process.close();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_broken_wire_kills_and_reaps_the_child() {
    let dir = home("malformed");
    let process = synthetic(&dir);
    let failure = process
        .client
        .call(
            "malformed",
            &serde_json::json!({}),
            Some(Instant::now() + WAIT),
        )
        .unwrap_err();
    assert_eq!(failure, Error::Wire);
    assert!(
        reaped(&process),
        "the child behind a broken wire was not reaped"
    );
    process.close();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_executable_fails_to_start() {
    let missing = home("missing").join("not-a-program");
    let started = Process::start(&ProcessConfig {
        path: missing.display().to_string(),
        ..ProcessConfig::default()
    });
    assert!(started.is_err());
}
