#![cfg(unix)]

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crossbeam_channel::RecvTimeoutError;
use oxap_adapter_claude::rpc::{ClientError, Inbound, Process, ProcessConfig, ProcessError};

static SPAWNING: Mutex<()> = Mutex::new(());
const WAIT: Duration = Duration::from_secs(5);
const KEEP_ALIVE: &str = r#"printf '%s\n' '{"type":"keep_alive"}'"#;
const DRAIN: &str = "while IFS= read -r line; do :; done";

fn serial() -> MutexGuard<'static, ()> {
    SPAWNING.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn fake_claude(script: &str) -> ProcessConfig {
    ProcessConfig {
        path: "/bin/sh".into(),
        args: vec!["-c".into(), script.into(), "claude".into()],
        env: Some(vec!["PATH=/usr/bin:/bin".into()]),
        exit_timeout: Some(Duration::from_secs(2)),
        ..ProcessConfig::default()
    }
}

fn running(process: &Process) {
    match process.client.inbound().recv_timeout(WAIT) {
        Ok(Inbound::Observation(observation)) => {
            assert_eq!(observation.message_type, "keep_alive")
        }
        other => panic!("the child never wrote its first frame: {other:?}"),
    }
}

fn exited(process: &Process) -> bool {
    matches!(
        process.done().recv_timeout(Duration::ZERO),
        Err(RecvTimeoutError::Disconnected)
    )
}

#[test]
fn a_child_that_exits_on_stdin_eof_closes_cleanly_and_a_second_close_repeats_the_first() {
    let _serial = serial();
    let process = Process::start(&fake_claude(&format!("{KEEP_ALIVE}; {DRAIN}")), None).unwrap();
    running(&process);
    assert_eq!(process.close(None), Ok(()));
    assert!(exited(&process));
    assert_eq!(process.wait_error(), None);
    assert_eq!(process.close(None), Ok(()));
    assert_eq!(
        process.client.call(b"{}", None),
        Err(ClientError::Transport("EOF".into()))
    );
}

#[test]
fn a_nonzero_exit_is_the_wait_error_and_not_a_close_error() {
    let _serial = serial();
    let process = Process::start(
        &fake_claude(&format!("{KEEP_ALIVE}; {DRAIN}; exit 1")),
        None,
    )
    .unwrap();
    running(&process);
    assert_eq!(process.close(None), Ok(()));
    assert_eq!(process.wait_error().as_deref(), Some("exit status 1"));
}

#[test]
fn an_empty_environment_allowlist_passes_nothing_ambient() {
    let _serial = serial();
    assert!(std::env::var_os("HOME").is_some());
    let mut config = fake_claude(&format!(
        "printf '%s' \"${{HOME-unset}}\" >&2; {KEEP_ALIVE}; {DRAIN}"
    ));
    config.env = Some(Vec::new());
    let process = Process::start(&config, None).unwrap();
    running(&process);
    assert_eq!(process.close(None), Ok(()));
    assert_eq!(process.stderr(), "unset");
}

#[test]
fn a_child_that_ignores_stdin_eof_is_interrupted_then_killed_and_reaped() {
    let _serial = serial();
    let mut config = fake_claude("trap '' INT; while true; do sleep 1; done");
    config.exit_timeout = Some(Duration::from_millis(300));
    let process = Process::start(&config, None).unwrap();
    let started = Instant::now();
    assert_eq!(process.close(None), Err(ProcessError::NotExited));
    assert!(started.elapsed() < WAIT);
    assert!(exited(&process));
    assert_eq!(process.close(None), Err(ProcessError::NotExited));
}

#[test]
fn a_close_past_its_deadline_kills_the_child() {
    let _serial = serial();
    let mut config = fake_claude("trap '' INT; while true; do sleep 1; done");
    config.exit_timeout = Some(Duration::from_secs(30));
    let process = Process::start(&config, None).unwrap();
    let started = Instant::now();
    let deadline = started + Duration::from_millis(300);
    assert_eq!(
        process.close(Some(deadline)),
        Err(ProcessError::DeadlineExceeded)
    );
    assert!(started.elapsed() < WAIT);
    assert!(exited(&process));
    assert_eq!(
        ProcessError::DeadlineExceeded.to_string(),
        "context deadline exceeded"
    );
}

#[test]
fn a_malformed_frame_retires_the_client_with_the_reader_error() {
    let _serial = serial();
    let process = Process::start(
        &fake_claude(&format!(
            "printf '%s\\n' '[SandboxDebug] not json'; {DRAIN}"
        )),
        None,
    )
    .unwrap();
    assert_eq!(
        process.client.done().recv_timeout(WAIT),
        Err(RecvTimeoutError::Disconnected)
    );
    let error = process.client.err().unwrap().to_string();
    assert!(error.contains("exactly one JSON object"), "{error}");
    assert_eq!(process.close(None), Ok(()));
}

#[test]
fn stderr_is_bounded_and_redacted() {
    let _serial = serial();
    let mut config = fake_claude(&format!(
        "printf '%s\\n' 'x-api-key: supersecret' >&2; printf '%0500d' 0 >&2; {KEEP_ALIVE}; {DRAIN}"
    ));
    config.stderr_limit = 64;
    let process = Process::start(&config, None).unwrap();
    running(&process);
    assert_eq!(process.close(None), Ok(()));
    let captured = process.stderr();
    assert!(!captured.contains("supersecret"), "{captured}");
    assert!(captured.contains("[REDACTED]"), "{captured}");
    assert!(captured.ends_with(" [truncated]"), "{captured}");
}

#[test]
fn a_start_without_an_executable_or_past_its_deadline_spawns_nothing() {
    assert_eq!(
        Process::start(&ProcessConfig::default(), None).err(),
        Some(ProcessError::MissingPath)
    );
    assert_eq!(
        ProcessError::MissingPath.to_string(),
        "claude rpc: executable path is required"
    );
    assert_eq!(
        Process::start(&fake_claude("exit 0"), Some(Instant::now())).err(),
        Some(ProcessError::DeadlineExceeded)
    );
}

#[test]
fn a_descendant_holding_stderr_past_the_drain_grace_finds_its_read_end_closed() {
    let _serial = serial();
    let dir = std::env::temp_dir().join(format!("oxap-claude-stderr-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let alive = dir.join("alive");
    let broken = dir.join("broken");
    let mut config = fake_claude(&format!(
        "(trap '' PIPE; sleep 3; if printf x >&2; then : > '{}'; else : > '{}'; fi) >/dev/null &\n{KEEP_ALIVE}; {DRAIN}",
        alive.display(),
        broken.display()
    ));
    config.exit_timeout = Some(Duration::from_millis(500));
    let process = Process::start(&config, None).unwrap();
    running(&process);
    let _ = process.close(None);
    assert_eq!(
        process.done().recv_timeout(WAIT),
        Err(RecvTimeoutError::Disconnected)
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !alive.exists() && !broken.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let (wrote, refused) = (alive.exists(), broken.exists());
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        refused && !wrote,
        "the descendant's stderr write after the child exited reached an open read end"
    );
}

#[test]
fn a_forced_close_returns_after_its_reader_and_pending_call_end_while_a_descendant_holds_stdout() {
    let _serial = serial();
    let mut config = fake_claude(&format!(
        "(sleep 8) &\n{KEEP_ALIVE}; IFS= read -r line; {KEEP_ALIVE}; {DRAIN}"
    ));
    config.exit_timeout = Some(Duration::from_millis(500));
    let process = Arc::new(Process::start(&config, None).unwrap());
    running(&process);
    let (called, call) = crossbeam_channel::bounded(1);
    let caller = process.clone();
    std::thread::spawn(move || {
        let _ = called.send(caller.client.call(br#"{"subtype":"probe"}"#, None));
    });
    running(&process);
    let (closed, close) = crossbeam_channel::bounded(1);
    let closer = process.clone();
    std::thread::spawn(move || {
        let _ = closed.send(closer.close(None));
    });
    assert_eq!(
        close
            .recv_timeout(WAIT)
            .expect("close blocked while a descendant held stdout"),
        Err(ProcessError::NotExited)
    );
    assert_eq!(
        process.client.read_done().recv_timeout(Duration::ZERO),
        Err(RecvTimeoutError::Disconnected),
        "close returned while the reader was still running"
    );
    assert!(call
        .recv_timeout(WAIT)
        .expect("the pending call never settled after close")
        .is_err());
    assert_eq!(
        process.done().recv_timeout(Duration::ZERO),
        Err(RecvTimeoutError::Disconnected)
    );
}

#[test]
fn a_forced_close_fails_a_write_blocked_on_a_descendant_holding_stdin_unread() {
    let _serial = serial();
    let mut config = fake_claude(&format!(
        "exec 3<&0\n(exec 0<&3 3<&-; exec sleep 12) >/dev/null 2>&1 &\nexec 3<&-\n{KEEP_ALIVE}; exec sleep 30"
    ));
    config.exit_timeout = Some(Duration::from_millis(500));
    let process = Arc::new(Process::start(&config, None).unwrap());
    running(&process);
    let blob = format!(r#"{{"subtype":"probe","blob":"{}"}}"#, "x".repeat(1 << 20));
    let (called, call) = crossbeam_channel::bounded(1);
    let caller = process.clone();
    std::thread::spawn(move || {
        let _ = called.send(caller.client.call(blob.as_bytes(), None));
    });
    std::thread::sleep(Duration::from_millis(200));
    let (closed, close) = crossbeam_channel::bounded(1);
    let closer = process.clone();
    std::thread::spawn(move || {
        let _ = closed.send(closer.close(None));
    });
    assert!(close
        .recv_timeout(WAIT)
        .expect("close blocked while a descendant held stdin unread")
        .is_err());
    assert!(call
        .recv_timeout(WAIT)
        .expect("the call's write still blocks after close")
        .is_err());
}

#[test]
fn a_call_settles_when_the_child_exits_right_after_its_response() {
    let _serial = serial();
    let script = r#"IFS= read -r line
id=$(printf '%s' "$line" | sed 's/.*"request_id":"\([^"]*\)".*/\1/')
blob=$(head -c 1048576 /dev/zero | tr '\0' x)
printf '%s\n' "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$id\",\"response\":{\"ok\":true,\"blob\":\"$blob\"}}}"
exit 0"#;
    let expected = format!(r#"{{"ok":true,"blob":"{}"}}"#, "x".repeat(1 << 20)).into_bytes();
    for iteration in 0..10 {
        let process = Process::start(&fake_claude(script), None).unwrap();
        let inbound = process.client.inbound();
        std::thread::spawn(move || while inbound.recv().is_ok() {});
        let settled = process.client.call(br#"{"subtype":"probe"}"#, None);
        assert_eq!(settled, Ok(Some(expected.clone())), "iteration {iteration}");
        assert_eq!(process.close(None), Ok(()));
    }
}
