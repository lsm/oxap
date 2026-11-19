use oxap_adapter::{Error, OpenRequest, CONTROL_UNADVERTISED};
use oxap_adapter_copilot::adapter::{Adapter, Config};
use oxap_adapter_copilot::{CAPABILITY_REVISION, PINNED_VERSION};
use oxap_protocol::{CompactionPolicy, Participant, ReasoningLevel, SupportLevel, ToolDefinition};
use serde_json::Value;

const GO_CAPABILITIES: &str = r#"{"endpoint":{"id":"copilot.headless","name":"GitHub Copilot CLI Adapter","version":"@VERSION@","adapter":"copilot-headless-stdio"},"protocol_versions":["0.1"],"profiles":["open-agent-protocol.agent-control-core"],"features":{"action.tools":{"level":"native","reason":"tool.execution_start and tool.execution_complete with progress between"},"action.tools.execute":{"level":"unavailable","reason":"Copilot executes its own tools"},"capabilities":{"level":"emulated","reason":"a conservative descriptor for the pinned headless server"},"protocol.initialize":{"level":"emulated","reason":"the adapter checks the server's protocol version with ping and creates the native session"},"run.status":{"level":"emulated"},"run.streaming":{"level":"native"},"session.message.delivery.auto":{"level":"emulated","reason":"start when no run is open, otherwise queue behind it, as session.send enqueues"},"session.message.delivery.queue":{"level":"emulated","reason":"session.send enqueues behind an open run; an explicit queue on an idle session is admitted as a reservation that Copilot promotes at once"},"session.message.submit":{"level":"emulated","reason":"session.send answers a message id at once; the run starts at its own user.message"},"session.open":{"level":"native","reason":"session.create in one headless server per OAP session"},"session.state":{"level":"emulated","reason":"adapter-owned projection of the runs it admitted"}},"limits":{"max_active_runs_per_session":9,"max_queued_runs_per_session":8}}"#;

fn config() -> Config {
    Config {
        executable: "/nonexistent/copilot".into(),
        working_directory: std::env::temp_dir().display().to_string(),
        ..Config::default()
    }
}

fn request() -> OpenRequest {
    OpenRequest {
        session_id: "s1".into(),
        participant: Participant {
            id: "user".into(),
            ..Participant::default()
        },
        ..OpenRequest::default()
    }
}

#[test]
fn the_descriptor_matches_gos_probe_at_the_pinned_revision() {
    let descriptor = Adapter::new(config()).unwrap().describe();
    let expected: Value =
        serde_json::from_str(&GO_CAPABILITIES.replace("@VERSION@", &PINNED_VERSION)).unwrap();
    assert_eq!(
        serde_json::to_value(&descriptor.capabilities).unwrap(),
        expected
    );
    assert_eq!(descriptor.capability_revision, *CAPABILITY_REVISION);
    assert_eq!(descriptor.journal.scope, "session");
    assert_eq!(descriptor.journal.persistence, "process_memory");
    assert_eq!(descriptor.journal.replay, SupportLevel::EMULATED);
    assert_eq!(descriptor.journal.capacity, 256);
    assert_eq!(descriptor.max_active_runs_per_session, 9);
    assert!(!descriptor.interactive_gates);
    assert_eq!(descriptor.cancellation_target, "none");
    assert_eq!(descriptor.cancellation_implementation, "unavailable");
}

#[test]
fn the_headless_options_are_appended_after_the_configured_args() {
    let adapter = Adapter::new(Config {
        args: vec!["--log-level".into(), "none".into()],
        ..config()
    })
    .unwrap();
    assert_eq!(
        adapter.config().args,
        [
            "--log-level",
            "none",
            "--headless",
            "--stdio",
            "--no-auto-update"
        ]
    );
}

#[test]
fn configuration_requires_an_executable_an_absolute_directory_and_no_standalone_dashes() {
    let absolute = std::env::temp_dir().display().to_string();
    for invalid in [
        Config::default(),
        Config {
            executable: "copilot".into(),
            ..Config::default()
        },
        Config {
            executable: "copilot".into(),
            working_directory: "relative".into(),
            ..Config::default()
        },
        Config {
            executable: "copilot".into(),
            working_directory: absolute,
            args: vec!["--".into()],
            ..Config::default()
        },
    ] {
        assert!(
            matches!(Adapter::new(invalid.clone()), Err(Error::Harness(_))),
            "{invalid:?}"
        );
    }
}

#[test]
fn open_refuses_unavailable_features_before_starting_a_process() {
    let adapter = Adapter::new(config()).unwrap();
    let mut requests = vec![
        request(),
        request(),
        request(),
        request(),
        request(),
        request(),
    ];
    requests[0].reopen = true;
    requests[1].native_session_id = "native".into();
    requests[2].adopted = true;
    requests[3].reasoning_level = ReasoningLevel::HIGH;
    requests[4].compaction_policy = Some(CompactionPolicy::default());
    requests[5].tools = vec![ToolDefinition {
        name: "synthetic".into(),
        ..ToolDefinition::default()
    }];
    for request in requests {
        match adapter.admit(&request) {
            Err(Error::UnsupportedControl(refusal)) => {
                assert_eq!(refusal.reason, CONTROL_UNADVERTISED, "{refusal:?}")
            }
            other => panic!("feature reached process startup: {other:?}"),
        }
    }
}

#[test]
fn open_requires_a_participant() {
    let adapter = Adapter::new(config()).unwrap();
    let mut anonymous = request();
    anonymous.participant.id = "".into();
    assert_eq!(adapter.admit(&anonymous), Err(Error::InvalidParticipant));
    assert_eq!(adapter.admit(&request()), Ok(()));
}
