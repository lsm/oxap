mod support;

use futures::executor::block_on;
use oxap_adapter::OpenRequest;
use oxap_serve::stdio::{serve, StdioOptions};
use oxap_serve::{Hub, Registry};
use serde_json::Value;

fn opened() -> Hub {
    let hub = Hub::new(Registry::default_registry());
    block_on(hub.open(
        "memory",
        OpenRequest {
            session_id: "s".into(),
            ..OpenRequest::default()
        },
    ))
    .unwrap();
    hub
}

fn request(envelope_session: &str, payload_session: &str) -> String {
    format!(
        "{{\"protocol\":\"open-agent-protocol\",\"version\":\"0.1\",\"profile\":\"open-agent-protocol.agent-control-core\",\"type\":\"session.task.output.request\",\"id\":\"req-1\",\"session_id\":\"{envelope_session}\",\"capability_revision\":\"rev-cited\",\"payload\":{{\"session_id\":\"{payload_session}\",\"task_id\":\"task-1\",\"from\":3}}}}"
    )
}

fn answer(hub: &Hub, request: &str) -> Value {
    let line =
        format!("{{\"id\":9,\"op\":\"task_output\",\"session_id\":\"s\",\"request\":{request}}}\n");
    let output = support::Captured::default();
    serve(
        hub,
        std::io::Cursor::new(line.into_bytes()),
        output.clone(),
        &StdioOptions::default(),
    )
    .unwrap();
    serde_json::from_slice(&output.bytes()).unwrap()
}

#[test]
fn a_session_whose_adapter_reads_no_task_output_refuses_it_as_an_unsupported_feature() {
    let hub = opened();
    let refused = answer(&hub, &request("s", "s"));
    assert_eq!(refused["id"], 9);
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"]["code"], "unsupported_feature");
    assert_eq!(
        refused["error"]["message"],
        "adapter: unsupported input: session.task.output (unadvertised)"
    );
    assert!(refused["error"].get("details").is_none());
}

#[test]
fn a_payload_or_envelope_naming_another_session_is_a_scope_mismatch() {
    let hub = opened();
    for request in [request("s", "other"), request("other", "s")] {
        let refused = answer(&hub, &request);
        assert_eq!(refused["error"]["code"], "scope_mismatch", "{request}");
        assert_eq!(
            refused["error"]["message"],
            "request session does not match the addressed session"
        );
    }
}
