use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use oxap_adapter::memory::{Clock, IdGenerator};
use oxap_adapter::Error;
use oxap_protocol::{Envelope, EnvelopeType, RunStatus};
use serde_json::{json, Value};

use super::Reducer;
use crate::native::Event;
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
    Reducer::new(
        "s1".into(),
        "native-1".into(),
        Arc::new(Counter::default()),
        Arc::new(Fixed),
    )
}

fn event(session: &str, kind: &str, data: Value) -> Event {
    Event {
        session_id: session.into(),
        kind: kind.into(),
        data: serde_json::from_str(&data.to_string()).unwrap(),
        ..Event::default()
    }
}

fn started(reducer: &mut Reducer, native: &str, interaction: &str) -> usize {
    let index = reducer.admit(false).unwrap();
    reducer.runs[index].native_message = native.into();
    let user = json!({"messageId": native, "interactionId": interaction});
    reducer
        .observe(&event("native-1", "user.message", user))
        .unwrap();
    index
}

fn extensions(envelope: &Envelope) -> Value {
    let mut object = serde_json::Map::new();
    for (key, raw) in &envelope.extensions {
        object.insert(key.clone(), serde_json::from_str(raw.get()).unwrap());
    }
    Value::Object(object)
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

#[test]
fn a_run_streams_its_text_and_settles_with_summed_usage_at_idle() {
    let mut reducer = reducer();
    started(&mut reducer, "m1", "i1");
    for (kind, data) in [
        (
            "assistant.message_delta",
            json!({"interactionId": "i1", "deltaContent": "hel"}),
        ),
        (
            "assistant.message_delta",
            json!({"interactionId": "i1", "deltaContent": ""}),
        ),
        (
            "assistant.message",
            json!({"interactionId": "i1", "content": "hello"}),
        ),
        (
            "assistant.usage",
            json!({"inputTokens": 3, "outputTokens": 4, "cost": 0.5, "model": "gpt-a"}),
        ),
        (
            "assistant.usage",
            json!({"inputTokens": 5, "outputTokens": 6, "cost": 1, "model": "gpt-<b>"}),
        ),
        ("session.idle", json!({})),
    ] {
        reducer.observe(&event("native-1", kind, data)).unwrap();
    }
    assert_eq!(
        kinds(&reducer),
        ["run.started", "content.delta", "run.completed"]
    );
    let run = &reducer.runs[0];
    for (index, envelope) in reducer.events.iter().enumerate() {
        assert_eq!(envelope.sequence, Some(index as u64 + 1));
        assert_eq!(envelope.timestamp_ms, Some(1_700_000_000_000));
        assert_eq!(envelope.session_id.as_ref().unwrap().as_str(), "s1");
        assert_eq!(envelope.run_id.as_ref(), Some(&run.id));
        assert_eq!(
            envelope.capability_revision.as_deref(),
            Some(CAPABILITY_REVISION.as_str())
        );
    }
    let ids = json!({"session_id": "s1", "run_id": run.id});
    assert_eq!(
        payload(&reducer.events[0]),
        json!({"status": "running", "session_id": "s1", "run_id": ids["run_id"]})
    );
    assert_eq!(
        payload(&reducer.events[1]),
        json!({"message_id": run.message_id, "part": {"type": "text", "text": "hel"}, "session_id": "s1", "run_id": ids["run_id"]})
    );
    assert_eq!(
        payload(&reducer.events[2]),
        json!({
            "final_response": {"id": run.message_id, "role": "assistant", "content": [{"type": "text", "text": "hello"}]},
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 8, "output_tokens": 10},
            "session_id": "s1",
            "run_id": ids["run_id"],
        })
    );
    assert_eq!(
        extensions(&reducer.events[2]),
        json!({"com.github.copilot.cost": {"premium_requests": 1.5}, "com.github.copilot.routed_model": {"model": "gpt-<b>"}})
    );
    assert!(
        reducer.events[2].extensions["com.github.copilot.routed_model"]
            .get()
            .contains("\\u003cb\\u003e")
    );
    assert!(reducer.events[0].extensions.is_empty());
    assert_eq!(run.status, RunStatus::COMPLETED);
    assert!(run.terminal);
    assert_eq!(reducer.current, None);
}

#[test]
fn usage_that_cannot_be_summed_is_dropped_rather_than_guessed() {
    let mut reducer = reducer();
    started(&mut reducer, "m1", "i1");
    for data in [
        json!({"inputTokens": "3", "outputTokens": 18446744073709551615u64, "cost": "0.5"}),
        json!({"inputTokens": 1, "outputTokens": 1, "cost": 1}),
    ] {
        reducer
            .observe(&event("native-1", "assistant.usage", data))
            .unwrap();
    }
    reducer
        .observe(&event("native-1", "session.idle", json!({})))
        .unwrap();
    assert_eq!(payload(&reducer.events[1])["usage"], json!({}));
    assert!(reducer.events[1].extensions.is_empty());

    let mut reducer = self::reducer();
    started(&mut reducer, "m1", "i1");
    for data in [
        json!({"inputTokens": 18446744073709551615u64, "cost": 1.0e308}),
        json!({"inputTokens": 1, "cost": 1.0e308}),
    ] {
        reducer
            .observe(&event("native-1", "assistant.usage", data))
            .unwrap();
    }
    reducer
        .observe(&event("native-1", "session.idle", json!({})))
        .unwrap();
    assert_eq!(payload(&reducer.events[1])["usage"], json!({}));
}

#[test]
fn events_for_another_interaction_are_ignored() {
    let mut reducer = reducer();
    started(&mut reducer, "m1", "i1");
    let stray = json!({"interactionId": "i2", "deltaContent": "stray"});
    reducer
        .observe(&event("native-1", "assistant.message_delta", stray))
        .unwrap();
    reducer
        .observe(&event(
            "native-1",
            "session.idle",
            json!({"interactionId": "i2"}),
        ))
        .unwrap();
    assert_eq!(kinds(&reducer), ["run.started"]);
}

#[test]
fn an_event_for_another_native_session_fails_only_an_open_run() {
    let mut reducer = reducer();
    reducer
        .observe(&event("native-2", "session.idle", json!({})))
        .unwrap();
    assert!(reducer.events.is_empty());
    started(&mut reducer, "m1", "i1");
    reducer
        .observe(&event("native-2", "session.idle", json!({})))
        .unwrap();
    assert_eq!(kinds(&reducer), ["run.started", "run.failed"]);
    assert_eq!(
        payload(&reducer.events[1])["error"]["code"],
        "copilot_foreign_session"
    );
    assert_eq!(reducer.runs[0].status, RunStatus::FAILED);
}

#[test]
fn an_aborted_idle_and_a_session_error_fail_the_run() {
    let mut reducer = reducer();
    started(&mut reducer, "m1", "i1");
    reducer
        .observe(&event("native-1", "session.idle", json!({"aborted": true})))
        .unwrap();
    started(&mut reducer, "m2", "i2");
    reducer
        .observe(&event("native-1", "session.error", json!({})))
        .unwrap();
    started(&mut reducer, "m3", "i3");
    reducer
        .observe(&event(
            "native-1",
            "session.error",
            json!({"message": "quota"}),
        ))
        .unwrap();
    let failures: Vec<Value> = reducer
        .events
        .iter()
        .filter(|envelope| envelope.envelope_type == EnvelopeType::RUN_FAILED)
        .map(|envelope| payload(envelope)["error"].clone())
        .collect();
    assert_eq!(
        failures,
        [
            json!({"code": "copilot_aborted", "message": "Copilot ended the busy period as aborted, without a cancel for this run"}),
            json!({"code": "copilot_session_error", "message": "Copilot reported a session error"}),
            json!({"code": "copilot_session_error", "message": "quota"}),
        ]
    );
}

#[test]
fn a_queued_runs_user_message_settles_the_current_run_first() {
    let mut reducer = reducer();
    started(&mut reducer, "m1", "i1");
    let queued = reducer.admit(false).unwrap();
    assert_eq!(reducer.runs[queued].status, RunStatus::QUEUED);
    reducer.runs[queued].native_message = "m2".into();
    reducer
        .observe(&event(
            "native-1",
            "user.message",
            json!({"messageId": "unknown"}),
        ))
        .unwrap();
    reducer
        .observe(&event(
            "native-1",
            "user.message",
            json!({"messageId": "m2", "interactionId": "i2"}),
        ))
        .unwrap();
    assert_eq!(
        kinds(&reducer),
        ["run.started", "run.completed", "run.started"]
    );
    assert_eq!(reducer.events[2].sequence, Some(1));
    assert_eq!(reducer.current, Some(queued));
    assert_eq!(reducer.runs[queued].status, RunStatus::RUNNING);
    reducer
        .observe(&event("native-1", "session.idle", json!({})))
        .unwrap();
    reducer
        .observe(&event("native-1", "session.idle", json!({})))
        .unwrap();
    assert_eq!(
        kinds(&reducer),
        [
            "run.started",
            "run.completed",
            "run.started",
            "run.completed"
        ]
    );
}
