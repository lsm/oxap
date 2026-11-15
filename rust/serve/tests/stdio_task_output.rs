mod support;

use std::sync::Arc;

use futures::executor::block_on;
use oxap_adapter::memory::{Config as MemoryConfig, Memory};
use oxap_adapter::{
    Adapter, BoxFuture, Descriptor, EventStream, InteractionResolution, OpenRequest, Recovery,
    ResumeRequest, Session, SubmitRequest, TaskOutputReader,
};
use oxap_protocol::{
    MessageSubmitResponse, RunCancelResponse, RunId, SessionState, SessionTaskOutputResponse,
};
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

struct Reading(Box<dyn Session>);

impl Session for Reading {
    fn submit(
        &self,
        request: SubmitRequest,
    ) -> BoxFuture<'_, (MessageSubmitResponse, EventStream)> {
        self.0.submit(request)
    }
    fn state(&self) -> BoxFuture<'_, SessionState> {
        self.0.state()
    }
    fn resolve(&self, resolution: InteractionResolution) -> BoxFuture<'_, ()> {
        self.0.resolve(resolution)
    }
    fn cancel(&self, run_id: RunId) -> BoxFuture<'_, RunCancelResponse> {
        self.0.cancel(run_id)
    }
    fn resume(&self, request: ResumeRequest) -> BoxFuture<'_, (Recovery, EventStream)> {
        self.0.resume(request)
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        self.0.close()
    }
    fn task_output_reader(&self) -> Option<&dyn TaskOutputReader> {
        Some(self)
    }
}

impl TaskOutputReader for Reading {
    fn task_output(
        &self,
        task_id: String,
        from: Option<u64>,
    ) -> BoxFuture<'_, SessionTaskOutputResponse> {
        Box::pin(async move {
            Ok(SessionTaskOutputResponse {
                output: format!("{task_id} from {}", from.unwrap_or_default()),
                next: 4,
                truncated: false,
                ended: true,
            })
        })
    }
}

struct Readers(Arc<dyn Adapter>);

impl Adapter for Readers {
    fn probe(&self) -> BoxFuture<'_, Descriptor> {
        self.0.probe()
    }
    fn open(&self, request: OpenRequest) -> BoxFuture<'_, Box<dyn Session>> {
        Box::pin(async move {
            let inner = self.0.open(request).await?;
            Ok(Box::new(Reading(inner)) as Box<dyn Session>)
        })
    }
}

#[test]
fn a_session_that_reads_task_output_answers_with_its_output_in_reply_to_the_request() {
    let mut registry = Registry::new();
    registry
        .register(
            "memory",
            Arc::new(Readers(Arc::new(Memory::new(MemoryConfig::default())))),
        )
        .unwrap();
    let hub = Hub::new(registry);
    block_on(hub.open(
        "memory",
        OpenRequest {
            session_id: "s".into(),
            ..OpenRequest::default()
        },
    ))
    .unwrap();
    let answered = answer(&hub, &request("s", "s"));
    assert_eq!(answered["ok"], true, "{answered}");
    let result = &answered["result"];
    assert_eq!(result["type"], "session.task.output.response");
    assert_eq!(result["in_reply_to"], "req-1");
    assert_eq!(result["session_id"], "s");
    assert_eq!(result["capability_revision"], "rev-cited");
    assert_eq!(
        result["payload"],
        serde_json::json!({"output": "task-1 from 3", "next": 4, "truncated": false, "ended": true})
    );
}
