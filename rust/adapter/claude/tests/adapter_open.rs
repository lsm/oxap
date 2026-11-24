use oxap_adapter::Adapter;
use oxap_adapter_claude::{
    Adapter as ClaudeAdapter, AuthMode, Config, Header, Provider, ToolPosture,
};

#[test]
fn rejects_nul_in_the_launch_environment_before_startup() {
    let error = ClaudeAdapter::new(Config {
        executable: Some("unused".into()),
        tools: ToolPosture::Unrestricted,
        environment: vec!["LAUNCH_VALUE=invalid\0value".into()],
        ..Default::default()
    })
    .err()
    .expect("invalid environment must be refused before opening a session");
    assert!(error.to_string().contains("environment entry carries NUL"));
    assert!(!error.to_string().contains("invalid"));
}

#[test]
fn launch_plan_matches_go_for_all_auth_modes() {
    let mut config = Config {
        executable: Some("unused".into()),
        tools: ToolPosture::Unrestricted,
        environment: [
            "ANTHROPIC_BASE_URL=https://old.example",
            "ANTHROPIC_API_KEY=old",
            "ANTHROPIC_AUTH_TOKEN=old",
            "CLAUDE_CODE_OAUTH_TOKEN=old",
            "OTHER=entry=with=equals",
        ]
        .map(str::to_owned)
        .to_vec(),
        ..Default::default()
    };
    let plan = |config| ClaudeAdapter::new(config).unwrap().spawn_plan().unwrap();
    assert_eq!(plan(config.clone()).env, config.environment);
    let base_url = "HTTPS://gateway.example/anthropic";
    for (auth, secret, credential) in [
        (AuthMode::None, "", "ANTHROPIC_API_KEY=oap-unauthenticated"),
        (AuthMode::ApiKey, "key", "ANTHROPIC_API_KEY=key"),
        (AuthMode::Token, "token", "ANTHROPIC_AUTH_TOKEN=token"),
    ] {
        config.provider = Some(Provider {
            base_url: base_url.into(),
            auth,
            secret: secret.into(),
        });
        let launch = plan(config.clone());
        assert!(!format!("{launch:?}").contains("ANTHROPIC_"));
        assert_eq!(
            launch.env,
            [
                "OTHER=entry=with=equals",
                "ANTHROPIC_BASE_URL=HTTPS://gateway.example/anthropic",
                credential
            ]
        );
    }
    config.headers.push(Header {
        name: "X-Trace".into(),
        value: String::new(),
    });
    assert_eq!(
        plan(config.clone()).env.last().unwrap(),
        "ANTHROPIC_CUSTOM_HEADERS=X-Trace: "
    );
    config.environment.push("ANTHROPIC_CUSTOM_HEADERS=".into());
    assert!(plan(config)
        .env
        .iter()
        .any(|entry| entry == "ANTHROPIC_CUSTOM_HEADERS=X-Trace: "));
    println!("PASS launch plans: preserved native environment and all three configured auth modes");
}

#[test]
fn invalid_provider_and_headers_are_refused_at_construction() {
    let config = Config {
        executable: Some("unused".into()),
        tools: ToolPosture::Unrestricted,
        ..Default::default()
    };
    for (url, auth, secret, diagnostic) in [
        ("", AuthMode::None, "", "base URL"),
        ("ftp://gateway.example", AuthMode::None, "", "base URL"),
        ("https:///path", AuthMode::None, "", "base URL"),
        ("https://gate way.example", AuthMode::None, "", "base URL"),
        (
            "https://gateway.example\nX: value",
            AuthMode::None,
            "",
            "base URL",
        ),
        (
            "https://gateway.example",
            AuthMode::None,
            "private-value",
            "takes no secret",
        ),
        (
            "https://gateway.example",
            AuthMode::ApiKey,
            "",
            "needs a secret",
        ),
        (
            "https://gateway.example",
            AuthMode::Token,
            "",
            "needs a secret",
        ),
        (
            "https://gateway.example",
            AuthMode::Token,
            "private-value\r\n",
            "line break or NUL",
        ),
        (
            "https://gateway.example",
            AuthMode::ApiKey,
            "private-value\0",
            "line break or NUL",
        ),
    ] {
        let error = ClaudeAdapter::new(Config {
            provider: Some(Provider {
                base_url: url.into(),
                auth,
                secret: secret.into(),
            }),
            ..config.clone()
        })
        .err()
        .expect("invalid provider was accepted")
        .to_string();
        assert!(error.contains(diagnostic), "{error}");
        assert!(!error.contains("private-value"));
    }
    for (name, value) in [
        ("", ""),
        ("x:y", ""),
        ("x y", ""),
        ("x", "value\rnext"),
        ("x", "value\nnext"),
        ("x", "value\0next"),
    ] {
        let error = ClaudeAdapter::new(Config {
            headers: vec![Header {
                name: name.into(),
                value: value.into(),
            }],
            ..config.clone()
        })
        .err()
        .expect("invalid header was accepted")
        .to_string();
        assert!(error.contains("cannot be carried in ANTHROPIC_CUSTOM_HEADERS"));
        assert!(!error.contains("value"));
    }
    println!("PASS validation: 10 invalid routes and 6 invalid headers refused");
}
use oxap_protocol::{CompactionKind, CompactionPolicy, ReasoningLevel};

fn descriptor(adapter: &ClaudeAdapter) -> oxap_adapter::Descriptor {
    futures::executor::block_on(adapter.probe()).unwrap()
}

fn adapter() -> ClaudeAdapter {
    ClaudeAdapter::new(Config {
        tools: ToolPosture::Unrestricted,
        ..Config::default()
    })
    .unwrap()
}

#[test]
fn a_probe_describes_the_pinned_endpoint_the_features_and_the_process_memory_journal() {
    let adapter = adapter();
    let described = descriptor(&adapter);
    let pin = oxap_harness::current("claude-code").unwrap();
    assert_eq!(
        described.capabilities.endpoint.id.as_str(),
        "claude-code.cli"
    );
    assert_eq!(described.capabilities.endpoint.name, "Claude Code Adapter");
    assert_eq!(
        described.capabilities.endpoint.version,
        pin.endpoint_version
    );
    assert_eq!(
        described.capabilities.endpoint.adapter,
        "claude-code-stream-json"
    );
    assert_eq!(described.capability_revision, pin.capability_revision);
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
fn the_descriptor_uses_go_feature_keys_and_reports_rust_support() {
    let adapter = adapter();
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

#[test]
fn an_open_refuses_what_the_adapter_does_not_advertise() {
    let adapter = adapter();
    let request = oxap_adapter::OpenRequest {
        tools: vec![oxap_protocol::ToolDefinition {
            name: "x".into(),
            ..Default::default()
        }],
        ..oxap_adapter::OpenRequest::default()
    };
    let refused = futures::executor::block_on(adapter.open(request))
        .err()
        .unwrap();
    assert!(
        refused.to_string().contains("action.tools.provide"),
        "{refused}"
    );
}

#[test]
fn an_open_refuses_a_level_and_a_policy_claude_code_cannot_take() {
    let adapter = adapter();
    let request = oxap_adapter::OpenRequest {
        reasoning_level: ReasoningLevel::OFF,
        ..oxap_adapter::OpenRequest::default()
    };
    let refused = futures::executor::block_on(adapter.open(request))
        .err()
        .unwrap();
    assert!(
        refused.to_string().contains("session.reasoning"),
        "{refused}"
    );
    assert!(
        refused
            .to_string()
            .contains("Claude Code's effort levels are low through max"),
        "{refused}"
    );
    let request = oxap_adapter::OpenRequest {
        compaction_policy: Some(CompactionPolicy {
            kind: CompactionKind::SHARE,
            share_percent: 50,
            ..CompactionPolicy::default()
        }),
        ..oxap_adapter::OpenRequest::default()
    };
    let refused = futures::executor::block_on(adapter.open(request))
        .err()
        .unwrap();
    assert!(
        refused.to_string().contains("session.compaction.policy"),
        "{refused}"
    );
    assert!(
        refused
            .to_string()
            .contains("Claude Code takes a token window, not a share"),
        "{refused}"
    );
}

#[test]
fn an_open_that_reopens_without_a_bound_conversation_is_refused() {
    let adapter = adapter();
    let request = oxap_adapter::OpenRequest {
        reopen: true,
        ..oxap_adapter::OpenRequest::default()
    };
    let refused = futures::executor::block_on(adapter.open(request))
        .err()
        .unwrap();
    assert!(
        refused.to_string().contains("session.open.reopen"),
        "{refused}"
    );
    assert!(
        refused
            .to_string()
            .contains("must name a Claude Code session UUID"),
        "{refused}"
    );
}

#[test]
fn a_tool_posture_is_required_and_names_at_least_one_tool() {
    assert!(ClaudeAdapter::new(Config::default()).is_err());
    assert!(ClaudeAdapter::new(Config {
        tools: ToolPosture::Unstated,
        ..Config::default()
    })
    .is_err());
    assert!(ClaudeAdapter::new(Config {
        tools: ToolPosture::allow(&[]),
        ..Config::default()
    })
    .is_err());
    assert!(ClaudeAdapter::new(Config {
        tools: ToolPosture::allow(&["".to_owned()]),
        ..Config::default()
    })
    .is_err());
    assert!(ClaudeAdapter::new(Config {
        tools: ToolPosture::allow(&["Bash".to_owned()]),
        ..Config::default()
    })
    .is_ok());
    assert!(ClaudeAdapter::new(Config {
        tools: ToolPosture::Unrestricted,
        ..Config::default()
    })
    .is_ok());
}

#[test]
fn the_arguments_and_environment_are_the_ones_go_spawns_with() {
    let plan = ClaudeAdapter::new(Config {
        executable: Some("/usr/bin/claude".into()),
        model: Some("claude-test".into()),
        tools: ToolPosture::allow(&["Bash(ls)".to_owned(), "Read".to_owned()]),
        environment: vec!["HOME=/tmp".into()],
        working_directory: Some("/tmp".into()),
        ..Config::default()
    })
    .unwrap()
    .spawn_plan()
    .unwrap();
    assert_eq!(plan.path, "/usr/bin/claude");
    assert_eq!(
        plan.args,
        [
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--system-prompt",
            "",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--setting-sources=",
            "--model",
            "claude-test",
            "--tools",
            "Bash,Read",
            "--allowedTools",
            "Bash(ls)",
            "Read",
        ]
    );
    assert_eq!(plan.dir, "/tmp");
    assert!(plan.env.contains(&"HOME=/tmp".to_owned()));
}

#[test]
fn an_unrestricted_posture_names_no_tool_arguments() {
    let plan = ClaudeAdapter::new(Config {
        executable: Some("/usr/bin/claude".into()),
        tools: ToolPosture::Unrestricted,
        ..Config::default()
    })
    .unwrap()
    .spawn_plan()
    .unwrap();
    assert!(!plan.args.iter().any(|arg| arg == "--tools"));
    assert!(!plan.args.iter().any(|arg| arg == "--allowedTools"));
}

#[test]
fn two_probes_of_one_adapter_describe_the_same_endpoint() {
    let adapter = adapter();
    let first = descriptor(&adapter);
    let second = descriptor(&adapter);
    assert_eq!(first.capability_revision, second.capability_revision);
    assert_eq!(
        first.capabilities.features.len(),
        second.capabilities.features.len()
    );
}
