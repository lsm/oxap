mod support;

use std::sync::Arc;

use futures::executor::block_on;
use oxap_adapter::memory::{Config as MemoryConfig, Memory};
use oxap_adapter::{
    Adapter, BoxFuture, Descriptor, EventStream, InteractionResolution, OpenRequest, Recovery,
    ResumeRequest, Session, SubmitRequest, TaskCanceller,
};
use oxap_protocol::{
    MessageSubmitResponse, RunCancelResponse, RunId, SessionState, SessionTaskCancelResponse,
};
use oxap_serve::stdio::{serve, StdioOptions};
use oxap_serve::{Hub, Registry};
use serde_json::Value;

fn request(task: &str) -> String {
    format!(
        "{{\"protocol\":\"open-agent-protocol\",\"version\":\"0.1\",\"profile\":\"open-agent-protocol.agent-control-core\",\"type\":\"session.task.cancel.request\",\"id\":\"req-1\",\"session_id\":\"s\",\"capability_revision\":\"rev-cited\",\"payload\":{{\"session_id\":\"s\",\"task_id\":\"{task}\"}}}}"
    )
}

fn answer(hub: &Hub, request: &str) -> Value {
    let line =
        format!("{{\"id\":9,\"op\":\"cancel\",\"session_id\":\"s\",\"request\":{request}}}\n");
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

fn opened(registry: Registry) -> Hub {
    let hub = Hub::new(registry);
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

#[test]
fn a_session_whose_adapter_cancels_no_tasks_refuses_a_task_cancel_as_an_unsupported_feature() {
    let hub = opened(Registry::default_registry());
    let refused = answer(&hub, &request("task-1"));
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"]["code"], "unsupported_feature");
    assert_eq!(
        refused["error"]["message"],
        "adapter: unsupported input: session.task.cancel (unadvertised)"
    );
}

struct Cancelling(Box<dyn Session>);

impl Session for Cancelling {
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
    fn task_canceller(&self) -> Option<&dyn TaskCanceller> {
        Some(self)
    }
}

impl TaskCanceller for Cancelling {
    fn cancel_task(&self, task_id: String) -> BoxFuture<'_, SessionTaskCancelResponse> {
        Box::pin(async move {
            Ok(SessionTaskCancelResponse {
                outcome: if task_id == "done" {
                    "already_ended".into()
                } else {
                    "requested".into()
                },
            })
        })
    }
}

struct Cancellers(Arc<dyn Adapter>);

impl Adapter for Cancellers {
    fn probe(&self) -> BoxFuture<'_, Descriptor> {
        self.0.probe()
    }
    fn open(&self, request: OpenRequest) -> BoxFuture<'_, Box<dyn Session>> {
        Box::pin(async move {
            let inner = self.0.open(request).await?;
            Ok(Box::new(Cancelling(inner)) as Box<dyn Session>)
        })
    }
}

#[test]
fn a_canceller_answers_its_outcome_in_reply_to_the_task_cancel_request() {
    let mut registry = Registry::new();
    registry
        .register(
            "memory",
            Arc::new(Cancellers(Arc::new(Memory::new(MemoryConfig::default())))),
        )
        .unwrap();
    let hub = opened(registry);
    for (task, outcome) in [("task-1", "requested"), ("done", "already_ended")] {
        let answered = answer(&hub, &request(task));
        assert_eq!(answered["ok"], true, "{answered}");
        let result = &answered["result"];
        assert_eq!(result["type"], "session.task.cancel.response");
        assert_eq!(result["in_reply_to"], "req-1");
        assert_eq!(result["session_id"], "s");
        assert_eq!(result["capability_revision"], "rev-cited");
        assert_eq!(result["payload"], serde_json::json!({ "outcome": outcome }));
    }
}
