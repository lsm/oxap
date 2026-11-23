mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use oxap_adapter::gojson::go_marshal;
use oxap_adapter::runtime::Runtime;
use oxap_adapter::testkit::{drain, next, RunTrace};
use oxap_adapter::{EventStream, InteractionResolution, OpenRequest};
use oxap_adapter_codex_appserver::adapter::pin;
use oxap_adapter_codex_appserver::rpc::{Inbound, RequestId};
use oxap_adapter_codex_appserver::session::Session;
use oxap_protocol::{
    Envelope, EnvelopeType, InputAnswer, Message, MessageContent, MessageRole,
    MessageSubmitRequest, MessageSubmitResponse, Participant, PermissionResolveRequest,
    RequestedDeliveryMode, UserInputResolveRequest,
};
use serde::Deserialize;
use serde_json::{value::RawValue, Value};
use support::{adapter, Fake};

const WAIT: Duration = Duration::from_secs(5);
const STARTED: &str =
    r#"{"threadId":"native-thread","turn":{"id":"native-turn","status":"inProgress"}}"#;

#[derive(Deserialize)]
struct Manifest {
    cases: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    path: String,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    native: String,
    expected_oap: String,
    model_id: Option<String>,
    output_schema: Option<Box<RawValue>>,
}

#[derive(Deserialize)]
struct Frame {
    direction: String,
    kind: String,
    method: String,
    params: Option<Box<RawValue>>,
    id: Option<i64>,
    error: Option<String>,
    #[serde(default)]
    await_events: usize,
    resolve: Option<Resolution>,
    expected_result: Option<Value>,
    expected_error: Option<ExpectedError>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Resolution {
    Permission { choice_id: String, granted: bool },
    Input { answers: Vec<InputAnswer> },
}

#[derive(Deserialize)]
struct ExpectedError {
    code: i64,
    message: String,
}

fn resolve(
    session: &Session,
    admission: &MessageSubmitResponse,
    events: &[Envelope],
    resolution: Resolution,
) -> usize {
    let event = events
        .iter()
        .rev()
        .find(|event| {
            event.envelope_type == EnvelopeType::ACTION_PERMISSION_REQUESTED
                || event.envelope_type == EnvelopeType::USER_INPUT_REQUESTED
        })
        .unwrap();
    let payload: Value = serde_json::from_str(event.payload.as_ref().unwrap().get()).unwrap();
    let interaction_id = payload["interaction_id"].as_str().unwrap().into();
    let requested_by = "codex.app-server".into();
    let responded_by = "user".into();
    let session_id = admission.session_id.clone();
    let run_id = admission.run_id.clone().unwrap();
    let (permission, input, count) = match resolution {
        Resolution::Permission { choice_id, granted } => (
            Some(PermissionResolveRequest {
                interaction_id,
                requested_by,
                responded_by,
                session_id,
                run_id,
                choice_id,
                granted,
                ..Default::default()
            }),
            None,
            1,
        ),
        Resolution::Input { answers } => (
            None,
            Some(UserInputResolveRequest {
                interaction_id,
                requested_by,
                responded_by,
                session_id,
                run_id,
                answers,
            }),
            2,
        ),
    };
    session
        .resolve(InteractionResolution {
            run_id: admission.run_id.clone().unwrap(),
            responded_by: "user".into(),
            permission,
            input,
        })
        .unwrap();
    count
}

fn take(stream: &mut EventStream, events: &mut Vec<Envelope>, count: usize) {
    events.extend((0..count).map(|_| next(stream, WAIT)));
}

fn replay(root: &Path, entry: &Entry) {
    let dir = root.join(&entry.path);
    let case: Case = serde_json::from_slice(&fs::read(dir.join("case.json")).unwrap()).unwrap();
    assert_eq!(case.id, entry.id);
    let native = fs::read_to_string(dir.join(case.native)).unwrap();
    let mut frames = native
        .lines()
        .map(|line| serde_json::from_str::<Frame>(line).unwrap());
    let expected = fs::read_to_string(dir.join(case.expected_oap)).unwrap();

    let fake = Fake::new([
        Ok(r#"{"thread":{"id":"native-thread"}}"#),
        Ok(r#"{"turn":{"id":"native-turn","status":"inProgress"}}"#),
    ]);
    fake.always_reply("turn/interrupt", Ok("{}"));
    let clock = Arc::new(AtomicI64::new(0));
    let ids = Arc::new(AtomicU64::new(0));
    let adapter = adapter(&fake, |config| {
        config.model = "glm-test".into();
        config.journal_capacity = 32;
        config.runtime = Runtime::new(
            move || clock.fetch_add(1, Ordering::SeqCst) + 1,
            move |kind| format!("{kind}-{:02}", ids.fetch_add(1, Ordering::SeqCst) + 1),
        );
    });
    let session = adapter
        .open(OpenRequest {
            session_id: "session-1".into(),
            participant: Participant {
                id: "user".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
    let submit = MessageSubmitRequest {
        session_id: "session-1".into(),
        messages: vec![Message {
            role: MessageRole::USER,
            content: MessageContent::from_text("hello"),
            ..Default::default()
        }],
        delivery: RequestedDeliveryMode::AUTO,
        model_id: case.model_id,
        output_schema: case.output_schema,
        ..Default::default()
    };
    let (admission, mut stream) = session.submit(submit.clone()).unwrap();
    let calls = fake.calls();
    let (method, params) = &calls[1];
    assert_eq!(method, "turn/start");
    let params: Value = serde_json::from_str(params).unwrap();
    if let Some(model) = &submit.model_id {
        assert_eq!(params["model"], model.as_str());
        assert_eq!(admission.model_id, *model);
        assert_eq!(session.state().unwrap().current_model_id, "glm-test");
    }
    if let Some(schema) = &submit.output_schema {
        assert_eq!(
            params["outputSchema"],
            serde_json::from_str::<Value>(schema.get()).unwrap()
        );
    }

    let mut events = Vec::new();
    let cancelled = entry.id == "interrupted-turn";
    if entry.id == "process-exit" {
        fake.notify("turn/started", STARTED);
        take(&mut stream, &mut events, 1);
    }
    if cancelled {
        let first = frames.next().unwrap();
        assert_eq!(first.method, "turn/started");
        fake.notify(&first.method, first.params.unwrap().get());
        take(&mut stream, &mut events, 1);
        assert!(
            session
                .cancel(admission.run_id.as_ref().unwrap())
                .unwrap()
                .accepted
        );
        take(&mut stream, &mut events, 1);
    }
    for frame in frames {
        match (frame.direction.as_str(), frame.kind.as_str()) {
            ("server_to_client", "notification") => {
                fake.notify(&frame.method, frame.params.unwrap().get());
                take(&mut stream, &mut events, frame.await_events);
            }
            ("server_to_client", "request") => {
                let id = frame.id.unwrap();
                let answers = fake.request_with(id, &frame.method, frame.params.unwrap().get());
                let first = events.len();
                take(&mut stream, &mut events, frame.await_events.max(1));
                if let Some(resolution) = frame.resolve {
                    let count = resolve(&session, &admission, &events[first..], resolution);
                    take(&mut stream, &mut events, count);
                }
                let (ack, acked) = crossbeam_channel::bounded(0);
                fake.deliver(Inbound::Barrier(ack)).unwrap();
                assert_eq!(
                    acked.recv_timeout(WAIT),
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected)
                );
                let answers = answers.0.lock().unwrap();
                assert_eq!(answers.len(), 1);
                let answer = &answers[0];
                assert_eq!(answer.id, Some(RequestId::Integer(id)));
                match (frame.expected_result, frame.expected_error) {
                    (Some(want), None) => {
                        assert!(answer.error.is_none());
                        assert_eq!(
                            serde_json::from_str::<Value>(answer.result.as_ref().unwrap().get())
                                .unwrap(),
                            want
                        );
                    }
                    (None, Some(want)) => {
                        assert!(answer.result.is_none());
                        let got = answer.error.as_ref().unwrap();
                        assert_eq!(
                            (got.code, got.message.as_str()),
                            (want.code, want.message.as_str())
                        );
                    }
                    _ => panic!("missing or ambiguous reply for {}", frame.method),
                }
            }
            ("process", "transport") => {
                *fake.failed.lock().unwrap() = Some(frame.error.unwrap());
                fake.hang_up();
            }
            _ => panic!(
                "unsupported corpus frame {}/{}",
                frame.direction, frame.kind
            ),
        }
    }
    events.extend(drain(&mut stream, WAIT));
    assert_eq!(
        go_marshal(&serde_json::to_string(&events).unwrap()),
        go_marshal(&expected),
    );
    RunTrace {
        submitted: Some(submit),
        cancelled,
        ..RunTrace::new(admission, adapter.describe(), events)
    }
    .assert_valid();
    session.close().unwrap();
}

#[test]
fn shared_codex_corpus_replays_byte_for_byte() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(&pin().corpus);
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert!(!manifest.cases.is_empty());
    let mut failed = Vec::new();
    for entry in &manifest.cases {
        eprintln!("replay {}", entry.id);
        if std::panic::catch_unwind(|| replay(&root, entry)).is_err() {
            failed.push(entry.id.as_str());
        }
    }
    eprintln!(
        "Codex corpus: {}/{} cases passed",
        manifest.cases.len() - failed.len(),
        manifest.cases.len()
    );
    assert!(failed.is_empty(), "failed cases: {failed:?}");
}
