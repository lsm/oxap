#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use futures::executor::block_on;
use oxap_adapter::{Adapter as _, Error, OpenRequest};
use oxap_adapter_claude::{adapter::pin, Adapter, Config, ToolPosture};
use oxap_protocol::ReasoningLevel;
use serde_json::{json, Value};

fn open_fixture(mode: &str) {
    let directory = tempfile::tempdir().unwrap();
    let peer = directory.path().join("peer");
    fs::write(&peer, include_str!("session_open_peer.py")).unwrap();
    fs::set_permissions(&peer, fs::Permissions::from_mode(0o700)).unwrap();
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(&pin().corpus);
    let adapter = Adapter::new(Config {
        executable: Some(peer.display().to_string()),
        tools: ToolPosture::Unrestricted,
        model: Some("configured-model".into()),
        working_directory: Some(directory.path().display().to_string()),
        environment: vec![
            format!("PATH={}", std::env::var("PATH").unwrap()),
            format!("HOME={}", directory.path().display()),
            "NODE_ENV=test".into(),
            format!(
                "FIXTURE={}",
                corpus.join("initialize-lifecycle/native.jsonl").display()
            ),
            format!("MODE={mode}"),
        ],
        ..Default::default()
    })
    .unwrap();
    let reasoning = if mode == "plain" {
        ReasoningLevel::default()
    } else {
        ReasoningLevel::HIGH
    };
    let request = OpenRequest {
        session_id: "session".into(),
        reasoning_level: reasoning.clone(),
        ..Default::default()
    };
    let opened = block_on(adapter.open(request));
    if mode.ends_with("-refusal") {
        let error = opened.err().unwrap().to_string();
        let exchange = if mode == "initialize-refusal" {
            "initialize exchange"
        } else {
            "apply_flag_settings"
        };
        assert!(
            error.contains(exchange) && error.contains("fixture refusal"),
            "{error}"
        );
    } else {
        let session = opened.unwrap();
        let state = block_on(session.state()).unwrap();
        assert_eq!(state.session_id.as_str(), "session");
        assert_eq!(state.status.as_str(), "idle");
        assert_eq!(state.current_model_id, "claude-sonnet-4-5");
        assert_eq!(state.reasoning_level, reasoning);
        assert!(state.updated_at_ms > 0);
        let binding = session.native_session_id().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(state.metadata["claude_native_session_id"].get())
                .unwrap(),
            json!(binding)
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("binding")).unwrap(),
            binding
        );
        block_on(session.close()).unwrap();
        block_on(session.close()).unwrap();
        assert!(matches!(
            block_on(session.state()),
            Err(Error::SessionClosed)
        ));
    }
    assert!(directory.path().join("eof").is_file());
    println!("PASS {mode}: recorded exchanges and gateway EOF");
}

#[test]
fn opens_and_closes_the_corpus_gateway() {
    for mode in [
        "plain",
        "settings",
        "initialize-refusal",
        "settings-refusal",
    ] {
        open_fixture(mode);
    }
    println!("4/4 Claude startup cases passed");
}
