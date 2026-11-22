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
