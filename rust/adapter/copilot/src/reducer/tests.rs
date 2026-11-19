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

fn tool_events(reducer: &mut Reducer, events: &[(&str, Value)]) {
    for (kind, data) in events {
        let mut data = data.clone();
        data["interactionId"] = json!("i1");
        reducer.observe(&event("native-1", kind, data)).unwrap();
    }
}

fn tool_ids(reducer: &Reducer) -> Vec<(Option<&str>, Option<&str>)> {
    reducer
        .events
        .iter()
        .map(|envelope| {
            (
                envelope.tool_call_id.as_ref().map(|id| id.as_str()),
                envelope.in_reply_to.as_ref().map(|id| id.as_str()),
            )
        })
        .collect()
}

#[test]
fn a_tool_call_is_requested_started_reported_and_completed_in_reply_to_its_start() {
    let mut reducer = reducer();
    started(&mut reducer, "m1", "i1");
    tool_events(
        &mut reducer,
        &[
            (
                "tool.execution_start",
                json!({"toolCallId": "t1", "toolName": "bash", "arguments": {"command": "a < b"}}),
            ),
            (
                "tool.execution_partial_result",
                json!({"toolCallId": "t1", "partialOutput": "half"}),
            ),
            (
                "tool.shell_output",
                json!({"toolCallId": "t1", "text": "shell"}),
            ),
            (
                "tool.execution_partial_result",
                json!({"toolCallId": "unknown", "partialOutput": "stray"}),
            ),
            (
                "tool.execution_complete",
                json!({"toolCallId": "t1", "success": true, "result": {"content": "done"}}),
            ),
            (
                "tool.shell_output",
                json!({"toolCallId": "t1", "text": "late"}),
            ),
            ("session.idle", json!({})),
        ],
    );
    assert_eq!(
        kinds(&reducer),
        [
            "run.started",
            "action.call.requested",
            "action.call.started",
            "action.call.progress",
            "action.call.progress",
            "action.call.completed",
            "run.completed",
        ]
    );
    let requested = reducer.events[1].id.as_str().to_owned();
    let started = reducer.events[2].id.as_str().to_owned();
    let call = Some("tool-call-4");
    assert_eq!(
        tool_ids(&reducer),
        [
            (None, None),
            (call, None),
            (call, Some(requested.as_str())),
            (call, Some(started.as_str())),
            (call, Some(started.as_str())),
            (call, Some(started.as_str())),
            (None, None),
        ]
    );
    let run_id = json!(reducer.runs[0].id);
    let tool = json!({
        "tool_call_id": "tool-call-4",
        "requested_by": "copilot.headless",
        "execution_owner": "copilot-cli",
        "name": "bash",
        "session_id": "s1",
        "run_id": run_id,
    });
    let with = |key: &str, value: Value| {
        let mut payload = tool.clone();
        payload[key] = value;
        payload
    };
    assert_eq!(
        payload(&reducer.events[1]),
        with("arguments_json", json!({"command": "a < b"}))
    );
    assert!(reducer.events[1]
        .payload
        .as_ref()
        .unwrap()
        .get()
        .contains(r#""arguments_json":{"command":"a \u003c b"}"#));
    assert_eq!(payload(&reducer.events[2]), tool);
    assert_eq!(payload(&reducer.events[3]), with("progress", json!("half")));
    assert_eq!(
        payload(&reducer.events[4]),
        with("progress", json!("shell"))
    );
    assert_eq!(
        payload(&reducer.events[5]),
        with("result", json!({"content": "done"}))
    );
    for (index, envelope) in reducer.events.iter().enumerate() {
        assert_eq!(envelope.sequence, Some(index as u64 + 1));
    }
}

#[test]
fn a_failed_tool_reports_denial_and_a_default_message() {
    let mut reducer = reducer();
    started(&mut reducer, "m1", "i1");
    tool_events(
        &mut reducer,
        &[
            (
                "tool.execution_start",
                json!({"toolCallId": "t1", "toolName": "bash"}),
            ),
            (
                "tool.execution_start",
                json!({"toolCallId": "t2", "toolName": "edit"}),
            ),
            (
                "tool.execution_complete",
                json!({"toolCallId": "t1", "success": false, "error": {"code": "denied", "message": "no"}}),
            ),
            (
                "tool.execution_complete",
                json!({"toolCallId": "t2", "success": "true", "error": "unreadable"}),
            ),
        ],
    );
    assert_eq!(payload(&reducer.events[1])["arguments_json"], Value::Null);
    let failures: Vec<Value> = reducer
        .events
        .iter()
        .filter(|envelope| envelope.envelope_type == EnvelopeType::ACTION_CALL_FAILED)
        .map(|envelope| payload(envelope)["error"].clone())
        .collect();
    assert_eq!(
        failures,
        [
            json!({"code": "copilot_tool_denied", "message": "no"}),
            json!({"code": "copilot_tool_failed", "message": "Copilot reported the tool call failed"}),
        ]
    );
    assert_eq!(reducer.current, Some(0));
}

#[test]
fn settling_a_run_fails_its_unfinished_tools_before_the_terminal() {
    let mut reducer = reducer();
    started(&mut reducer, "m1", "i1");
    tool_events(
        &mut reducer,
        &[
            (
                "tool.execution_start",
                json!({"toolCallId": "t1", "toolName": "bash"}),
            ),
            ("session.idle", json!({})),
        ],
    );
    assert_eq!(
        kinds(&reducer),
        [
            "run.started",
            "action.call.requested",
            "action.call.started",
            "action.call.failed",
            "run.completed"
        ]
    );
    let started = reducer.events[2].id.clone();
    assert_eq!(reducer.events[3].in_reply_to.as_ref(), Some(&started));
    assert_eq!(
        payload(&reducer.events[3])["error"],
        json!({"code": "incomplete_tool", "message": "the Copilot run settled with an unfinished tool call"})
    );
}

#[test]
fn a_broken_tool_lifecycle_fails_the_run() {
    for (events, message) in [
        (
            vec![("tool.execution_start", json!({"toolCallId": "t1"}))],
            "a tool start that is unnamed or already started",
        ),
        (
            vec![("tool.execution_start", json!({"toolName": "bash"}))],
            "a tool start that is unnamed or already started",
        ),
        (
            vec![
                (
                    "tool.execution_start",
                    json!({"toolCallId": "t1", "toolName": "bash"}),
                ),
                (
                    "tool.execution_start",
                    json!({"toolCallId": "t1", "toolName": "bash"}),
                ),
            ],
            "a tool start that is unnamed or already started",
        ),
        (
            vec![(
                "tool.execution_complete",
                json!({"toolCallId": "t1", "success": true}),
            )],
            "a tool completion without a start in this run",
        ),
        (
            vec![
                (
                    "tool.execution_start",
                    json!({"toolCallId": "t1", "toolName": "bash"}),
                ),
                (
                    "tool.execution_complete",
                    json!({"toolCallId": "t1", "success": true}),
                ),
                (
                    "tool.execution_complete",
                    json!({"toolCallId": "t1", "success": true}),
                ),
            ],
            "a second completion for one tool call",
        ),
    ] {
        let mut reducer = reducer();
        started(&mut reducer, "m1", "i1");
        tool_events(&mut reducer, &events);
        let failed = reducer.events.last().unwrap();
        assert_eq!(failed.envelope_type, EnvelopeType::RUN_FAILED, "{message}");
        assert_eq!(
            payload(failed)["error"],
            json!({"code": "copilot_invalid_tool_lifecycle", "message": message})
        );
    }
}

#[test]
fn tool_events_without_an_open_run_are_ignored() {
    let mut reducer = reducer();
    let start = json!({"toolCallId": "t1", "toolName": "bash"});
    reducer
        .observe(&event("native-1", "tool.execution_start", start))
        .unwrap();
    assert!(reducer.events.is_empty());
}
