use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use oxap_adapter::memory::{Clock, IdGenerator};
use oxap_adapter::Error;
use oxap_protocol::{Envelope, RunStatus};
use serde_json::{json, Value};

use super::Reducer;
use crate::CAPABILITY_REVISION;

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

fn reducer() -> Reducer {
    Reducer::new("s1".into(), Arc::new(Counter::default()), Arc::new(Fixed))
}

fn payload(envelope: &Envelope) -> Value {
    serde_json::from_str(envelope.payload.as_ref().unwrap().get()).unwrap()
}

fn kinds(reducer: &Reducer) -> Vec<&str> {
    reducer
        .events
        .iter()
        .map(|envelope| envelope.envelope_type.as_str())
        .collect()
}

#[test]
fn admission_bounds_one_running_run_and_eight_queued_ones() {
    let mut reducer = reducer();
    let first = reducer.admit(false).unwrap();
    assert_eq!(reducer.runs[first].status, RunStatus::RUNNING);
    for _ in 0..8 {
        let index = reducer.admit(false).unwrap();
        assert_eq!(reducer.runs[index].status, RunStatus::QUEUED);
    }
    assert!(matches!(reducer.admit(false), Err(Error::RunActive)));

    let mut reducer = self::reducer();
    for _ in 0..8 {
        let index = reducer.admit(true).unwrap();
        assert_eq!(reducer.runs[index].status, RunStatus::QUEUED);
    }
    assert!(matches!(reducer.admit(true), Err(Error::RunActive)));
    assert_eq!(reducer.runs.len(), 8);
}

#[test]
fn only_a_settled_queued_run_frees_a_queue_slot() {
    let mut reducer = reducer();
    for _ in 0..9 {
        reducer.admit(false).unwrap();
    }
    reducer.complete(0).unwrap();
    assert!(matches!(reducer.admit(false), Err(Error::RunActive)));
    reducer.fail(1, "copilot_session_error", "quota").unwrap();
    let index = reducer.admit(false).unwrap();
    assert_eq!(reducer.runs[index].status, RunStatus::QUEUED);
}

#[test]
fn completion_without_text_carries_an_empty_final_response() {
    let mut reducer = reducer();
    let index = reducer.admit(false).unwrap();
    reducer.complete(index).unwrap();
    let run = &reducer.runs[index];
    assert_eq!(kinds(&reducer), ["run.completed"]);
    let envelope = &reducer.events[0];
    assert_eq!(envelope.id.as_str(), "event-3");
    assert_eq!(envelope.sequence, Some(1));
    assert_eq!(envelope.timestamp_ms, Some(1_700_000_000_000));
    assert_eq!(envelope.session_id.as_ref().unwrap().as_str(), "s1");
    assert_eq!(envelope.run_id.as_ref(), Some(&run.id));
    assert_eq!(
        envelope.capability_revision.as_deref(),
        Some(CAPABILITY_REVISION.as_str())
    );
    assert!(envelope.extensions.is_empty());
    assert_eq!(
        payload(envelope),
        json!({
            "final_response": {"id": run.message_id, "role": "assistant", "content": ""},
            "stop_reason": "end_turn",
            "session_id": "s1",
            "run_id": run.id,
        })
    );
    assert_eq!(run.status, RunStatus::COMPLETED);
    assert!(run.terminal);
}

#[test]
fn a_run_settles_once_and_a_failure_carries_its_protocol_error() {
    let mut reducer = reducer();
    let index = reducer.admit(false).unwrap();
    reducer
        .fail(index, "copilot_session_error", "quota")
        .unwrap();
    reducer.complete(index).unwrap();
    reducer.fail(index, "copilot_aborted", "again").unwrap();
    assert_eq!(kinds(&reducer), ["run.failed"]);
    assert_eq!(
        payload(&reducer.events[0])["error"],
        json!({"code": "copilot_session_error", "message": "quota"})
    );
    assert_eq!(reducer.runs[index].status, RunStatus::FAILED);
    assert_eq!(reducer.current, None);
}
