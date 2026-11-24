#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    task::{Context, Poll},
    thread,
    time::{Duration, Instant},
};

use futures::{executor::block_on, FutureExt, StreamExt};
use oxap_adapter::{Adapter as _, Error, OpenRequest, ReplayGap, ResumeRequest, SubmitRequest};
use oxap_adapter_claude::{adapter::pin, Adapter, AuthMode, Config, Header, Provider, ToolPosture};
use oxap_protocol::{
    ContentPart, ContentPartType, Envelope, Message, MessageContent, MessageRole,
    MessageSubmitRequest, ReasoningLevel, RequestedDeliveryMode, ToolsListRequest,
    FEATURE_DELIVERY_BTW, FEATURE_DELIVERY_QUEUE, FEATURE_DELIVERY_STEER, FEATURE_INSTRUCTIONS,
    FEATURE_MODEL_SELECTION, FEATURE_STRUCTURED_OUTPUT, FEATURE_TOOLS_LIST, FEATURE_TOOL_SELECTION,
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
    let corpus = corpus.join(match mode {
        "catalog" => "tools-catalog-sources/native.jsonl",
        "run-streaming" | "run-buffered" => "streaming-provenance/native.jsonl",
        "run-settlement" => "settlement-statuses/native.jsonl",
        "run-process-exit" => "process-exit/native.jsonl",
        "run-malformed" => "malformed-stdout/native.jsonl",
        mode if mode.starts_with("run-tools") => "tool-lifecycle/native.jsonl",
        mode if mode.starts_with("run-interrupt-") => "interrupt-cancel/native.jsonl",
        "run-hygiene" | "run-native-wins" | "run-native-buffered" | "run-aborted" => {
            "hygiene-recovery/native.jsonl"
        }
        mode if mode.starts_with("admission-") => "admission-corroboration/native.jsonl",
        _ => "initialize-lifecycle/native.jsonl",
    });
    let frames = fs::read_to_string(&corpus)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let config = Config {
        executable: Some(peer.display().to_string()),
        tools: ToolPosture::Unrestricted,
        model: Some(
            if mode.starts_with("admission-") || mode.starts_with("run-") {
                "claude-test"
            } else {
                "configured-model"
            }
            .into(),
        ),
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
    verify_submit("submit-text", text_submit("hello"), false);
    let mut request = text_submit("");
    request.request.delivery = RequestedDeliveryMode::AUTO;
    request.request.messages[0].content = MessageContent::from_parts(&[
        ContentPart {
            part_type: ContentPartType::TEXT,
            text: "你好 <&>".into(),
            ..Default::default()
        },
        ContentPart {
            part_type: ContentPartType::TEXT,
            text: "second line".into(),
            ..Default::default()
        },
    ]);
    verify_submit("submit-parts", request, true);
    println!("2/2 Claude submit peer cases passed");
}

fn verify_submit(mode: &str, request: SubmitRequest, expand_prompts: bool) {
    let (mut config, directory, _) = fixture_config(mode);
    config.expand_prompts = expand_prompts;
    let adapter = Adapter::new(config).unwrap();
    let session = block_on(adapter.open(OpenRequest {
        session_id: "session".into(),
        ..Default::default()
    }))
    .unwrap();

    check_invalid_submits(session.as_ref());
    let mut submission = session.submit(request);
    let mut context = Context::from_waker(futures::task::noop_waker_ref());
    match submission.as_mut().poll(&mut context) {
        Poll::Pending => {}
        Poll::Ready(Err(error)) => panic!("native submit refused: {error}"),
        Poll::Ready(Ok(_)) => panic!("write alone must not admit a run"),
    }
    wait_for_turn(directory.path());
    assert!(matches!(
        block_on(session.submit(text_submit("again"))),
        Err(Error::RunActive)
    ));
    assert!(matches!(block_on(session.close()), Err(Error::RunActive)));
    let state = block_on(session.state()).unwrap();
    assert_eq!(state.status.as_str(), "idle");
    assert!(state.active_run_id.is_none());
    assert!(state.active_runs.is_empty());
    fs::write(directory.path().join("release-turn"), []).unwrap();
    assert!(matches!(block_on(submission), Err(Error::SessionClosed)));
    assert!(matches!(
        block_on(session.submit(text_submit("after EOF"))),
        Err(Error::SessionClosed)
    ));
    block_on(session.close()).unwrap();
    assert!(directory.path().join("no-extra-turn").is_file());
    println!("PASS {mode}: one native turn, invalid and overlapping submits refused, no premature admission");
}

#[test]
fn claude_admission_waits_for_corroboration() {
    for mode in ["admission-single", "admission-list", "admission-compacting"] {
        let (adapter, directory, frames) = fixture(mode);
        let session = block_on(adapter.open(OpenRequest {
            session_id: "session".into(),
            ..Default::default()
        }))
        .unwrap();
        let mut submission = session.submit(text_submit("hello"));
        let mut context = Context::from_waker(futures::task::noop_waker_ref());
        assert!(submission.as_mut().poll(&mut context).is_pending());
        wait_for_turn(directory.path());
        let idle = block_on(session.state()).unwrap();
        assert_eq!(idle.status.as_str(), "idle");
        assert!(idle.active_run_id.is_none());
        assert!(submission.as_mut().poll(&mut context).is_pending());

        fs::write(directory.path().join("release-turn"), []).unwrap();
        let (admission, mut stream) = block_on(submission).expect(mode);
        assert_eq!(idle.current_model_id, "claude-test");
        let start = stream
            .next()
            .now_or_never()
            .expect("run.started is ready")
            .unwrap()
            .unwrap();
        let mut payload = serde_json::to_value(&start).unwrap()["payload"].clone();
        let expected: Vec<Value> = serde_json::from_str(
            &fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../..")
                    .join(&pin().corpus)
                    .join("admission-corroboration/expected-oap.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let run_id = admission.run_id.as_ref().unwrap();
        assert_eq!(start.envelope_type.as_str(), expected[0]["type"]);
        assert_eq!(start.sequence, Some(1));
        assert_eq!(start.run_id.as_ref(), Some(run_id));
        assert!(payload["started_at_ms"].as_i64().unwrap() > 0);
        payload["started_at_ms"] = expected[0]["payload"]["started_at_ms"].clone();
        payload["run_id"] = expected[0]["payload"]["run_id"].clone();
        assert_eq!(payload, expected[0]["payload"].clone());
        assert!(admission.accepted);
        assert_eq!(admission.model_id, payload["model_id"]);
        assert_eq!(admission.admission.as_str(), "started");
        assert_eq!(admission.effective_delivery.as_str(), "start");
        assert_eq!(admission.status.as_str(), "running");
        let running = block_on(session.state()).unwrap();
        assert_eq!(running.status.as_str(), "running");
        assert_eq!(running.active_run_id.as_ref(), Some(run_id));
        assert_eq!(running.current_model_id, frames[5]["raw"]["model"]);
        assert!(stream.next().poll_unpin(&mut context).is_pending());
        assert!(matches!(
            block_on(session.submit(text_submit("again"))),
            Err(Error::RunActive)
        ));
        println!("PASS {mode}: only corroborated turn admitted, run.started first, buffered metadata replayed");
    }
    println!("3/3 Claude admission peer cases passed");
}

#[test]
fn claude_admission_refuses_a_closed_session_after_corroboration() {
    let (adapter, directory, _) = fixture("admission-disconnect");
    let session = block_on(adapter.open(OpenRequest {
        session_id: "session".into(),
        ..Default::default()
    }))
    .unwrap();
    let mut submission = session.submit(text_submit("hello"));
    let mut context = Context::from_waker(futures::task::noop_waker_ref());
    assert!(submission.as_mut().poll(&mut context).is_pending());
    wait_for_turn(directory.path());
    fs::write(directory.path().join("release-turn"), []).unwrap();

    wait_until_closed(session.as_ref());
    assert!(directory.path().join("echo-drained").is_file());
    assert!(
        matches!(block_on(submission), Err(Error::SessionClosed)),
        "a closed session must not admit a run"
    );
    block_on(session.close()).unwrap();
    println!("PASS admission-disconnect: the reader drained the echo, then EOF refused the pending submit");
}

#[test]
fn claude_run_replays_streaming_and_settlement() {
    for (mode, case) in [
        ("run-streaming", "streaming-provenance"),
        ("run-buffered", "streaming-provenance"),
        ("run-settlement", "settlement-statuses"),
        ("run-hygiene", "hygiene-recovery"),
    ] {
        replay_run_events(mode, case);
    }
}

#[test]
fn claude_tools_replay_corpus() {
    replay_run_events("run-tools", "tool-lifecycle");
}

fn replay_run_events(mode: &str, case: &str) {
    let (adapter, directory, frames) = fixture(mode);
    let session = block_on(adapter.open(OpenRequest {
        session_id: "session".into(),
        ..Default::default()
    }))
    .unwrap();
    let expected = expected_run_events(case);
    let runs: Vec<_> = expected
        .split_inclusive(|event| {
            matches!(event["type"].as_str(), Some("run.completed" | "run.failed"))
        })
        .collect();
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["action"] == "submit")
            .count(),
        runs.len()
    );
    for (index, (expected, frame)) in runs
        .iter()
        .zip(frames.iter().filter(|frame| frame["action"] == "submit"))
        .enumerate()
    {
        let text = frame["raw"]["message"]["content"].as_str().unwrap();
        let (admission, stream) = block_on(session.submit(text_submit(text))).unwrap();
        if mode == "run-streaming" {
            let running = block_on(session.state()).unwrap();
            assert_eq!(running.transcript_cursor, "1");
        }
        fs::write(directory.path().join(format!("continue-{index}")), []).unwrap();
        let actual: Vec<_> = block_on(stream.collect::<Vec<_>>())
            .into_iter()
            .collect::<Result<Vec<Envelope>, _>>()
            .unwrap();
        assert_run_events(
            &admission,
            &actual,
            expected,
            &format!("{mode} run {index}"),
        );
        let idle = block_on(session.state()).unwrap();
        assert_eq!(idle.status.as_str(), "idle");
        assert!(idle.active_run_id.is_none());
        assert!(idle.active_runs.is_empty());
        assert_eq!(idle.transcript_cursor, expected.len().to_string());
    }
    block_on(session.close()).unwrap();
    assert!(directory.path().join("eof").is_file());
    println!(
        "PASS {mode}: {} runs and {} corpus envelopes",
        runs.len(),
        expected.len()
    );
}

#[test]
fn claude_journal_recovers_live_and_completed_runs() {
    let (config, directory, _) = fixture_config("run-streaming");
    let adapter = Adapter::new(Config {
        journal_capacity: 2,
        ..config
    })
    .unwrap();
    let session = block_on(adapter.open(OpenRequest {
        session_id: "session".into(),
        ..Default::default()
    }))
    .unwrap();
    let (admission, original) = block_on(session.submit(text_submit("hello"))).unwrap();
    let run_id = admission.run_id.unwrap();
    let resume = |after_sequence| {
        block_on(session.resume(ResumeRequest {
            run_id: run_id.clone(),
            after_sequence,
        }))
    };
    let (live, replay) = resume(0).expect("resume the live run from the beginning");
    assert_eq!((live.replayed_from, live.replayed_through), (1, 1));
    assert_eq!(live.state.active_run_id.as_ref(), Some(&run_id));
    let (head, mut follow) = resume(1).unwrap();
    assert_eq!((head.replayed_from, head.replayed_through), (1, 1));
    assert!(follow.next().now_or_never().is_none());
    assert!(matches!(resume(2), Err(Error::ReplayCursorFuture)));
    assert!(matches!(
        block_on(session.resume(ResumeRequest {
            run_id: session.native_session_id().unwrap().into(),
            after_sequence: 0,
        })),
        Err(Error::RunNotFound)
    ));

    fs::write(directory.path().join("continue-0"), []).unwrap();
    let original = stream_bytes(original);
    assert_eq!(original.len(), 5);
    assert_eq!(stream_bytes(replay), original);
    assert_eq!(stream_bytes(follow), original[1..]);
    println!(
        "PASS live recovery: {} original envelopes matched byte for byte",
        original.len()
    );

    assert!(matches!(
        resume(2),
        Err(Error::ReplayGap(ReplayGap {
            requested_after: 2,
            oldest_available: 4,
            latest_available: 5,
        }))
    ));
    let (done, suffix) = resume(3).unwrap();
    assert_eq!(done.state.status.as_str(), "idle");
    assert_eq!((done.replayed_from, done.replayed_through), (4, 5));
    assert_eq!(stream_bytes(suffix), original[3..]);
    let (done, eof) = resume(5).unwrap();
    assert_eq!((done.replayed_from, done.replayed_through), (5, 5));
    assert!(stream_bytes(eof).is_empty());
    block_on(session.close()).unwrap();
    assert!(directory.path().join("eof").is_file());
    assert!(matches!(resume(5), Err(Error::SessionClosed)));
    println!("PASS completed recovery: cursor gap 2/4/5, 2 retained envelopes, empty replay at end and explicit close");
}

#[test]
fn claude_journal_recovers_the_inferred_terminal_after_disconnect() {
    let (adapter, directory, _) = fixture("run-process-exit");
    let session = block_on(adapter.open(OpenRequest {
        session_id: "session".into(),
        ..Default::default()
    }))
    .unwrap();
    let (admission, original) = block_on(session.submit(text_submit("hello"))).unwrap();
    fs::write(directory.path().join("continue-0"), []).unwrap();
    let original = stream_bytes(original);
    wait_until_closed(session.as_ref());
    let (recovery, replay) = block_on(session.resume(ResumeRequest {
        run_id: admission.run_id.unwrap(),
        after_sequence: 1,
    }))
    .expect("resume the settled run after native EOF");
    assert_eq!((recovery.replayed_from, recovery.replayed_through), (2, 3));
    assert_eq!(recovery.state.status.as_str(), "idle");
    assert_eq!(original.len(), 3);
    assert_eq!(stream_bytes(replay), original[1..]);
    block_on(session.close()).unwrap();
    println!(
        "PASS disconnect recovery: {} retained envelopes including inferred failure",
        original.len() - 1
    );
}

fn stream_bytes(stream: oxap_adapter::EventStream) -> Vec<Vec<u8>> {
    block_on(
        stream
            .map(|event| serde_json::to_vec(&event.unwrap()).unwrap())
            .collect(),
    )
}

#[test]
fn claude_interrupt_replays_the_corpus_and_keeps_ownership() {
    check_interrupt("run-interrupt-held", "run.cancelled");
    check_interrupt("run-interrupt-immediate", "run.cancelled");
}

#[test]
fn claude_interrupt_arbitrates_terminal_and_receipt_order() {
    check_interrupt("run-interrupt-terminal-first", "run.cancelled");
    check_interrupt("run-interrupt-completion", "run.completed");
}

#[test]
fn claude_interrupt_requires_a_valid_receipt_for_cancellation() {
    check_interrupt("run-interrupt-refused", "run.failed");
    check_interrupt("run-interrupt-malformed", "run.failed");
    check_interrupt("run-interrupt-no-receipt", "run.failed");
}

fn check_interrupt(mode: &str, terminal: &str) {
    let (adapter, directory, _) = fixture(mode);
    let session = block_on(adapter.open(OpenRequest {
        session_id: "session".into(),
        ..Default::default()
    }))
    .unwrap();
    let (admission, mut stream) = block_on(session.submit(text_submit("hello"))).unwrap();
    let run = admission.run_id.as_ref().unwrap();
    let mut events = vec![
        block_on(stream.next()).unwrap().unwrap(),
        block_on(stream.next()).unwrap().unwrap(),
    ];
    assert!(matches!(
        block_on(session.cancel("unknown".into())),
        Err(Error::RunNotFound)
    ));
    let mut cancel = session.cancel(run.clone());
    let mut context = Context::from_waker(futures::task::noop_waker_ref());
    assert!(
        cancel.as_mut().poll(&mut context).is_pending(),
        "{mode}: interrupt did not wait for receipt"
    );
    wait_for_peer(directory.path(), "interrupt-observed");
    let mut duplicate = session.cancel(run.clone());
    assert!(duplicate.as_mut().poll(&mut context).is_pending());
    fs::write(directory.path().join("continue-0"), []).unwrap();
    if mode == "run-interrupt-completion" {
        events.push(block_on(stream.next()).unwrap().unwrap());
        assert_eq!(events[2].envelope_type.as_str(), terminal);
        assert!(matches!(
            block_on(session.submit(text_submit("too early"))),
            Err(Error::RunActive)
        ));
        fs::write(directory.path().join("continue-1"), []).unwrap();
    }
    if mode == "run-interrupt-terminal-first" {
        wait_for_peer(directory.path(), "terminal-sent");
        fs::write(directory.path().join("continue-1"), []).unwrap();
    }
    let ack = block_on(cancel);
    assert_eq!(block_on(duplicate), ack);
    if mode == "run-interrupt-held" {
        assert_eq!(block_on(session.cancel(run.clone())), ack);
        assert!(matches!(
            block_on(session.submit(text_submit("too early"))),
            Err(Error::RunActive)
        ));
        assert!(matches!(block_on(session.close()), Err(Error::RunActive)));
        assert_eq!(
            block_on(session.state()).unwrap().active_run_id.as_ref(),
            Some(run)
        );
        assert!(stream.next().now_or_never().is_none());
        fs::write(directory.path().join("continue-1"), []).unwrap();
    }
    if terminal == "run.failed" {
        assert!(matches!(ack, Err(Error::Harness(_))), "{mode}: {ack:?}");
    } else {
        let ack = ack.unwrap();
        assert!(ack.accepted);
        assert_eq!(ack.status.as_str(), "cancelling");
        assert_eq!(&ack.run_id, run);
        assert_eq!(ack.session_id, admission.session_id);
    }
    events.extend(
        block_on(stream.collect::<Vec<_>>())
            .into_iter()
            .map(Result::unwrap),
    );
    assert_eq!(events.len(), 3);
    assert_eq!(events[2].envelope_type.as_str(), terminal);
    assert_eq!(events[2].sequence, Some(3));
    if terminal == "run.cancelled" {
        assert_run_events(
            &admission,
            &events,
            &expected_run_events("interrupt-cancel"),
            mode,
        );
    }
    if terminal == "run.failed" {
        let event = serde_json::to_value(&events[2]).unwrap();
        assert_eq!(
            event["payload"]["error"]["code"],
            "claude_aborted_streaming"
        );
    }
    let retry = block_on(session.cancel(run.clone()));
    let status = terminal.strip_prefix("run.").unwrap();
    if status == "cancelled" {
        let retry = retry.unwrap();
        assert!(retry.accepted);
        assert_eq!(retry.status.as_str(), status);
        assert_eq!(&retry.run_id, run);
    } else {
        assert!(
            matches!(
                retry,
                Err(Error::RunAlreadyTerminal {
                    run_id: Some(ref id),
                    status: Some(ref actual),
                }) if id == run && actual.as_str() == status
            ),
            "{mode}: {retry:?}"
        );
    }
    let (recovery, replay) = block_on(session.resume(ResumeRequest {
        run_id: run.clone(),
        after_sequence: 2,
    }))
    .unwrap();
    assert!(recovery.state.active_run_id.is_none());
    assert_eq!(
        stream_bytes(replay),
        [serde_json::to_vec(&events[2]).unwrap()]
    );
    block_on(session.close()).unwrap();
    assert!(matches!(
        block_on(session.cancel(run.clone())),
        Err(Error::SessionClosed)
    ));
    if mode != "run-interrupt-no-receipt" {
        assert!(directory.path().join("eof").is_file());
    }
    println!("PASS {mode}: 3 envelopes, one {terminal}, one native interrupt");
}

fn expected_run_events(case: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(&pin().corpus)
        .join(case)
        .join("expected-oap.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn assert_run_events(
    admission: &oxap_protocol::MessageSubmitResponse,
    actual: &[Envelope],
    expected: &[Value],
    label: &str,
) {
    assert_eq!(
        actual
            .iter()
            .map(|event| event.envelope_type.as_str())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        "{label}"
    );
    let mut event_ids = std::collections::HashMap::<String, Value>::new();
    let mut tool_ids = std::collections::HashMap::new();
    for (event, expected) in actual.iter().zip(expected) {
        let mut event = serde_json::to_value(event).unwrap();
        if !event["in_reply_to"].is_null() {
            event["in_reply_to"] = event_ids[event["in_reply_to"].as_str().unwrap()].clone();
        }
        assert!(event_ids
            .insert(
                event["id"].as_str().unwrap().to_owned(),
                expected["id"].clone()
            )
            .is_none());
        if let Some(id) = event["tool_call_id"].as_str() {
            assert_eq!(event["payload"]["tool_call_id"], id);
            if event["type"] == "action.call.requested" {
                assert!(tool_ids
                    .insert(id.to_owned(), expected["tool_call_id"].clone())
                    .is_none());
            }
            let normalized = tool_ids[id].clone();
            event["tool_call_id"] = normalized.clone();
            event["payload"]["tool_call_id"] = normalized;
        }
        assert_eq!(event["run_id"], admission.run_id.as_ref().unwrap().as_str());
        assert_eq!(event["payload"]["run_id"], event["run_id"]);
        let message = &event["payload"]["message_id"];
        if !message.is_null() {
            assert_eq!(message, admission.message_ids[0].as_str());
            event["payload"]["message_id"] = expected["payload"]["message_id"].clone();
        }
        let final_id = &event["payload"]["final_response"]["id"];
        if !final_id.is_null() {
            assert_eq!(final_id, admission.message_ids[0].as_str());
            event["payload"]["final_response"]["id"] =
                expected["payload"]["final_response"]["id"].clone();
        }
        assert!(event["timestamp_ms"].as_i64().unwrap() > 0);
        event["timestamp_ms"] = expected["timestamp_ms"].clone();
        event["id"] = expected["id"].clone();
        event["run_id"] = expected["run_id"].clone();
        event["payload"]["run_id"] = expected["payload"]["run_id"].clone();
        if event["type"] == "run.started" {
            assert!(event["payload"]["started_at_ms"].as_i64().unwrap() > 0);
            event["payload"]["started_at_ms"] = expected["payload"]["started_at_ms"].clone();
        }
        assert_eq!(&event, expected, "{label}");
    }
}

#[test]
fn claude_arbitration_drains_stdout_before_settling() {
    for (mode, case) in [
        ("run-process-exit", "process-exit"),
        ("run-malformed", "malformed-stdout"),
        ("run-native-wins", "hygiene-recovery"),
        ("run-native-buffered", "hygiene-recovery"),
        ("run-aborted", "hygiene-recovery"),
    ] {
        let (adapter, directory, _) = fixture(mode);
        let session = block_on(adapter.open(OpenRequest {
            session_id: "session".into(),
            ..Default::default()
        }))
        .unwrap();
        let mut submission = session.submit(text_submit("hello"));
        let mut context = Context::from_waker(futures::task::noop_waker_ref());
        assert!(submission.as_mut().poll(&mut context).is_pending());
        if mode == "run-native-buffered" {
            wait_until_closed(session.as_ref());
        }
        let (admission, stream) = block_on(submission).expect(mode);
        fs::write(directory.path().join("continue-0"), []).unwrap();
        let actual = block_on(stream.collect::<Vec<_>>())
            .into_iter()
            .collect::<Result<Vec<Envelope>, _>>()
            .unwrap();
        let mut expected = expected_run_events(case);
        if mode == "run-native-wins" {
            expected[2]["extensions"]["com.anthropic.claude-code.structured_output"] =
                json!({"findings": [{"line": 3}]});
        }
        if mode == "run-aborted" {
            let terminal = &mut expected[2];
            terminal["type"] = json!("run.failed");
            let payload = terminal["payload"].as_object_mut().unwrap();
            payload.remove("final_response");
            payload.remove("stop_reason");
            payload.insert(
                "error".into(),
                json!({
                    "code": "claude_aborted_tools", "message": "fixture abort"
                }),
            );
        }
        assert_run_events(&admission, &actual, &expected, mode);
        wait_until_closed(session.as_ref());
        assert!(matches!(
            block_on(session.submit(text_submit("retry"))),
            Err(Error::SessionClosed)
        ));
        block_on(session.close()).unwrap();
        println!("PASS {mode}: {} envelopes and one terminal", actual.len());
    }
}

fn wait_until_closed(session: &dyn oxap_adapter::Session) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(block_on(session.state()), Err(Error::SessionClosed)) {
        assert!(Instant::now() < deadline, "transport did not close");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_turn(directory: &Path) {
    wait_for_peer(directory, "turn-observed");
}

fn wait_for_peer(directory: &Path, marker: &str) {
    let observed = directory.join(marker);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !observed.is_file() {
        assert!(Instant::now() < deadline, "peer did not write {marker}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn dropping_an_uncorroborated_submit_allows_close() {
    let (adapter, directory, _) = fixture("submit-abandon");
    let session = block_on(adapter.open(OpenRequest {
        session_id: "session".into(),
        ..Default::default()
    }))
    .unwrap();
    let mut submission = session.submit(text_submit("hello"));
    let mut context = Context::from_waker(futures::task::noop_waker_ref());
    assert!(submission.as_mut().poll(&mut context).is_pending());
    wait_for_turn(directory.path());
    assert_eq!(block_on(session.close()), Err(Error::RunActive));
    drop(submission);

    block_on(session.close()).expect("dropping submit must release the session for close");
    assert!(directory.path().join("eof").is_file());
    assert!(matches!(
        block_on(session.state()),
        Err(Error::SessionClosed)
    ));
    assert!(matches!(
        block_on(session.submit(text_submit("retry"))),
        Err(Error::SessionClosed)
    ));
    println!("PASS submit-abandon: caller dropped submit after native write, then closed the session and the peer observed EOF");
}

fn check_invalid_submits(session: &dyn oxap_adapter::Session) {
    let original = serde_json::to_value(text_submit("invalid").request).unwrap();
    let submit_changed = |field: &str, value| {
        let mut wire = original.clone();
        wire[field] = value;
        block_on(session.submit(SubmitRequest {
            request: serde_json::from_value(wire).unwrap(),
            ..Default::default()
        }))
        .err()
        .expect("invalid request admitted")
    };
    let user_message = &original["messages"][0];
    for (field, invalid) in [
        ("session_id", json!("")),
        ("messages", json!([])),
        ("messages", json!([user_message, user_message])),
        (
            "messages",
            json!([{"role": "assistant", "content": "invalid"}]),
        ),
        (
            "messages",
            json!([{"role": "user", "content": [{"type": "image"}]}]),
        ),
        ("delivery", json!("start")),
    ] {
        assert_eq!(submit_changed(field, invalid), Error::InvalidSubmission);
    }
    assert_eq!(
        submit_changed("session_id", json!("other")),
        Error::RunNotFound
    );
    for (field, value, feature) in [
        ("delivery", json!("queue"), FEATURE_DELIVERY_QUEUE),
        ("delivery", json!("steer"), FEATURE_DELIVERY_STEER),
        ("delivery", json!("btw"), FEATURE_DELIVERY_BTW),
        ("instructions", json!("invalid"), FEATURE_INSTRUCTIONS),
        ("model_id", json!("other"), FEATURE_MODEL_SELECTION),
        ("output_schema", json!({}), FEATURE_STRUCTURED_OUTPUT),
        (
            "tool_choice",
            json!({"allowed": ["Read"]}),
            FEATURE_TOOL_SELECTION,
        ),
    ] {
        assert!(
            matches!(submit_changed(field, value), Error::UnsupportedControl(control) if control.feature == feature && control.reason == "unadvertised")
        );
    }
}

#[test]
fn a_failed_native_write_retires_the_session() {
    let (adapter, directory, _) = fixture("submit-write-refused");
    let session = block_on(adapter.open(OpenRequest {
        session_id: "session".into(),
        ..Default::default()
    }))
    .unwrap();
    assert!(matches!(
        block_on(session.submit(text_submit("undeliverable"))),
        Err(Error::Harness(detail)) if detail.contains("Broken pipe")
    ));
    assert!(matches!(
        block_on(session.state()),
        Err(Error::SessionClosed)
    ));
    assert!(matches!(
        block_on(session.submit(text_submit("retry"))),
        Err(Error::SessionClosed)
    ));
    fs::write(directory.path().join("release-turn"), []).unwrap();
    block_on(session.close()).unwrap();
    println!("PASS submit-write-refused: broken native stdin returned a write error and closed the session");
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
