mod support;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::task::noop_waker;
use oxap_adapter::runtime::Runtime;
use oxap_adapter::{Adapter as _, Error, InteractionResolution, ResumeRequest, SubmitRequest};
use oxap_adapter_codex_appserver::adapter::{Adapter, Config};
use oxap_adapter_codex_appserver::process::ProcessConfig;
use oxap_adapter_codex_appserver::session::Session;
use oxap_protocol::{
    Message, MessageContent, MessageRole, MessageSubmitRequest, RunId, SessionSettingsUpdateRequest,
};
use support::{adapter, request, Fake};

fn wait<F: Future + Unpin>(mut future: F) -> F::Output {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Poll::Ready(value) =
            Pin::new(&mut future).poll(&mut Context::from_waker(&noop_waker()))
        {
            return value;
        }
        assert!(Instant::now() < deadline, "the operation never finished");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn start<F: Future + Unpin>(future: &mut F) {
    let _ = Pin::new(future).poll(&mut Context::from_waker(&noop_waker()));
}

fn submission() -> SubmitRequest {
    SubmitRequest {
        request: MessageSubmitRequest {
            session_id: "s".into(),
            messages: vec![Message {
                role: MessageRole::USER,
                content: MessageContent::from_text("hi"),
                ..Message::default()
            }],
            ..MessageSubmitRequest::default()
        },
        ..SubmitRequest::default()
    }
}

fn calls(fake: &Fake, method: &str) -> usize {
    fake.calls()
        .iter()
        .filter(|(called, _)| called == method)
        .count()
}

fn open(fake: &Arc<Fake>) -> Session {
    adapter(fake, |_| {}).open(request()).unwrap()
}

#[test]
fn the_contract_serves_the_session_go_serves() {
    let fake = Fake::new([
        Ok(r#"{"thread":{"id":"t-1"}}"#),
        Ok(r#"{"turn":{"id":"turn-1","status":"inProgress"}}"#),
    ]);
    let adapter = adapter(&fake, |_| {});
    let descriptor = wait(adapter.probe()).unwrap();
    assert_eq!(
        serde_json::to_value(&descriptor.capabilities).unwrap(),
        serde_json::to_value(&adapter.describe().capabilities).unwrap()
    );
    let session = wait(oxap_adapter::Adapter::open(&adapter, request())).unwrap();
    assert_eq!(session.native_session_id().as_deref(), Some("t-1"));
    assert!(session.settings_updater().is_some());
    let (response, _stream) = wait(session.submit(submission())).unwrap();
    assert_eq!(response.run_id, Some(RunId::from("run-minted")));
    assert_eq!(fake.closes(), 0);
}

#[test]
fn a_submit_dropped_during_turn_start_closes_the_session_as_go_does() {
    let fake = Fake::new([
        Ok(r#"{"thread":{"id":"t-1"}}"#),
        Ok(r#"{"turn":{"id":"turn-1","status":"inProgress"}}"#),
    ]);
    fake.fail_calls_once_closed();
    let session = open(&fake);
    let (arrivals, release) = fake.hold_calls("turn/start");
    let mut submitting = oxap_adapter::Session::submit(&session, submission());
    start(&mut submitting);
    arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(submitting);
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(session.state(), Err(Error::SessionClosed)) {
        assert!(Instant::now() < deadline, "the session stayed open");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(fake.closes(), 1);
}

#[test]
fn a_submit_dropped_while_another_holds_the_session_never_starts_a_turn() {
    let fake = Fake::new([
        Ok(r#"{"thread":{"id":"t-1"}}"#),
        Err("busy"),
        Ok(r#"{"turn":{"id":"turn-2","status":"inProgress"}}"#),
    ]);
    let next = Arc::new(AtomicU64::new(0));
    let session = Arc::new(
        adapter(&fake, |config| {
            config.runtime = Runtime::new(
                || 1,
                move |kind| format!("{kind}-{}", next.fetch_add(1, Ordering::SeqCst) + 1),
            );
        })
        .open(request())
        .unwrap(),
    );
    let (arrivals, release) = fake.hold_calls("turn/start");
    let first = {
        let session = session.clone();
        std::thread::spawn(move || wait(oxap_adapter::Session::submit(&*session, submission())))
    };
    arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut second = oxap_adapter::Session::submit(&*session, submission());
    start(&mut second);
    std::thread::sleep(Duration::from_millis(100));
    drop(second);
    release.send(()).unwrap();
    fake.stop_holding();
    assert!(first.join().unwrap().is_err());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(calls(&fake, "turn/start"), 1);
    assert_eq!(fake.closes(), 0);
    let (third, _stream) = session.submit(submission().request).unwrap();
    assert_eq!(
        (
            third.run_id.unwrap().as_str(),
            third.submission_id.as_str(),
            third.message_ids
        ),
        ("run-5", "submission-7", vec!["message-4".into()])
    );
}

#[test]
fn a_cancel_dropped_while_another_interrupts_sends_no_interrupt_as_go_does() {
    let fake = Fake::new([
        Ok(r#"{"thread":{"id":"t-1"}}"#),
        Ok(r#"{"turn":{"id":"turn-1","status":"inProgress"}}"#),
    ]);
    fake.always_reply("turn/interrupt", Err("busy"));
    let session = Arc::new(open(&fake));
    let (response, _stream) = session.submit(submission().request).unwrap();
    let run = response.run_id.unwrap();
    let (arrivals, release) = fake.hold_interrupts();
    let first = {
        let (session, run) = (session.clone(), run.clone());
        std::thread::spawn(move || wait(oxap_adapter::Session::cancel(&*session, run)))
    };
    arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut second = oxap_adapter::Session::cancel(&*session, run);
    start(&mut second);
    std::thread::sleep(Duration::from_millis(100));
    drop(second);
    release.send(()).unwrap();
    assert!(first.join().unwrap().is_err());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(calls(&fake, "turn/interrupt"), 1);
    assert_eq!(fake.closes(), 0);
}

#[test]
fn an_open_dropped_during_thread_start_closes_its_client_as_go_does() {
    let fake = Fake::new([Ok(r#"{"thread":{"id":"t-1"}}"#)]);
    fake.fail_calls_once_closed();
    let adapter = adapter(&fake, |_| {});
    let (arrivals, release) = fake.hold_calls("thread/start");
    let mut opening = oxap_adapter::Adapter::open(&adapter, request());
    start(&mut opening);
    arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(opening);
    assert!(fake.closes() > 0, "the dropped open left its client open");
    release.send(()).unwrap();
}

fn poll_each_once(
    session: &dyn oxap_adapter::Session,
    polled: &crossbeam_channel::Sender<()>,
) -> (Result<(), Error>, bool) {
    let mut state = session.state();
    start(&mut state);
    let mut resolving = session.resolve(InteractionResolution::default());
    start(&mut resolving);
    let mut resuming = session.resume(ResumeRequest::default());
    start(&mut resuming);
    let request = SessionSettingsUpdateRequest {
        session_id: "s".into(),
        ..SessionSettingsUpdateRequest::default()
    };
    let mut updating = session.settings_updater().unwrap().update_settings(request);
    start(&mut updating);
    polled.send(()).unwrap();
    (wait(updating).map(|_| ()), wait(state).is_ok())
}

#[test]
fn a_held_turn_start_leaves_every_other_operation_a_future_that_polls_at_once() {
    let fake = Fake::new([
        Ok(r#"{"thread":{"id":"t-1"}}"#),
        Ok(r#"{"turn":{"id":"turn-1","status":"inProgress"}}"#),
    ]);
    let session = Arc::new(open(&fake));
    let (arrivals, release) = fake.hold_calls("turn/start");
    let submitting = {
        let session = session.clone();
        std::thread::spawn(move || wait(oxap_adapter::Session::submit(&*session, submission())))
    };
    arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    let (polled, finished) = crossbeam_channel::unbounded();
    let host = {
        let session = session.clone();
        std::thread::spawn(move || poll_each_once(&*session, &polled))
    };
    let prompt = finished.recv_timeout(Duration::from_secs(1));
    release.send(()).unwrap();
    assert!(
        prompt.is_ok(),
        "an operation blocked before it returned its future"
    );
    let (updated, stated) = host.join().unwrap();
    assert!(matches!(updated, Err(Error::RunActive)));
    assert!(stated);
    assert!(submitting.join().unwrap().is_ok());
}

#[cfg(unix)]
#[test]
fn an_open_dropped_while_initialize_is_withheld_ends_codex_as_go_does() {
    let mut codex = Codex::open(
        "initialize",
        "echo $$ > pid; read request; : > seen; while read line; do :; done",
        Duration::from_secs(2),
    );
    codex.drop_open();
    let survived = codex.survives(Duration::from_secs(5));
    assert!(!survived, "the dropped open left Codex running");
}

#[cfg(unix)]
#[test]
fn an_open_dropped_during_thread_start_returns_before_codex_exits_as_go_does() {
    let mut codex = Codex::open(
        "thread-start",
        r#"echo $$ > pid; read request; printf '%s\n' '{"id":0,"result":{}}'; read notice; read start; : > seen; while :; do sleep 1; done"#,
        Duration::from_secs(3),
    );
    let began = Instant::now();
    codex.drop_open();
    let took = began.elapsed();
    let survived = codex.survives(Duration::from_secs(8));
    assert!(
        took < Duration::from_secs(1),
        "dropping the open waited {took:?} for Codex to exit"
    );
    assert!(!survived, "the dropped open left Codex running");
}

#[cfg(unix)]
struct Codex {
    opening: Option<oxap_adapter::BoxFuture<'static, Box<dyn oxap_adapter::Session>>>,
    dir: std::path::PathBuf,
    pid: String,
}

#[cfg(unix)]
impl Codex {
    fn open(name: &str, script: &str, shutdown: Duration) -> Self {
        let dir =
            std::env::temp_dir().join(format!("oap-codex-contract-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let adapter = Adapter::new(Config {
            process: ProcessConfig {
                path: "/bin/sh".into(),
                args: vec!["-c".into(), script.into(), "codex".into()],
                dir: dir.to_string_lossy().into_owned(),
                env: Some(vec!["PATH=/usr/bin:/bin".into()]),
                shutdown_timeout: Some(shutdown),
                ..ProcessConfig::default()
            },
            ..Config::default()
        })
        .unwrap();
        let mut opening: oxap_adapter::BoxFuture<'static, Box<dyn oxap_adapter::Session>> =
            Box::pin(async move { oxap_adapter::Adapter::open(&adapter, request()).await });
        start(&mut opening);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dir.join("seen").exists() {
            assert!(Instant::now() < deadline, "Codex never reached its stall");
            std::thread::sleep(Duration::from_millis(5));
        }
        let pid = std::fs::read_to_string(dir.join("pid"))
            .unwrap()
            .trim()
            .to_owned();
        let codex = Self {
            opening: Some(opening),
            dir,
            pid,
        };
        std::thread::sleep(Duration::from_millis(200));
        assert!(codex.alive(), "Codex ended before the open was dropped");
        codex
    }

    fn drop_open(&mut self) {
        drop(self.opening.take());
    }

    fn alive(&self) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &self.pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    fn survives(&self, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        while self.alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.alive()
    }
}

#[cfg(unix)]
impl Drop for Codex {
    fn drop(&mut self) {
        if self.alive() {
            let _ = std::process::Command::new("kill").arg(&self.pid).status();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
