use oxap_adapter::{Adapter, Descriptor, Error, OpenRequest};
use oxap_adapter_pi::adapter::{pin, Adapter as PiAdapter, Config};
use oxap_protocol::{CompactionPolicy, Participant, ToolDefinition, ToolSourceAttachment};

fn config() -> Config {
    Config {
        executable: "/bin/pi".into(),
        working_directory: "/tmp".into(),
        ..Config::default()
    }
}

fn descriptor(adapter: &PiAdapter) -> Descriptor {
    futures::executor::block_on(adapter.probe()).unwrap()
}

fn refusal(adapter: &PiAdapter, request: OpenRequest) -> String {
    match futures::executor::block_on(adapter.open(request)) {
        Ok(_) => panic!("open must be refused"),
        Err(error) => error.to_string(),
    }
}

fn user() -> Participant {
    Participant {
        id: "user".into(),
        ..Participant::default()
    }
}

#[test]
fn a_probe_advertises_the_capabilities_go_advertises() {
    let mut expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/go-descriptor.json")).unwrap();
    expected["endpoint"]["version"] = pin().endpoint_version.clone().into();
    let described = descriptor(&PiAdapter::new(config()).unwrap());
    assert_eq!(
        serde_json::to_value(&described.capabilities).unwrap(),
        expected
    );
    assert_eq!(described.capability_revision, pin().capability_revision);
    assert_eq!(described.journal.scope, "session");
    assert_eq!(described.journal.persistence, "process_memory");
    assert_eq!(described.journal.replay.as_str(), "degraded");
    assert_eq!(described.journal.capacity, 256);
    assert_eq!(described.max_active_runs_per_session, 1);
    assert!(!described.interactive_gates);
    assert_eq!(described.cancellation_target, "run");
    assert_eq!(
        described.cancellation_implementation,
        "native_abort_with_agent_settled_authority"
    );
}

#[test]
fn a_configured_journal_capacity_is_the_one_described() {
    let adapter = PiAdapter::new(Config {
        journal_capacity: 9,
        ..config()
    })
    .unwrap();
    assert_eq!(descriptor(&adapter).journal.capacity, 9);
}

#[test]
fn construction_refuses_what_go_refuses() {
    let refused = |config: Config| PiAdapter::new(config).err().unwrap().to_string();
    assert_eq!(
        refused(Config::default()),
        "pi adapter: factory or executable is required"
    );
    for directory in ["", "relative"] {
        assert_eq!(
            refused(Config {
                working_directory: directory.into(),
                ..config()
            }),
            "pi adapter: absolute working directory is required"
        );
    }
    assert_eq!(
        refused(Config {
            args: vec!["--model".into(), "--".into()],
            ..config()
        }),
        "pi adapter: standalone -- prevents enforced extension disabling"
    );
    for arg in ["--extension", "-e", "--extension=x"] {
        assert_eq!(
            refused(Config {
                args: vec![arg.into()],
                ..config()
            }),
            "pi adapter: explicit extensions are incompatible with canonical prompt admission"
        );
    }
}

#[test]
fn launch_arguments_disable_extensions_after_the_configured_ones() {
    let adapter = PiAdapter::new(Config {
        args: vec!["--model".into(), "m".into()],
        environment: Some(vec!["A=1".into()]),
        ..config()
    })
    .unwrap();
    let launch = adapter.process_config();
    assert_eq!(launch.path, "/bin/pi");
    assert_eq!(launch.args, ["--model", "m", "--no-extensions"]);
    assert_eq!(launch.dir, "/tmp");
    assert_eq!(launch.env, Some(vec!["A=1".to_owned()]));
    assert_eq!(PiAdapter::new(config()).unwrap().process_config().env, None);
}

#[test]
fn open_refuses_a_missing_participant_provided_tools_and_a_threshold_policy_as_go_does() {
    let adapter = PiAdapter::new(config()).unwrap();
    assert_eq!(
        refusal(&adapter, OpenRequest::default()),
        Error::InvalidParticipant.to_string()
    );
    assert_eq!(
        refusal(
            &adapter,
            OpenRequest {
                participant: user(),
                tools: vec![ToolDefinition {
                    name: "x".into(),
                    ..ToolDefinition::default()
                }],
                ..OpenRequest::default()
            }
        ),
        "adapter: unsupported input: action.tools.provide (unadvertised)"
    );
    assert_eq!(
        refusal(
            &adapter,
            OpenRequest {
                participant: user(),
                tool_sources: vec![ToolSourceAttachment::default()],
                ..OpenRequest::default()
            }
        ),
        "adapter: unsupported input: action.tool_sources.attach (unadvertised)"
    );
    for kind in ["share", "tokens"] {
        assert_eq!(
            refusal(
                &adapter,
                OpenRequest {
                    participant: user(),
                    compaction_policy: Some(CompactionPolicy {
                        kind: kind.into(),
                        ..CompactionPolicy::default()
                    }),
                    ..OpenRequest::default()
                }
            ),
            "adapter: unsupported input: session.compaction.policy (unsatisfiable): Pi switches its own compaction on or off; its threshold lives in a settings file"
        );
    }
}

#[test]
fn open_refuses_an_unusable_reopen_binding_before_a_process_starts() {
    let adapter = PiAdapter::new(config()).unwrap();
    let error = futures::executor::block_on(adapter.open(OpenRequest {
        participant: user(),
        reopen: true,
        native_session_id: r#"{"sessionId":"bound-session","sessionFile":"session.jsonl"}"#.into(),
        ..OpenRequest::default()
    }))
    .err()
    .expect("an unusable binding must be refused");
    match error {
        Error::UnsupportedControl(control) => {
            assert_eq!(control.feature, "session.open.reopen");
            assert_eq!(control.reason, "unsatisfiable");
        }
        other => panic!("refusal={other}"),
    }
}
