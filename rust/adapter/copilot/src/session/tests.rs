use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use oxap_adapter::memory::{Clock, IdGenerator};
use oxap_adapter::testkit::drain;
use oxap_adapter::{Error, ReplayGap, ResumeRequest};
use oxap_adapter::{Session as _, CONTROL_UNADVERTISED};
use oxap_protocol::{
    Admission, ContentPart, ContentPartType, EffectiveDeliveryMode, Envelope, Message,
    MessageContent, MessageRole, MessageSubmitRequest, RequestedDeliveryMode, RunStatus,
    SessionStatus,
};
use serde_json::{json, Value};

use super::Session;
use crate::process::{Process, ProcessConfig};
use crate::reducer::Reducer;

const WAIT: Duration = Duration::from_secs(5);
const CHILD: &str = r#"
import json, sys
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
sent = 0
def event(kind, data):
    write({"jsonrpc": "2.0", "method": "session.event", "params": {"sessionId": "native-1", "event": {"type": kind, "data": data}}})
def turn(id, prompt):
    event("user.message", {"messageId": id, "interactionId": id})
    event("assistant.message_delta", {"interactionId": id, "deltaContent": prompt})
    event("session.idle", {})
while True:
    message = read()
    if message["method"] == "exit":
        sys.exit(0)
    if message["method"] == "session.send":
        sent += 1
        id, prompt = "native-message-%d" % sent, message["params"]["prompt"]
        if prompt == "refuse":
            write({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32000, "message": "busy"}})
            continue
        if prompt == "anonymous":
            write({"jsonrpc": "2.0", "id": message["id"], "result": {}})
            continue
        if prompt == "early":
            turn(id, prompt)
        write({"jsonrpc": "2.0", "id": message["id"], "result": {"messageId": id}})
        if prompt != "early" and prompt != "silent":
            turn(id, prompt)
        continue
    for event in message["params"].get("events", []):
        write({"jsonrpc": "2.0", "method": "session.event", "params": {"sessionId": "native-1", "event": event}})
    write({"jsonrpc": "2.0", "id": message["id"], "result": {}})
"#;

struct Fixed;

impl Clock for Fixed {
    fn now_ms(&self) -> i64 {
        1_700_000_000_000
    }
}

#[derive(Default)]
struct Counter(AtomicU64);

impl IdGenerator for Counter {
    fn new_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::SeqCst) + 1)
    }
}

fn python() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH is set");
    std::env::split_paths(&path)
        .map(|dir| dir.join("python3"))
        .find(|candidate| candidate.is_file())
        .expect("the synthetic Copilot child needs python3 on PATH")
}

fn session() -> Session {
    journaled(64)
}

fn journaled(capacity: usize) -> Session {
    let process = Process::start(&ProcessConfig {
        path: python().display().to_string(),
        args: vec!["-c".into(), CHILD.into()],
        dir: std::env::temp_dir().display().to_string(),
        ..ProcessConfig::default()
    })
    .unwrap();
    let ids = Arc::new(Counter::default());
    let reducer = Reducer::new("s1".into(), "native-1".into(), ids, Arc::new(Fixed));
    Session::new(reducer, process, capacity, WAIT)
}

fn admit(session: &Session, native: &str) -> oxap_protocol::RunId {
    let mut inner = session.shared.lock();
    let index = inner.reducer.admit(false).unwrap();
    inner.reducer.runs[index].native_message = native.into();
    inner.reducer.runs[index].id.clone()
}

fn emit(session: &Session, events: Value) {
    let client = &session.shared.process.client;
    client
        .call("emit", &json!({ "events": events }), None)
        .unwrap();
}

fn follow(session: &Session, run: &oxap_protocol::RunId) -> oxap_adapter::EventStream {
    let request = ResumeRequest {
        run_id: run.clone(),
        after_sequence: 0,
    };
    session.resume(&request).unwrap().1
}

fn kinds(events: &[Envelope]) -> Vec<&str> {
    events
        .iter()
        .map(|envelope| envelope.envelope_type.as_str())
        .collect()
}

fn error(envelope: &Envelope) -> Value {
    let payload: Value = serde_json::from_str(envelope.payload.as_ref().unwrap().get()).unwrap();
    payload["error"].clone()
}

#[test]
fn state_reports_the_running_run_and_the_queue_behind_it() {
    let session = session();
    let first = admit(&session, "m1");
    let second = admit(&session, "m2");
    let state = session.state().unwrap();
    assert_eq!(state.session_id.as_str(), "s1");
    assert_eq!(state.status, SessionStatus::RUNNING);
    assert_eq!(state.active_run_id.as_ref(), Some(&first));
    assert_eq!(state.active_runs.len(), 2);
    assert_eq!(state.active_runs[0].run_id, first);
    assert_eq!(state.active_runs[0].queue_position, None);
    assert_eq!(state.active_runs[1].run_id, second);
    assert_eq!(state.active_runs[1].status, RunStatus::QUEUED);
    assert_eq!(state.active_runs[1].queue_position, Some(1));
    assert_eq!(state.active_runs[1].relationship, "primary");
    assert_eq!(state.active_runs[1].as_of_sequence, Some(0));
    assert_eq!(session.native_session_id(), "native-1");
    session.close();
}

#[test]
fn native_events_reach_the_journal_and_resume_replays_them() {
    let session = session();
    let run = admit(&session, "m1");
    let mut stream = follow(&session, &run);
    emit(
        &session,
        json!([
            {"type": "user.message", "data": {"messageId": "m1", "interactionId": "i1"}},
            {"type": "assistant.message_delta", "data": {"interactionId": "i1", "deltaContent": "hi"}},
            {"type": "session.idle", "data": {}},
        ]),
    );
    let events = drain(&mut stream, WAIT);
    assert_eq!(
        kinds(&events),
        ["run.started", "content.delta", "run.completed"]
    );
    assert_eq!(session.state().unwrap().status, SessionStatus::IDLE);

    let request = ResumeRequest {
        run_id: run.clone(),
        after_sequence: 1,
    };
    let (recovery, mut replay) = session.resume(&request).unwrap();
    assert_eq!((recovery.replayed_from, recovery.replayed_through), (2, 3));
    assert!(recovery.replay_gap.is_none());
    assert_eq!(
        kinds(&drain(&mut replay, WAIT)),
        ["content.delta", "run.completed"]
    );

    let future = ResumeRequest {
        run_id: run,
        after_sequence: 4,
    };
    assert!(matches!(
        session.resume(&future),
        Err(Error::ReplayCursorFuture)
    ));
    let unknown = ResumeRequest {
        run_id: "run-unknown".into(),
        after_sequence: 0,
    };
    assert!(matches!(session.resume(&unknown), Err(Error::RunNotFound)));
    session.close();
}

#[test]
fn a_process_exit_fails_every_open_run() {
    let session = session();
    let first = admit(&session, "m1");
    let second = admit(&session, "m2");
    let (mut running, mut queued) = (follow(&session, &first), follow(&session, &second));
    let client = &session.shared.process.client;
    assert!(client.call("exit", &json!({}), None).is_err());
    for stream in [&mut running, &mut queued] {
        let events = drain(stream, WAIT);
        assert_eq!(kinds(&events), ["run.failed"]);
        assert_eq!(
            error(&events[0]),
            json!({"code": "copilot_process_exit", "message": "the Copilot server closed its RPC connection"})
        );
    }
    session.close();
}

#[test]
fn close_settles_open_runs_and_refuses_later_calls() {
    let session = session();
    let run = admit(&session, "m1");
    let mut stream = follow(&session, &run);
    session.close();
    let events = drain(&mut stream, WAIT);
    assert_eq!(kinds(&events), ["run.failed"]);
    assert_eq!(
        error(&events[0]),
        json!({"code": "copilot_closed", "message": "the Copilot session closed"})
    );
    assert!(matches!(session.state(), Err(Error::SessionClosed)));
    let request = ResumeRequest {
        run_id: run,
        after_sequence: 0,
    };
    assert!(matches!(
        session.resume(&request),
        Err(Error::SessionClosed)
    ));
    assert!(session.shared.process.done().recv_timeout(WAIT).is_err());
}

#[test]
fn an_invalid_session_event_fails_the_run_and_closes_the_wire() {
    let session = session();
    let run = admit(&session, "m1");
    let mut stream = follow(&session, &run);
    emit(&session, json!([{"data": {}}]));
    let events = drain(&mut stream, WAIT);
    assert_eq!(kinds(&events), ["run.failed"]);
    assert_eq!(
        error(&events[0]),
        json!({"code": "copilot_invalid_event", "message": "the Copilot server sent an invalid session event"})
    );
    assert!(session
        .shared
        .process
        .client
        .done()
        .recv_timeout(WAIT)
        .is_err());
    session.close();
}

#[test]
fn a_cursor_behind_the_evicted_prefix_is_a_replay_gap() {
    let session = journaled(2);
    let run = admit(&session, "m1");
    emit(
        &session,
        json!([
            {"type": "user.message", "data": {"messageId": "m1", "interactionId": "i1"}},
            {"type": "assistant.message_delta", "data": {"interactionId": "i1", "deltaContent": "hi"}},
            {"type": "session.idle", "data": {}},
        ]),
    );
    let deadline = Instant::now() + WAIT;
    while !session.shared.lock().reducer.runs[0].terminal {
        assert!(Instant::now() < deadline, "the run never settled");
        std::thread::sleep(Duration::from_millis(10));
    }
    let request = ResumeRequest {
        run_id: run,
        after_sequence: 0,
    };
    let gap = ReplayGap {
        requested_after: 0,
        oldest_available: 2,
        latest_available: 3,
    };
    assert!(matches!(session.resume(&request), Err(Error::ReplayGap(found)) if found == gap));
    session.close();
}

fn user(text: &str) -> Message {
    Message {
        role: MessageRole::USER,
        content: MessageContent::from_text(text),
        ..Message::default()
    }
}

fn submission(texts: &[&str]) -> MessageSubmitRequest {
    MessageSubmitRequest {
        session_id: "s1".into(),
        messages: texts.iter().map(|text| user(text)).collect(),
        ..MessageSubmitRequest::default()
    }
}

fn delta(envelope: &Envelope) -> Value {
    let payload: Value = serde_json::from_str(envelope.payload.as_ref().unwrap().get()).unwrap();
    payload["part"]["text"].clone()
}

#[test]
fn a_submitted_prompt_streams_its_run_to_completion() {
    let session = session();
    let (ack, mut stream) = session.submit(submission(&["first", "second"])).unwrap();
    assert!(ack.accepted);
    assert_eq!(ack.session_id.as_str(), "s1");
    assert_eq!(ack.status, RunStatus::RUNNING);
    assert_eq!(ack.admission, Admission::STARTED);
    assert_eq!(ack.requested_delivery, RequestedDeliveryMode::AUTO);
    assert_eq!(ack.effective_delivery, EffectiveDeliveryMode::START);
    assert_eq!(ack.delivery_resolution, "session_idle");
    assert_eq!(ack.message_ids.len(), 1);
    let events = drain(&mut stream, WAIT);
    assert_eq!(
        kinds(&events),
        ["run.started", "content.delta", "run.completed"]
    );
    assert_eq!(events[0].run_id, ack.run_id);
    assert_eq!(delta(&events[1]), json!("first\n\nsecond"));
    session.close();
}

#[test]
fn events_that_arrive_before_the_reply_bind_to_the_admitted_run() {
    let session = session();
    let (ack, mut stream) = session.submit(submission(&["early"])).unwrap();
    let events = drain(&mut stream, WAIT);
    assert_eq!(
        kinds(&events),
        ["run.started", "content.delta", "run.completed"]
    );
    assert_eq!(events[0].run_id, ack.run_id);
    session.close();
}

#[test]
fn a_submission_behind_an_open_run_is_queued() {
    let session = session();
    let (first, _running) = session.submit(submission(&["silent"])).unwrap();
    let (second, _queued) = session.submit(submission(&["silent"])).unwrap();
    assert_eq!(first.admission, Admission::STARTED);
    assert_eq!(second.admission, Admission::QUEUED);
    assert_eq!(second.status, RunStatus::QUEUED);
    assert_eq!(second.effective_delivery, EffectiveDeliveryMode::QUEUE);
    assert_eq!(second.delivery_resolution, "session_busy");
    let state = session.state().unwrap();
    assert_eq!(state.active_run_id, first.run_id);
    assert_eq!(state.active_runs[1].run_id, second.run_id.clone().unwrap());
    assert_eq!(state.active_runs[1].queue_position, Some(1));
    session.close();
}

#[test]
fn a_native_refusal_admits_no_run_and_keeps_the_session() {
    let session = session();
    match session.submit(submission(&["refuse"])) {
        Err(Error::Harness(text)) => assert_eq!(text, "Copilot RPC error -32000: busy"),
        other => panic!("refusal became {:?}", other.map(|(ack, _)| ack)),
    }
    assert!(session.state().unwrap().active_runs.is_empty());
    let (ack, mut stream) = session.submit(submission(&["after"])).unwrap();
    assert_eq!(ack.admission, Admission::STARTED);
    assert_eq!(
        kinds(&drain(&mut stream, WAIT)),
        ["run.started", "content.delta", "run.completed"]
    );
    session.close();
}

#[test]
fn a_reply_without_a_message_id_ends_the_session() {
    let session = session();
    match session.submit(submission(&["anonymous"])) {
        Err(Error::Harness(text)) => {
            assert_eq!(text, "copilot: session.send returned no messageId")
        }
        other => panic!("missing message id became {:?}", other.map(|(ack, _)| ack)),
    }
    assert!(session.state().unwrap().active_runs.is_empty());
    assert!(matches!(
        session.submit(submission(&["after"])),
        Err(Error::SessionClosed)
    ));
    session.close();
}

#[test]
fn submissions_that_copilot_cannot_carry_are_refused_before_the_wire() {
    let session = session();
    let image = Message {
        role: MessageRole::USER,
        content: MessageContent::from_parts(&[ContentPart {
            part_type: ContentPartType::IMAGE,
            ..ContentPart::default()
        }]),
        ..Message::default()
    };
    let mut invalid = vec![
        submission(&[""]),
        submission(&[]),
        MessageSubmitRequest {
            messages: vec![image],
            ..submission(&[])
        },
        MessageSubmitRequest {
            messages: vec![Message {
                role: MessageRole::ASSISTANT,
                ..user("hi")
            }],
            ..submission(&[])
        },
        MessageSubmitRequest {
            target_run_id: Some("run-1".into()),
            ..submission(&["hi"])
        },
    ];
    for request in invalid.drain(..) {
        assert!(
            matches!(
                session.submit(request.clone()),
                Err(Error::InvalidSubmission)
            ),
            "{request:?}"
        );
    }
    let foreign = MessageSubmitRequest {
        session_id: "s2".into(),
        ..submission(&["hi"])
    };
    assert!(matches!(session.submit(foreign), Err(Error::RunNotFound)));
    let instructed = MessageSubmitRequest {
        instructions: Some("be brief".into()),
        ..submission(&["hi"])
    };
    match session.submit(instructed) {
        Err(Error::UnsupportedControl(refusal)) => assert_eq!(refusal.reason, CONTROL_UNADVERTISED),
        other => panic!("instructions became {:?}", other.map(|(ack, _)| ack)),
    }
    let steered = MessageSubmitRequest {
        delivery: RequestedDeliveryMode::STEER,
        ..submission(&["hi"])
    };
    match session.submit(steered) {
        Err(Error::UnsupportedControl(refusal)) => assert_eq!(
            (refusal.feature.as_str(), refusal.reason),
            ("session.message.delivery.steer", CONTROL_UNADVERTISED)
        ),
        other => panic!("steer became {:?}", other.map(|(ack, _)| ack)),
    }
    assert!(session.state().unwrap().active_runs.is_empty());
    session.close();
}

#[test]
fn the_session_contract_refuses_cancel_and_resolution() {
    let session = session();
    let cancel = futures::executor::block_on(session.cancel("run-1".into()));
    match cancel {
        Err(Error::UnsupportedControl(refusal)) => {
            assert_eq!(
                (refusal.feature.as_str(), refusal.reason),
                ("run.cancel", CONTROL_UNADVERTISED)
            )
        }
        other => panic!("cancel became {other:?}"),
    }
    let resolved = futures::executor::block_on(session.resolve(Default::default()));
    assert!(matches!(resolved, Err(Error::InteractionNotFound)));
    assert_eq!(
        oxap_adapter::Session::native_session_id(&session).as_deref(),
        Some("native-1")
    );
    futures::executor::block_on(oxap_adapter::Session::close(&session)).unwrap();
    assert!(matches!(session.state(), Err(Error::SessionClosed)));
}
