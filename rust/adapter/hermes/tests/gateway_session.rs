#![cfg(unix)]

use std::time::{Duration, Instant};

use oxap_adapter::{Error, CONTROL_UNSATISFIABLE};
use oxap_adapter_hermes::process::ProcessConfig;
use oxap_adapter_hermes::rpc::Inbound;
use oxap_adapter_hermes::GatewaySession;

const READY: &str = r#"{"jsonrpc":"2.0","method":"event","params":{"type":"gateway.ready","payload":{"skin":{"name":"default"},"change_events":true,"replay_epoch":"0123456789abcdef0123456789abcdef"}}}"#;
const INFO: &str = r#"{"jsonrpc":"2.0","method":"event","params":{"type":"session.info","session_id":"running","seq":1,"payload":{"model":"example"}}}"#;
const CREATED: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"session_id":"running","stored_session_id":"saved","extra":true}}"#;
const EMPTY_CREATE: &str = r#"{"id":1,"jsonrpc":"2.0","method":"session.create","params":{}}"#;
const CREATE_WITH_REASONING: &str = r#"{"id":1,"jsonrpc":"2.0","method":"session.create","params":{"model":"example","reasoning_effort":"none"}}"#;
const SET_REASONING: &str = r#"{"id":2,"jsonrpc":"2.0","method":"config.set","params":{"session_id":"running","key":"reasoning","value":"none"}}"#;
const SUBMIT: &str = r#"{"id":2,"jsonrpc":"2.0","method":"prompt.submit","params":{"session_id":"running","text":"hello\n\u003cworld\u003e"}}"#;
const INTERRUPT: &str =
    r#"{"id":3,"jsonrpc":"2.0","method":"session.interrupt","params":{"session_id":"running"}}"#;
const STARTED: &str = r#"{"jsonrpc":"2.0","method":"event","params":{"type":"message.start","session_id":"running","seq":2,"payload":{}}}"#;

fn gateway(create: &str, replies: &str, subsequent: &str) -> ProcessConfig {
    ProcessConfig {
        path: "/bin/sh".into(),
        args: vec![
            "-ec".into(),
            format!(
                "printf '%s\\n' '{READY}'\n\
                 IFS= read -r line\n\
                 test \"$line\" = '{create}'\n\
                 {replies}\n\
                 {subsequent}\n\
                 if IFS= read -r line; then exit 9; fi"
            ),
        ],
        env: Some(Vec::new()),
        exit_timeout: Some(Duration::from_secs(2)),
        ..ProcessConfig::default()
    }
}

fn open(config: &ProcessConfig, model: &str, reasoning: &str) -> Result<GatewaySession, Error> {
    GatewaySession::create(
        config,
        model,
        &reasoning.into(),
        Some(Instant::now() + Duration::from_secs(5)),
    )
}

#[test]
fn create_keeps_both_ids_and_an_observation_before_the_response() {
    let config = gateway(
        CREATE_WITH_REASONING,
        &format!("printf '%s\\n' '{INFO}' '{CREATED}'"),
        &format!(
            "IFS= read -r line\ntest \"$line\" = '{SET_REASONING}'\n\
             printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{}}}}'"
        ),
    );
    let session = open(&config, "example", "off").unwrap();
    assert_eq!(session.runtime_id(), "running");
    assert_eq!(session.stored_id(), "saved");
    let observed = session
        .inbound()
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let Inbound::Notification(event) = observed else {
        panic!("expected session.info, got {observed:?}");
    };
    assert_eq!(event.method, "event");
    assert!(event
        .params
        .unwrap()
        .get()
        .contains(r#""type":"session.info""#));
    session.close().unwrap();
}

#[test]
fn create_rejects_a_missing_id_and_omits_unset_options() {
    for result in [
        r#"{"session_id":"running"}"#,
        r#"{"stored_session_id":"saved"}"#,
        "null",
    ] {
        let config = gateway(
            EMPTY_CREATE,
            &format!("printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{result}}}'"),
            "",
        );
        let err = open(&config, "", "").err().expect("missing id accepted");
        assert_eq!(
            err.to_string(),
            "hermes adapter: invalid native protocol observation"
        );
    }
}

#[test]
fn create_reports_reasoning_refusal_as_an_unsatisfiable_control() {
    let config = gateway(
        CREATE_WITH_REASONING,
        &format!("printf '%s\\n' '{CREATED}'"),
        &format!(
            "IFS= read -r line\ntest \"$line\" = '{SET_REASONING}'\n\
             printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{{\"code\":-32602,\"message\":\"invalid effort\"}}}}'"
        ),
    );
    let Err(Error::UnsupportedControl(control)) = open(&config, "example", "off") else {
        panic!("expected the gateway's reasoning refusal");
    };
    assert_eq!(control.reason, CONTROL_UNSATISFIABLE);
    assert_eq!(control.field, "reasoning_level");
    assert!(control.detail.contains("invalid effort"));
}

#[test]
fn submit_and_interrupt_use_the_runtime_id_and_keep_early_observations() {
    let config = gateway(
        EMPTY_CREATE,
        &format!("printf '%s\\n' '{CREATED}'"),
        &format!(
            "IFS= read -r line\ntest \"$line\" = '{SUBMIT}'\n\
             printf '%s\\n' '{STARTED}' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"status\":\"streaming\",\"turn_isolation\":true}}}}'\n\
             IFS= read -r line\ntest \"$line\" = '{INTERRUPT}'\n\
             printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{{\"status\":\"interrupted\"}}}}'"
        ),
    );
    let session = open(&config, "", "").unwrap();
    let deadline = Some(Instant::now() + Duration::from_secs(5));
    session.submit_prompt("hello\n<world>", deadline).unwrap();
    let observed = session
        .inbound()
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let Inbound::Notification(event) = observed else {
        panic!("expected message.start, got {observed:?}");
    };
    assert!(event
        .params
        .unwrap()
        .get()
        .contains(r#""type":"message.start""#));
    let result = session.interrupt(deadline).unwrap();
    assert_eq!(result.status, "interrupted");
    session.close().unwrap();
}

#[test]
fn submit_refuses_non_streaming_results_and_gateway_errors() {
    for (reply, expected) in [
        (
            r#""result":{"status":"queued"}"#,
            "returned status \"queued\"",
        ),
        (
            r#""result":{"status":"steered"}"#,
            "returned status \"steered\"",
        ),
        (
            r#""result":{"status":"redirected"}"#,
            "returned status \"redirected\"",
        ),
        (r#""result":{}"#, "returned status \"\""),
        (
            r#""result":{"status":"streaming","turn_isolation":"bad"}"#,
            "decode prompt.submit response",
        ),
        (
            r#""error":{"code":5004,"message":"voice session active"}"#,
            "voice session active",
        ),
    ] {
        let config = gateway(
            EMPTY_CREATE,
            &format!("printf '%s\\n' '{CREATED}'"),
            &format!(
                "IFS= read -r line\ntest \"$line\" = '{SUBMIT}'\n\
                 printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":2,{reply}}}'"
            ),
        );
        let session = open(&config, "", "").unwrap();
        let error = session
            .submit_prompt(
                "hello\n<world>",
                Some(Instant::now() + Duration::from_secs(5)),
            )
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
        session.close().unwrap();
    }
}

#[test]
fn ordered_observations_keep_early_session_events_and_interleaved_requests() {
    use oxap_adapter_hermes::Observation;

    let global = r#"{"jsonrpc":"2.0","method":"event","params":{"type":"notice"}}"#;
    let request =
        r#"{"jsonrpc":"2.0","id":"check","method":"approval","params":{"session_id":"running"}}"#;
    let config = gateway(
        EMPTY_CREATE,
        &format!("printf '%s\\n' '{INFO}' '{global}' '{CREATED}'"),
        &format!(
            "IFS= read -r line\ntest \"$line\" = '{SUBMIT}'\n\
             printf '%s\\n' '{request}' '{STARTED}' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"status\":\"streaming\"}}}}'"
        ),
    );
    let mut session = open(&config, "", "").unwrap();
    let deadline = Some(Instant::now() + Duration::from_secs(5));
    session.submit_prompt("hello\n<world>", deadline).unwrap();
    assert_eq!(
        session
            .next_observation(Some(Instant::now()))
            .unwrap_err()
            .to_string(),
        "hermes adapter: observation deadline exceeded"
    );
    let Observation::Event(info) = session.next_observation(deadline).unwrap() else {
        panic!("expected session.info");
    };
    assert_eq!((info.kind.as_str(), info.seq), ("session.info", 1));
    let Observation::Request(request) = session.next_observation(deadline).unwrap() else {
        panic!("expected the interleaved request");
    };
    assert_eq!(request.method, "approval");
    let Observation::Event(start) = session.next_observation(deadline).unwrap() else {
        panic!("expected message.start");
    };
    assert_eq!((start.kind.as_str(), start.seq), ("message.start", 2));
    session.close().unwrap();
}

#[test]
fn ordered_observations_refuse_foreign_events_and_sequence_breaks() {
    for (id, seq, expected) in [
        ("saved", 2, "event for foreign session \"saved\""),
        ("running", 1, "non-contiguous seq 1 after 1"),
        ("running", 3, "non-contiguous seq 3 after 1"),
    ] {
        let invalid = format!(
            r#"{{"jsonrpc":"2.0","method":"event","params":{{"type":"notice","session_id":"{id}","seq":{seq}}}}}"#
        );
        let config = gateway(
            EMPTY_CREATE,
            &format!("printf '%s\\n' '{INFO}' '{CREATED}' '{invalid}' '{STARTED}'"),
            "",
        );
        let mut session = open(&config, "", "").unwrap();
        let deadline = Some(Instant::now() + Duration::from_secs(5));
        session.next_observation(deadline).unwrap();
        let error = format!("hermes adapter: invalid native protocol observation: {expected}");
        for _ in 0..2 {
            assert_eq!(
                session.next_observation(deadline).unwrap_err().to_string(),
                error
            );
        }
        session.close().unwrap();
    }
}

#[test]
fn prompt_start_requires_both_receipt_and_open_in_either_order() {
    use oxap_adapter_hermes::Observation;

    let receipt = r#"{"jsonrpc":"2.0","id":2,"result":{"status":"streaming"}}"#;
    let request =
        r#"{"jsonrpc":"2.0","id":"check","method":"approval","params":{"session_id":"running"}}"#;
    let mirror = r#"{"jsonrpc":"2.0","method":"event","params":{"type":"message.complete","session_id":"running","seq":2,"payload":{"text":"child"}}}"#;
    let start = STARTED.replace(r#""seq":2"#, r#""seq":3"#);
    for (first, second) in [(receipt, start.as_str()), (start.as_str(), receipt)] {
        let config = gateway(
            EMPTY_CREATE,
            &format!("printf '%s\\n' '{INFO}' '{CREATED}'"),
            &format!(
                "IFS= read -r line\ntest \"$line\" = '{SUBMIT}'\n\
                 printf '%s\\n' '{request}' '{mirror}' '{first}' '{second}'"
            ),
        );
        let mut session = open(&config, "", "").unwrap();
        let deadline = Some(Instant::now() + Duration::from_secs(5));
        let start = session.start_prompt("hello\n<world>", deadline).unwrap();
        assert_eq!((start.kind.as_str(), start.seq), ("message.start", 3));
        let Observation::Event(info) = session.next_observation(deadline).unwrap() else {
            panic!("expected the buffered session.info");
        };
        assert_eq!((info.kind.as_str(), info.seq), ("session.info", 1));
        let Observation::Request(observed) = session.next_observation(deadline).unwrap() else {
            panic!("expected the buffered reverse request");
        };
        assert_eq!(observed.method, "approval");
        let Observation::Event(child) = session.next_observation(deadline).unwrap() else {
            panic!("expected the buffered child-mirror settlement");
        };
        assert_eq!((child.kind.as_str(), child.seq), ("message.complete", 2));
        session.close().unwrap();
    }
}

#[test]
fn prompt_settling_before_open_does_not_start() {
    use oxap_adapter_hermes::Observation;

    let complete = r#"{"jsonrpc":"2.0","method":"event","params":{"type":"message.complete","session_id":"running","seq":2,"payload":{"status":"error","error":"no model"}}}"#;
    let config = gateway(
        EMPTY_CREATE,
        &format!("printf '%s\\n' '{INFO}' '{CREATED}'"),
        &format!(
            "IFS= read -r line\ntest \"$line\" = '{SUBMIT}'\n\
             printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"status\":\"streaming\"}}}}' '{complete}'"
        ),
    );
    let mut session = open(&config, "", "").unwrap();
    let deadline = Some(Instant::now() + Duration::from_secs(5));
    let error = session
        .start_prompt("hello\n<world>", deadline)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("settlement before the turn opened"));
    for (kind, seq) in [("session.info", 1), ("message.complete", 2)] {
        let Observation::Event(event) = session.next_observation(deadline).unwrap() else {
            panic!("expected buffered {kind}");
        };
        assert_eq!((event.kind.as_str(), event.seq), (kind, seq));
    }
    session.close().unwrap();
}

#[test]
fn started_prompt_projects_deltas_and_settlement() {
    use oxap_adapter_hermes::{Observation, PromptObservation};

    let receipt = r#"{"jsonrpc":"2.0","id":2,"result":{"status":"streaming"}}"#;
    let event = |kind: &str, seq: i64, payload: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","method":"event","params":{{"type":"{kind}","session_id":"running","seq":{seq},"payload":{payload}}}}}"#
        )
    };
    for (payload, failure) in [
        (
            r#"{"status":"complete","text":"final","usage":{"input":0,"total":7}}"#,
            None,
        ),
        (
            r#"{"status":"error","error":"refused","text":"fallback","error_surface":{"code":"auth"}}"#,
            Some(("hermes_auth", "refused")),
        ),
        (
            r#"{"status":"interrupted"}"#,
            Some(("hermes_interrupted", "turn interrupted")),
        ),
        (
            r#"{"text":"child"}"#,
            Some((
                "hermes_invalid_settlement",
                "child-mirror settlement on the parent stream",
            )),
        ),
        (
            r#"{"status":"complete","usage":{"output":-1}}"#,
            Some(("hermes_invalid_event", "invalid settlement")),
        ),
    ] {
        let text = event(
            "message.delta",
            3,
            r#"{"text":"answer","rendered":"preview"}"#,
        );
        let empty = event("message.delta", 4, r#"{"text":""}"#);
        let reasoning = event("reasoning.delta", 5, r#"{"text":"thinking"}"#);
        let terminal = event("message.complete", 6, payload);
        let config = gateway(
            EMPTY_CREATE,
            &format!("printf '%s\\n' '{INFO}' '{CREATED}'"),
            &format!(
                "IFS= read -r line\ntest \"$line\" = '{SUBMIT}'\n\
                 printf '%s\\n' '{receipt}' '{STARTED}' '{text}' '{empty}' '{reasoning}' '{terminal}'"
            ),
        );
        let mut session = open(&config, "", "").unwrap();
        let deadline = Some(Instant::now() + Duration::from_secs(5));
        session.start_prompt("hello\n<world>", deadline).unwrap();
        let PromptObservation::Other(Observation::Event(info)) =
            session.next_prompt_observation(deadline).unwrap()
        else {
            panic!("expected buffered session info");
        };
        assert_eq!(info.kind, "session.info");
        for expected in [
            serde_json::json!({"type":"text","text":"answer"}),
            serde_json::json!({"type":"reasoning","reasoning":"thinking"}),
        ] {
            let PromptObservation::Delta(part) = session.next_prompt_observation(deadline).unwrap()
            else {
                panic!("expected a content delta");
            };
            assert_eq!(serde_json::to_value(part).unwrap(), expected);
        }
        let terminal = session.next_prompt_observation(deadline).unwrap();
        match (terminal, failure) {
            (PromptObservation::Completed { text, usage }, None) => {
                assert_eq!(text, "final");
                assert_eq!(
                    serde_json::to_value(usage).unwrap(),
                    serde_json::json!({"input_tokens":0,"total_tokens":7})
                );
            }
            (PromptObservation::Failed(error), Some((code, message))) => {
                assert_eq!(
                    (error.code.as_str(), error.message.as_str()),
                    (code, message)
                );
            }
            (terminal, _) => panic!("unexpected settlement: {terminal:?}"),
        }
        session.close().unwrap();
    }
}

#[test]
fn run_replays_streaming_provenance_byte_for_byte() {
    replay_run(
        "streaming-provenance",
        &["thinking.delta", "message.interim"],
    );
}

#[test]
fn run_replays_tool_lifecycle_byte_for_byte() {
    replay_run("tool-lifecycle", &[]);
}

const HERMES_CORPUS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/adapters/hermes-v0.23.3/"
);

fn replay_run(case: &str, expected_observations: &[&str]) {
    use oxap_adapter::gojson::go_marshal;

    let native = std::fs::read_to_string(format!("{HERMES_CORPUS}{case}/native.jsonl")).unwrap();
    let (trace, observed) = replay_frames(native.lines());
    let trace: Vec<_> = trace
        .iter()
        .map(|envelope| go_marshal(&serde_json::to_string(envelope).unwrap()))
        .collect();
    let expected =
        std::fs::read_to_string(format!("{HERMES_CORPUS}{case}/expected-oap.json")).unwrap();
    assert_eq!(format!("[{}]", trace.join(",")), go_marshal(&expected));
    assert_eq!(observed, expected_observations);
}

#[test]
fn run_settles_open_tools_before_run_settlement() {
    use oxap_protocol::{ActionCallPayload, EnvelopeType};

    let native =
        std::fs::read_to_string(format!("{HERMES_CORPUS}tool-lifecycle/native.jsonl")).unwrap();
    let frames: Vec<_> = native.lines().collect();
    let turn_end = frames[8].replace(r#""seq":6"#, r#""seq":3"#);
    let (trace, _) = replay_frames(frames[..5].iter().copied().chain([turn_end.as_str()]));
    assert_eq!(trace.len(), 5);
    let [_, requested, started, failed, completed] = trace.as_slice() else {
        unreachable!()
    };
    assert_eq!(failed.envelope_type, EnvelopeType::ACTION_CALL_FAILED);
    assert_eq!(failed.in_reply_to.as_ref(), Some(&started.id));
    assert_eq!(failed.tool_call_id, requested.tool_call_id);
    let payload: ActionCallPayload = failed.decode_payload().unwrap();
    assert_eq!(payload.error.unwrap().code, "incomplete_tool");
    assert_eq!(completed.envelope_type, EnvelopeType::RUN_COMPLETED);
}

#[test]
fn run_settles_open_tools_on_disconnect_after_read_deadline() {
    use oxap_adapter::runtime::Runtime;
    use oxap_adapter_hermes::RunObservation;
    use oxap_protocol::{EnvelopeType, RunFailedPayload};

    let config = gateway(
        EMPTY_CREATE,
        &format!("printf '%s\\n' '{INFO}' '{CREATED}'"),
        &format!(
            "IFS= read -r line\ntest \"$line\" = '{SUBMIT}'\n\
             printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"status\":\"streaming\"}}}}' '{STARTED}'\n\
             printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"method\":\"event\",\"params\":{{\"type\":\"tool.start\",\"session_id\":\"running\",\"seq\":3,\"payload\":{{\"tool_id\":\"tool-1\",\"name\":\"search\"}}}}}}'\n\
             exit 0"
        ),
    );
    let mut session = open(&config, "", "").unwrap();
    let deadline = Some(Instant::now() + Duration::from_secs(5));
    let mut run = session
        .start_run(
            "hello\n<world>",
            "session".into(),
            "example",
            Runtime::default(),
            deadline,
        )
        .unwrap();
    let Some(RunObservation::Event(started)) = run.next_observation(deadline).unwrap() else {
        panic!("expected a run start");
    };
    assert_eq!(started.envelope_type, EnvelopeType::RUN_STARTED);
    let error = run.next_observation(Some(Instant::now())).unwrap_err();
    assert_eq!(
        error.to_string(),
        "hermes adapter: observation deadline exceeded"
    );
    let mut envelopes = Vec::new();
    while let Some(observation) = run.next_observation(deadline).unwrap() {
        if let RunObservation::Event(event) = observation {
            envelopes.push(event);
        }
    }
    let [requested, started, failed, terminal] = envelopes.as_slice() else {
        panic!("expected four envelopes after the read deadline, got {envelopes:?}");
    };
    assert_eq!(requested.envelope_type, EnvelopeType::ACTION_CALL_REQUESTED);
    assert_eq!(started.envelope_type, EnvelopeType::ACTION_CALL_STARTED);
    assert_eq!(failed.envelope_type, EnvelopeType::ACTION_CALL_FAILED);
    assert_eq!(failed.in_reply_to.as_ref(), Some(&started.id));
    assert_eq!(terminal.envelope_type, EnvelopeType::RUN_FAILED);
    let payload: RunFailedPayload = terminal.decode_payload().unwrap();
    assert_eq!(payload.error.code, "hermes_process_exit");
    assert_eq!(payload.error.message, "EOF");
    assert_eq!(payload.settled_by, "inferred");
    session.close().unwrap();
}

#[test]
fn run_refuses_duplicate_starts_and_unmatched_completions() {
    use oxap_protocol::{EnvelopeType, RunFailedPayload};

    let native =
        std::fs::read_to_string(format!("{HERMES_CORPUS}tool-lifecycle/native.jsonl")).unwrap();
    let frames: Vec<_> = native.lines().collect();
    let duplicate = frames[4].replace(r#""seq":2"#, r#""seq":3"#);
    let unmatched = frames[5].replace(r#""seq":3"#, r#""seq":2"#);
    for (first, last, message, count) in [
        (5, duplicate.as_str(), "duplicate tool call", 5),
        (4, unmatched.as_str(), "unmatched tool completion", 2),
    ] {
        let (trace, _) = replay_frames(frames[..first].iter().copied().chain([last]));
        assert_eq!(trace.len(), count);
        let failed = trace.last().unwrap();
        assert_eq!(failed.envelope_type, EnvelopeType::RUN_FAILED);
        let payload: RunFailedPayload = failed.decode_payload().unwrap();
        assert_eq!(payload.error.code, "hermes_tool_lifecycle");
        assert_eq!(payload.error.message, message);
        if count == 5 {
            assert_eq!(trace[3].envelope_type, EnvelopeType::ACTION_CALL_FAILED);
        }
    }
}

fn replay_frames<'a>(
    frames: impl IntoIterator<Item = &'a str>,
) -> (Vec<oxap_protocol::Envelope>, Vec<String>) {
    use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};

    use oxap_adapter::runtime::Runtime;
    use oxap_adapter_hermes::RunObservation;
    use serde::Deserialize;
    use serde_json::value::RawValue;

    #[derive(Deserialize)]
    struct Frame {
        direction: String,
        raw: Box<RawValue>,
    }

    let mut script = String::new();
    for frame in frames {
        let frame: Frame = serde_json::from_str(frame).unwrap();
        let wire = frame.raw.get();
        match frame.direction.as_str() {
            "host-to-gateway" => {
                script.push_str(&format!("IFS= read -r line\ntest \"$line\" = '{wire}'\n"));
            }
            "gateway-to-host" => script.push_str(&format!("printf '%s\\n' '{wire}'\n")),
            _ => {}
        }
    }
    let created = r#"{"jsonrpc":"2.0","id":1,"result":{"session_id":"sess0001","stored_session_id":"stored"}}"#;
    let config = gateway(
        EMPTY_CREATE,
        &format!("printf '%s\\n' '{created}'"),
        &script,
    );
    let mut session = open(&config, "", "").unwrap();
    let now = AtomicI64::new(2);
    let next = AtomicU8::new(b'a');
    let runtime = Runtime::new(
        move || now.fetch_add(1, Ordering::Relaxed),
        move |kind| {
            format!(
                "{kind}-{}",
                char::from(next.fetch_add(1, Ordering::Relaxed))
            )
        },
    );
    let deadline = Some(Instant::now() + Duration::from_secs(5));
    let mut run = session
        .start_run("hello", "session".into(), "hermes-test", runtime, deadline)
        .unwrap();
    let mut trace = Vec::new();
    let mut observed = Vec::new();
    while let Some(item) = run.next_observation(deadline).unwrap() {
        match item {
            RunObservation::Event(envelope) => {
                trace.push(*envelope);
            }
            RunObservation::Other(oxap_adapter_hermes::Observation::Event(event)) => {
                observed.push(event.kind);
            }
            item => panic!("unexpected observation: {item:?}"),
        }
    }
    assert!(run.next_observation(deadline).unwrap().is_none());
    assert!(matches!(
        run.interrupt(deadline),
        Err(Error::RunAlreadyTerminal { .. })
    ));
    session.close().unwrap();
    (trace, observed)
}

#[test]
fn run_rejects_invalid_answers_then_submits_a_clarification_batch_once() {
    use oxap_adapter::runtime::Runtime;
    use oxap_adapter_hermes::RunObservation;
    use oxap_protocol::{
        EnvelopeType, InputAnswer, RunStatus, RunStatusUpdatedPayload, UserInputRequestedPayload,
        UserInputResolutionStatus, UserInputResolveRequest, UserInputResolvedPayload,
    };

    let request = r#"{"jsonrpc":"2.0","id":"gate","method":"clarify","params":{"session_id":"running","questions":[{"qid":"detail","question":"Why?"},{"qid":"items","question":"Which?","choices":["a","b&c"],"multi_select":true}]}}"#;
    let response = r#"{"id":"gate","jsonrpc":"2.0","result":{"answers":{"detail":"because","items":"[\"a\",\"b\\u0026c\"]"}}}"#;
    let complete = r#"{"jsonrpc":"2.0","method":"event","params":{"type":"message.complete","session_id":"running","seq":3,"payload":{"status":"complete","text":"done"}}}"#;
    let config = gateway(
        EMPTY_CREATE,
        &format!("printf '%s\\n' '{INFO}' '{CREATED}'"),
        &format!(
            "IFS= read -r line\ntest \"$line\" = '{SUBMIT}'\n\
             printf '%s\\n' '{STARTED}' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"status\":\"streaming\"}}}}' '{request}'\n\
             IFS= read -r line\ntest \"$line\" = '{response}'\n\
             printf '%s\\n' '{complete}'"
        ),
    );
    let mut session = open(&config, "", "").unwrap();
    let deadline = Some(Instant::now() + Duration::from_secs(5));
    let mut run = session
        .start_run(
            "hello\n<world>",
            "session".into(),
            "example",
            Runtime::default(),
            deadline,
        )
        .unwrap();
    let next = |run: &mut oxap_adapter_hermes::GatewayRun<'_>| loop {
        if let RunObservation::Event(event) = run.next_observation(deadline).unwrap().unwrap() {
            break event;
        }
    };
    assert_eq!(next(&mut run).envelope_type, EnvelopeType::RUN_STARTED);
    let requested = next(&mut run);
    assert_eq!(requested.envelope_type, EnvelopeType::USER_INPUT_REQUESTED);
    let input: UserInputRequestedPayload = requested.decode_payload().unwrap();
    let waiting: RunStatusUpdatedPayload = next(&mut run).decode_payload().unwrap();
    assert_eq!(
        waiting.pending_user_input_id,
        Some(input.interaction_id.clone())
    );

    let answer = UserInputResolveRequest {
        interaction_id: input.interaction_id,
        requested_by: input.requested_by,
        responded_by: input.responded_by,
        session_id: input.session_id,
        run_id: input.run_id,
        answers: vec![
            InputAnswer {
                question_id: "items".into(),
                selected_option_ids: vec!["a".into(), "b&c".into()],
                ..InputAnswer::default()
            },
            InputAnswer {
                question_id: "detail".into(),
                text: "because".into(),
                ..InputAnswer::default()
            },
        ],
    };
    let mut invalid = answer.clone();
    invalid.responded_by = "someone-else".into();
    assert_eq!(run.resolve_input(invalid), Err(Error::InvalidResolution));
    let mut invalid = answer.clone();
    invalid.answers[0].selected_option_ids = vec!["unoffered".into()];
    assert_eq!(run.resolve_input(invalid), Err(Error::InvalidResolution));
    let mut invalid = answer.clone();
    invalid.answers.pop();
    assert_eq!(run.resolve_input(invalid), Err(Error::InvalidResolution));
    run.resolve_input(answer.clone()).unwrap();
    assert_eq!(
        run.resolve_input(answer.clone()),
        Err(Error::InteractionNotFound)
    );

    let resolved = next(&mut run);
    assert_eq!(resolved.envelope_type, EnvelopeType::USER_INPUT_RESOLVED);
    assert_eq!(resolved.in_reply_to, Some(requested.id));
    let resolved: UserInputResolvedPayload = resolved.decode_payload().unwrap();
    assert_eq!(resolved.status, UserInputResolutionStatus::SUBMITTED);
    assert_eq!(resolved.answers, answer.answers);
    let running: RunStatusUpdatedPayload = next(&mut run).decode_payload().unwrap();
    assert_eq!(running.status, RunStatus::RUNNING);
    assert_eq!(next(&mut run).envelope_type, EnvelopeType::RUN_COMPLETED);
    assert!(run.next_observation(deadline).unwrap().is_none());
    session.close().unwrap();
}
