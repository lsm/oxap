use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use oxap_adapter::memory::{Clock, IdGenerator};
use oxap_adapter::testkit::drain;
use oxap_adapter::{Error, ResumeRequest};
use oxap_protocol::{Envelope, RunStatus, SessionStatus};
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
while True:
    message = read()
    if message["method"] == "exit":
        sys.exit(0)
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
    let process = Process::start(&ProcessConfig {
        path: python().display().to_string(),
        args: vec!["-c".into(), CHILD.into()],
        dir: std::env::temp_dir().display().to_string(),
        ..ProcessConfig::default()
    })
    .unwrap();
    let ids = Arc::new(Counter::default());
    let reducer = Reducer::new("s1".into(), "native-1".into(), ids, Arc::new(Fixed));
    Session::new(reducer, process, 64)
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
