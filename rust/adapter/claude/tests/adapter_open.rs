use oxap_adapter::Adapter;
use oxap_adapter_claude::{Adapter as ClaudeAdapter, Config};

fn descriptor(adapter: &ClaudeAdapter) -> oxap_adapter::Descriptor {
    futures::executor::block_on(adapter.probe()).unwrap()
}

#[test]
fn a_probe_describes_the_pinned_endpoint_the_features_and_the_process_memory_journal() {
    let adapter = ClaudeAdapter::new(Config::default()).unwrap();
    let described = descriptor(&adapter);
    assert_eq!(
        described.capabilities.endpoint.id.as_str(),
        "claude-code.cli"
    );
    assert_eq!(described.capabilities.endpoint.name, "Claude Code Adapter");
    assert_eq!(described.capabilities.endpoint.version, "v2.1.322");
    assert_eq!(
        described.capabilities.endpoint.adapter,
        "claude-code-stream-json"
    );
    assert_eq!(described.capability_revision, "claude-code-2.1.322-oap-v1");
    assert_eq!(described.capabilities.protocol_versions, ["0.1"]);
    assert_eq!(
        described.capabilities.profiles,
        ["open-agent-protocol.agent-control-core"]
    );
    assert_eq!(described.max_active_runs_per_session, 1);
    assert!(described.interactive_gates);
    assert_eq!(described.cancellation_target, "session");
    assert_eq!(
        described.cancellation_implementation,
        "interrupt control request"
    );
    assert_eq!(described.journal.scope, "session");
    assert_eq!(described.journal.persistence, "process_memory");
    assert_eq!(described.journal.capacity, 256);
    assert_eq!(
        described.capabilities.sources,
        vec![oxap_protocol::ToolSourceDescriptor {
            id: "claude-code-native".into(),
            kind: "native".into(),
            display_name: "Claude Code built-in tools".into(),
            ..Default::default()
        }]
    );
    assert!(described.capabilities.tools.is_empty());
}

#[test]
fn the_described_features_are_exactly_the_keys_go_advertises() {
    let adapter = ClaudeAdapter::new(Config::default()).unwrap();
    let described = descriptor(&adapter);
    let mut keys = described
        .capabilities
        .features
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "action.permissions",
            "action.tools",
            "action.tools.execute",
            "action.tools.list",
            "capabilities",
            "protocol.initialize",
            "run.cancel",
            "run.compaction",
            "run.reconciliation",
            "run.replay",
            "run.resume",
            "run.status",
            "run.streaming",
            "run.tool_selection",
            "session.compaction.policy",
            "session.message.delivery.auto",
            "session.message.delivery.queue",
            "session.message.delivery.steer",
            "session.message.submit",
            "session.open",
            "session.open.reopen",
            "session.reasoning",
            "session.state",
            "user_input",
        ]
    );
    let reasoning = &described.capabilities.features["session.reasoning"];
    assert_eq!(reasoning.level.as_str(), "native");
    assert_eq!(reasoning.modes, ["session_open", "session_live"]);
    let policy = &described.capabilities.features["session.compaction.policy"];
    assert_eq!(policy.level.as_str(), "emulated");
    assert_eq!(policy.modes, ["session_open", "session_live"]);
    let selection = &described.capabilities.features["run.tool_selection"];
    assert_eq!(selection.level.as_str(), "emulated");
    assert_eq!(selection.scope, "run");
}
