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
