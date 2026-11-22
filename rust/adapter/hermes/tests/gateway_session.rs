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
