mod support;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::task::noop_waker;
use oxap_adapter::runtime::Runtime;
use oxap_adapter::{Adapter as _, Error, SubmitRequest};
use oxap_adapter_codex_appserver::session::Session;
use oxap_protocol::{Message, MessageContent, MessageRole, MessageSubmitRequest, RunId};
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
