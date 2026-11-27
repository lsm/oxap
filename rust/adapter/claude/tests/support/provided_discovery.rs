use std::{fs, os::unix::fs::PermissionsExt};

use futures::executor::block_on;
use oxap_adapter::CONTROL_UNADVERTISED;
use oxap_protocol::{ToolDefinition, ToolsListRequest, FEATURE_TOOLS_PROVIDE};
use serde_json::{json, value::to_raw_value, Value};

use super::*;

fn tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        execution_owner: "user".into(),
        source: "claude-code-native".into(),
        ..Default::default()
    }
}

fn request(tools: Vec<ToolDefinition>) -> OpenRequest {
    OpenRequest {
        session_id: "session".into(),
        tools,
        ..Default::default()
    }
}

#[test]
fn provided_discovery_exchanges_discovery_before_open_and_lists_native_echoes() {
    for mode in ["unrestricted", "allow", "empty"] {
        let directory = tempfile::tempdir().unwrap();
        let dir = directory.path().canonicalize().unwrap();
        let peer = dir.join("peer");
        fs::write(&peer, include_str!("provided_discovery_peer.py")).unwrap();
        fs::set_permissions(&peer, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Adapter::new(Config {
            executable: Some(peer.display().to_string()),
            working_directory: Some(dir.display().to_string()),
            environment: vec![
                format!("HOME={}", dir.display()),
                format!("PATH={}", std::env::var("PATH").unwrap()),
                format!("MODE={mode}"),
                "NODE_ENV=test".into(),
            ],
            tools: if mode == "allow" {
                ToolPosture::allow(&["Read".into()])
            } else {
                ToolPosture::Unrestricted
            },
            ..Default::default()
        })
        .unwrap();
        let schema = json!({"type":"object","properties":{"text":{"type":"string"}}});
        let echo = ToolDefinition {
            description: "echo it".into(),
            input_schema: Some(to_raw_value(&schema).unwrap()),
            ..tool("echo")
        };
        let mut look = tool("look");
        look.input_schema = Some(to_raw_value(&json!({"type":"object"})).unwrap());
        let mut supplied_look = tool("look");
        if mode == "allow" {
            supplied_look.input_schema = Some(to_raw_value(&Value::Null).unwrap());
        }
        let tools = if mode == "empty" {
            Vec::new()
        } else {
            vec![echo.clone(), supplied_look]
        };
        let session = block_on(adapter.open_staged(request(tools)))
            .unwrap_or_else(|err| panic!("{mode} open failed: {err}"));
        assert!(dir.join("discovered").is_file(), "{mode} discovery");
        let list = block_on(session.tool_lister().unwrap().tools(ToolsListRequest {
            session_id: Some("session".into()),
            allow_degraded_features: vec![FEATURE_TOOLS_LIST.into()],
        }))
        .unwrap();
        assert_eq!(list.tools.sources.len(), 1);
        assert_eq!(list.tools.sources[0].id, "claude-code-native");
        if mode == "empty" {
            assert!(list.tools.tools.is_empty());
        } else {
            assert_eq!(
                serde_json::to_value(list.tools.tools).unwrap(),
                json!([echo, look])
            );
        }
        block_on(session.close()).unwrap();
        assert!(dir.join("eof").is_file(), "{mode} did not close");
        println!("PASS {mode}: native launch, discovery, scoped catalog, EOF");
    }
}

#[test]
fn provided_discovery_refuses_unsatisfiable_definitions_before_spawn() {
    let adapter = Adapter::new(Config {
        tools: ToolPosture::Unrestricted,
        ..Default::default()
    })
    .unwrap();
    let mut owned = tool("echo");
    owned.execution_owner = "someone".into();
    let mut foreign = tool("echo");
    foreign.source = "elsewhere".into();
    let mut dialect = tool("echo");
    dialect.input_schema =
        Some(to_raw_value(&json!({"$schema":"http://json-schema.org/draft-07/schema#"})).unwrap());
    let mut boolean = tool("echo");
    boolean.input_schema = Some(to_raw_value(&false).unwrap());
    let mut numeric = tool("echo");
    numeric.input_schema = Some(to_raw_value(&json!({"$schema":42})).unwrap());
    let cases = [
        (vec![tool("")], "".to_owned(), ""),
        (vec![owned], "echo".to_owned(), ""),
        (vec![foreign], "echo".to_owned(), "elsewhere"),
        (vec![tool("echo"), tool("echo")], "echo".to_owned(), ""),
        (
            vec![tool("mcp__oap__echo")],
            "mcp__oap__echo".to_owned(),
            "",
        ),
        (vec![tool("Bash")], "Bash".to_owned(), ""),
        (vec![tool("hello world")], "hello world".to_owned(), ""),
        (vec![tool(&"x".repeat(55))], "x".repeat(55), ""),
        (vec![dialect], "echo".to_owned(), ""),
        (vec![boolean], "echo".to_owned(), ""),
        (vec![numeric], "echo".to_owned(), ""),
        (
            (0..65).map(|index| tool(&format!("t{index}"))).collect(),
            "t64".into(),
            "",
        ),
    ];
    for (tools, name, source) in cases {
        let result = block_on(adapter.open_staged(request(tools)));
        assert!(
            matches!(
                result.err(),
                Some(Error::UnsupportedControl(refusal))
                    if refusal.feature == FEATURE_TOOLS_PROVIDE
                        && refusal.reason == CONTROL_UNSATISFIABLE
                        && refusal.tool == name
                        && refusal.source == source
            ),
            "wrong refusal for {name}"
        );
    }
    println!("PASS 12 unsatisfiable declarations refused before spawn");
}

#[test]
fn provided_discovery_public_open_still_requires_the_capability() {
    let adapter = Adapter::new(Config {
        tools: ToolPosture::Unrestricted,
        ..Default::default()
    })
    .unwrap();
    assert!(!adapter
        .describe()
        .capabilities
        .features
        .contains_key(FEATURE_TOOLS_PROVIDE));
    assert!(matches!(
        block_on(adapter.open(request(vec![tool("echo")]))).err(),
        Some(Error::UnsupportedControl(refusal))
            if refusal.feature == FEATURE_TOOLS_PROVIDE && refusal.reason == CONTROL_UNADVERTISED
    ));
    println!("PASS public session open rejects provision until call resolution is supported");
}

#[test]
fn provided_lifecycle_projects_and_cancels_only_permitted_calls() {
    use futures::StreamExt;
    use oxap_adapter::{testkit::RunTrace, SubmitRequest};
    use oxap_protocol::{
        ActionCallPayload, Message, MessageContent, MessageRole, MessageSubmitRequest,
        RequestedDeliveryMode, RunStatus, RunStatusUpdatedPayload,
    };

    for mode in ["native-result", "sweep", "denied"] {
        let directory = tempfile::tempdir().unwrap();
        let dir = directory.path().canonicalize().unwrap();
        let peer = dir.join("peer");
        fs::write(&peer, include_str!("provided_lifecycle_peer.py")).unwrap();
        fs::set_permissions(&peer, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Adapter::new(Config {
            executable: Some(peer.display().to_string()),
            working_directory: Some(dir.display().to_string()),
            environment: vec![
                format!("HOME={}", dir.display()),
                format!("PATH={}", std::env::var("PATH").unwrap()),
                format!("MODE={mode}"),
                "NODE_ENV=test".into(),
            ],
            tools: ToolPosture::Unrestricted,
            ..Default::default()
        })
        .unwrap();
        let mut provision = tool("echo");
        provision.execution_owner = "operator".into();
        let mut open = request(vec![provision]);
        open.participant.id = "operator".into();
        let session = block_on(adapter.open_staged(open.clone())).expect(mode);
        let submit = MessageSubmitRequest {
            session_id: open.session_id.clone(),
            delivery: RequestedDeliveryMode::AUTO,
            messages: vec![Message {
                role: MessageRole::USER,
                content: MessageContent::from_text("go"),
                ..Default::default()
            }],
            tool_choice: (mode == "denied")
                .then(|| to_raw_value(&json!({"disallowed":["echo"]})).unwrap()),
            ..Default::default()
        };
        let (admission, mut stream) = block_on(session.submit(SubmitRequest {
            request: submit.clone(),
            ..Default::default()
        }))
        .expect(mode);
        let mut events = Vec::new();
        if mode != "denied" {
            events.extend(
                block_on(stream.by_ref().take(3).collect::<Vec<_>>())
                    .into_iter()
                    .map(Result::unwrap),
            );
            let kinds: Vec<_> = events
                .iter()
                .map(|event| event.envelope_type.as_str())
                .collect();
            assert_eq!(
                kinds,
                ["run.started", "action.call.requested", "run.status.updated"],
                "{mode}"
            );
            let call: ActionCallPayload = events[1].decode_payload().unwrap();
            let status: RunStatusUpdatedPayload = events[2].decode_payload().unwrap();
            assert_eq!(call.name, "echo");
            assert_eq!(call.execution_owner.as_str(), "operator");
            assert_eq!(call.responded_by.unwrap().as_str(), "operator");
            assert_eq!(call.requested_by.unwrap().as_str(), "claude-code.cli");
            assert_eq!(call.source, "claude-code-native");
            assert_eq!(call.arguments_json.unwrap().get(), r#"{"text": "hi"}"#);
            assert!(call.interaction_id.is_some());
            assert_eq!(status.pending_user_input_id, call.interaction_id);
            assert_eq!(status.status, RunStatus::WAITING_FOR_INPUT);
            let live = block_on(session.state()).unwrap();
            assert_eq!(live.active_runs[0].status, RunStatus::WAITING_FOR_INPUT);
            fs::write(dir.join("continue"), []).unwrap();
        }
        events.extend(
            block_on(stream.collect::<Vec<_>>())
                .into_iter()
                .map(Result::unwrap),
        );
        let expected = match mode {
            "native-result" => &[
                "run.started",
                "action.call.requested",
                "run.status.updated",
                "action.call.cancelled",
                "run.status.updated",
                "run.completed",
            ][..],
            "sweep" => &[
                "run.started",
                "action.call.requested",
                "run.status.updated",
                "action.call.cancelled",
                "run.completed",
            ],
            _ => &["run.started", "run.completed"],
        };
        let kinds: Vec<_> = events
            .iter()
            .map(|event| event.envelope_type.as_str())
            .collect();
        assert_eq!(kinds, expected, "{mode}");
        if mode == "denied" {
            assert!(dir.join("hook").is_file() && dir.join("gate").is_file());
        } else {
            let cancelled: ActionCallPayload = events[3].decode_payload().unwrap();
            let request: ActionCallPayload = events[1].decode_payload().unwrap();
            assert_eq!(cancelled.tool_call_id, request.tool_call_id);
            assert_eq!(cancelled.interaction_id, request.interaction_id);
            assert!(cancelled.arguments_json.is_none());
            assert!(events[3].in_reply_to.is_none());
        }
        if mode == "native-result" {
            let running: RunStatusUpdatedPayload = events[4].decode_payload().unwrap();
            assert_eq!(running.status, RunStatus::RUNNING);
            assert!(running.pending_user_input_id.is_none());
        }
        let mut trace = RunTrace::new(admission, adapter.describe(), events);
        open.tools.clear();
        trace.opened = Some(open);
        trace.submitted = Some(submit);
        trace.assert_valid();
        block_on(session.close()).unwrap();
        assert!(dir.join("eof").is_file());
        println!("PASS {mode}: provided tool projection, settlement, and EOF");
    }
}

#[test]
fn provided_resolution_exchanges_results_in_both_orders_and_cancels_held_calls() {
    use std::time::{Duration, Instant};

    use futures::StreamExt;
    use oxap_adapter::{CallResolution, SubmitRequest};
    use oxap_protocol::{
        ActionCallPayload, ActionCallResolveRequest, Message, MessageContent, MessageRole,
        MessageSubmitRequest, ProtocolError, RequestedDeliveryMode, ResolveArmStarted,
        ResolveReason,
    };

    for mode in ["reply-first", "held", "cancel-held"] {
        let directory = tempfile::tempdir().unwrap();
        let dir = directory.path().canonicalize().unwrap();
        let peer = dir.join("peer");
        fs::write(&peer, include_str!("provided_lifecycle_peer.py")).unwrap();
        fs::set_permissions(&peer, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Adapter::new(Config {
            executable: Some(peer.display().to_string()),
            working_directory: Some(dir.display().to_string()),
            environment: vec![
                format!("HOME={}", dir.display()),
                format!("PATH={}", std::env::var("PATH").unwrap()),
                format!("MODE={mode}"),
                "NODE_ENV=test".into(),
            ],
            tools: ToolPosture::Unrestricted,
            ..Default::default()
        })
        .unwrap();
        let session = block_on(adapter.open_staged(request(vec![tool("echo")]))).expect(mode);
        let (_, mut stream) = block_on(session.submit(SubmitRequest {
            request: MessageSubmitRequest {
                session_id: "session".into(),
                delivery: RequestedDeliveryMode::AUTO,
                messages: vec![Message {
                    role: MessageRole::USER,
                    content: MessageContent::from_text("go"),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        }))
        .expect(mode);
        let mut next = || block_on(stream.next()).unwrap().unwrap();
        assert_eq!(next().envelope_type.as_str(), "run.started");
        let requested = next();
        assert_eq!(requested.envelope_type.as_str(), "action.call.requested");
        let call: ActionCallPayload = requested.decode_payload().unwrap();
        assert_eq!(next().envelope_type.as_str(), "run.status.updated");
        let wait_for = |name| {
            let deadline = Instant::now() + Duration::from_secs(4);
            while !dir.join(name).is_file() {
                assert!(
                    Instant::now() < deadline,
                    "{mode}: missing peer marker {name}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let resolve = |id: &str, request| {
            block_on(
                session
                    .call_resolver()
                    .expect("staged session offers a resolver")
                    .resolve_call(CallResolution {
                        request_id: id.into(),
                        request,
                    }),
            )
            .unwrap()
        };
        let mut request = ActionCallResolveRequest {
            session_id: call.session_id,
            run_id: call.run_id,
            tool_call_id: call.tool_call_id,
            interaction_id: call.interaction_id.unwrap(),
            requested_by: call.requested_by.unwrap(),
            responded_by: call.responded_by.unwrap(),
            ..Default::default()
        };
        let started;
        if mode != "reply-first" {
            wait_for("barrier");
        }
        if mode == "held" {
            request.started = Some(ResolveArmStarted {});
            let mut unrelated = request.clone();
            unrelated.interaction_id = "unknown".into();
            unrelated.responded_by = "intruder".into();
            assert_eq!(
                resolve("unknown", unrelated).reason,
                ResolveReason::UNKNOWN_INTERACTION
            );
            let mut foreign = request.clone();
            foreign.responded_by = "intruder".into();
            assert_eq!(
                resolve("foreign", foreign).reason,
                ResolveReason::WRONG_RESPONDER
            );
            assert!(resolve("ack", request.clone()).accepted);
            started = Some(next());
            assert_eq!(
                resolve("repeat", request.clone()).reason,
                ResolveReason::REPEATED_ACKNOWLEDGEMENT
            );
            request.started = None;
        } else {
            started = None;
        }
        if mode == "reply-first" {
            request.error = Some(ProtocolError {
                code: "tool_failed".into(),
                message: "could not echo".into(),
                details: serde_json::from_value(json!({"retry":false})).unwrap(),
                ..Default::default()
            });
        } else {
            request.result = Some(to_raw_value("hi back").unwrap());
        }
        if mode != "cancel-held" {
            assert!(resolve("settle", request.clone()).accepted);
            let explicit = started.is_some();
            let started = started.unwrap_or_else(&mut next);
            assert_eq!(started.envelope_type.as_str(), "action.call.started");
            assert_eq!(started.in_reply_to, Some(requested.id.clone()));
            let payload: ActionCallPayload = started.decode_payload().unwrap();
            assert_eq!(
                payload.request_id,
                Some(if explicit { "ack" } else { "settle" }.into())
            );
        }
        fs::write(dir.join("continue"), []).unwrap();
        let settled = next();
        let payload: ActionCallPayload = settled.decode_payload().unwrap();
        let kind = match mode {
            "held" => "action.call.completed",
            "reply-first" => "action.call.failed",
            _ => "action.call.cancelled",
        };
        assert_eq!(settled.envelope_type.as_str(), kind);
        if mode != "cancel-held" {
            assert_eq!(payload.request_id, Some("settle".into()));
            assert_eq!(
                payload.result.as_deref().map(|raw| raw.get()),
                request.result.as_deref().map(|raw| raw.get())
            );
            assert_eq!(
                serde_json::to_value(payload.error).unwrap(),
                serde_json::to_value(&request.error).unwrap()
            );
            let running = next();
            assert_eq!(running.envelope_type.as_str(), "run.status.updated");
            assert_eq!(
                serde_json::from_str::<Value>(running.payload.as_deref().unwrap().get()).unwrap()
                    ["status"],
                "running"
            );
        }
        assert_eq!(next().envelope_type.as_str(), "run.completed");
        assert!(block_on(stream.next()).is_none());
        let late = resolve("late", request);
        assert!(!late.accepted);
        assert_eq!(late.reason, ResolveReason::ALREADY_RESOLVED);
        assert_eq!(late.details.unwrap().settlement_id, Some(settled.id));
        wait_for("reply");
        block_on(session.close()).unwrap();
        assert!(dir.join("eof").is_file());
        println!("PASS {mode}: OAP resolution, exact MCP reply, late refusal, and EOF");
    }
}
