#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    task::{Context, Poll},
    thread,
    time::{Duration, Instant},
};

use futures::executor::block_on;
use oxap_adapter::{Adapter as _, Error, OpenRequest, SubmitRequest};
use oxap_adapter_claude::{adapter::pin, Adapter, AuthMode, Config, Header, Provider, ToolPosture};
use oxap_protocol::{
    Message, MessageContent, MessageRole, MessageSubmitRequest, ReasoningLevel, ToolsListRequest,
    FEATURE_TOOLS_LIST,
};
use serde_json::{json, Value};

fn fixture(mode: &str) -> (Adapter, tempfile::TempDir, Vec<Value>) {
    let (config, directory, frames) = fixture_config(mode);
    (Adapter::new(config).unwrap(), directory, frames)
}

fn fixture_config(mode: &str) -> (Config, tempfile::TempDir, Vec<Value>) {
    let directory = tempfile::tempdir().unwrap();
    let peer = directory.path().join("peer");
    fs::write(&peer, include_str!("session_open_peer.py")).unwrap();
    fs::set_permissions(&peer, fs::Permissions::from_mode(0o700)).unwrap();
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(&pin().corpus);
    let corpus = corpus.join(if mode == "catalog" {
        "tools-catalog-sources/native.jsonl"
    } else {
        "initialize-lifecycle/native.jsonl"
    });
    let frames = fs::read_to_string(&corpus)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let config = Config {
        executable: Some(peer.display().to_string()),
        tools: ToolPosture::Unrestricted,
        model: Some("configured-model".into()),
        working_directory: Some(directory.path().display().to_string()),
        environment: vec![
            format!("PATH={}", std::env::var("PATH").unwrap()),
            format!("HOME={}", directory.path().display()),
            "NODE_ENV=test".into(),
            format!("FIXTURE={}", corpus.display()),
            format!("MODE={mode}"),
        ],
        ..Default::default()
    };
    (config, directory, frames)
}

fn open_fixture(mode: &str) {
    let (adapter, directory, _) = fixture(mode);
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

#[test]
fn configured_provider_reaches_the_cli_peer() {
    let (mut config, directory, _) = fixture_config("provider");
    config.environment.extend([
        "ANTHROPIC_BASE_URL=https://old.example".into(),
        "ANTHROPIC_CUSTOM_HEADERS=X-Host: first".into(),
    ]);
    config.provider = Some(Provider {
        base_url: "http://127.0.0.1:1/gateway".into(),
        auth: AuthMode::None,
        secret: String::new(),
    });
    config.headers = [("X-Trace", "two words"), ("x-trace", "third")]
        .map(|(name, value)| Header {
            name: name.into(),
            value: value.into(),
        })
        .to_vec();
    let adapter = Adapter::new(config).unwrap();
    let session = block_on(adapter.open(OpenRequest::default())).unwrap();
    block_on(session.close()).unwrap();
    assert!(directory.path().join("eof").is_file());
    println!("PASS Rust provider launch: route, ordered headers, initialize, and EOF");
}

fn text_submit(text: &str) -> SubmitRequest {
    SubmitRequest {
        request: MessageSubmitRequest {
            session_id: "session".into(),
            messages: vec![Message {
                role: MessageRole::USER,
                content: MessageContent::from_text(text),
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn writes_one_native_turn_without_admitting_on_write() {
    let (adapter, directory, _) = fixture("submit-text");
    let session = block_on(adapter.open(OpenRequest {
        session_id: "session".into(),
        ..Default::default()
    }))
    .unwrap();

    let mut submission = session.submit(text_submit("hello"));
    let mut context = Context::from_waker(futures::task::noop_waker_ref());
    match submission.as_mut().poll(&mut context) {
        Poll::Pending => {}
        Poll::Ready(Err(error)) => panic!("native submit refused: {error}"),
        Poll::Ready(Ok(_)) => panic!("write alone must not admit a run"),
    }
    let observed = directory.path().join("turn-observed");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !observed.is_file() {
        assert!(Instant::now() < deadline, "no turn reached native stdin");
        thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(
        block_on(session.submit(text_submit("again"))),
        Err(Error::RunActive)
    ));
    assert!(matches!(block_on(session.close()), Err(Error::RunActive)));
    let state = block_on(session.state()).unwrap();
    assert!(state.active_run_id.is_none());
    assert!(state.active_runs.is_empty());
    fs::write(directory.path().join("release-turn"), []).unwrap();
    assert!(matches!(block_on(submission), Err(Error::SessionClosed)));
    block_on(session.close()).unwrap();
    assert!(directory.path().join("no-extra-turn").is_file());
    println!(
        "PASS text submit: corpus turn reached stdin once, overlap refused, no premature admission"
    );
}

#[test]
fn lists_the_idle_gateway_catalog_and_replaces_it_on_refresh() {
    let (adapter, directory, frames) = fixture("catalog");
    let session = block_on(adapter.open(OpenRequest {
        session_id: "session".into(),
        ..Default::default()
    }))
    .unwrap();
    let lister = session
        .tool_lister()
        .expect("Claude serves action.tools.list");
    assert!(matches!(
        block_on(lister.tools(ToolsListRequest {
            session_id: Some("session".into()),
            ..Default::default()
        })),
        Err(Error::DegradedControl(feature)) if feature == FEATURE_TOOLS_LIST
    ));
    let request = ToolsListRequest {
        session_id: Some("session".into()),
        allow_degraded_features: vec![FEATURE_TOOLS_LIST.into()],
    };
    assert!(matches!(
        block_on(lister.tools(ToolsListRequest {
            session_id: Some("other".into()),
            ..request.clone()
        })),
        Err(Error::RunNotFound)
    ));
    let mut expected = frames
        .iter()
        .map(|frame| &frame["raw"]["catalog"])
        .filter(|catalog| !catalog.is_null());
    let empty = expected.next().unwrap();
    let populated = expected.next().unwrap();
    let list = || {
        let reply = block_on(lister.tools(request.clone())).unwrap();
        assert_eq!(reply.revision, pin().capability_revision);
        serde_json::to_value(reply.tools).unwrap()
    };
    let publish = |step: &str| {
        fs::write(directory.path().join(step), []).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while block_on(session.state()).unwrap().current_model_id != step {
            assert!(Instant::now() < deadline, "idle init {step} did not arrive");
            thread::sleep(Duration::from_millis(10));
        }
    };
    assert_eq!(&list(), empty);
    publish("publish-initial");
    assert_eq!(&list(), populated);
    let mut unscoped = empty.clone();
    unscoped.as_object_mut().unwrap().remove("session_id");
    for session_id in [None, Some("".into())] {
        let reply = block_on(lister.tools(ToolsListRequest {
            session_id,
            ..request.clone()
        }))
        .unwrap();
        assert_eq!(serde_json::to_value(reply.tools).unwrap(), unscoped);
    }
    publish("publish-refresh");
    let mut refreshed = populated.clone();
    refreshed["sources"].as_array_mut().unwrap().truncate(2);
    let tool = &mut refreshed["tools"][3];
    tool["source"] = json!("mcp:files");
    refreshed["tools"] = json!([tool]);
    assert_eq!(list(), refreshed);
    block_on(session.close()).unwrap();
    assert!(matches!(
        block_on(lister.tools(request)),
        Err(Error::SessionClosed)
    ));
    assert!(directory.path().join("eof").is_file());
    println!("PASS catalog: two corpus catalogs, scoped refresh, refusals and gateway EOF");
}
