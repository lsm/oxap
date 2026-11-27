#![cfg(unix)]

use std::{
    collections::{HashSet, VecDeque},
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{
        atomic::{AtomicI64, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

use futures::executor::block_on;
use oxap_adapter::{
    gojson::go_marshal,
    runtime::Runtime,
    testkit::{drain, next, RunTrace},
    Adapter as _, Error, EventStream, InteractionResolution, OpenRequest, ResumeRequest, Session,
    SubmitRequest,
};
use oxap_adapter_claude::{adapter::pin, Adapter, Config, ToolPosture};
use oxap_protocol::{
    Envelope, InputAnswer, MessageSubmitRequest, MessageSubmitResponse, RunCancelResponse,
    ToolsListRequest, UserInputRequestedPayload, UserInputResolveRequest, FEATURE_TOOLS_LIST,
};
use serde_json::{json, Value};

const LIMIT: Duration = Duration::from_secs(5);
type Submission = Result<(MessageSubmitResponse, EventStream), Error>;
type Cancellation = Result<RunCancelResponse, Error>;

fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn bytes(mut value: Value) -> String {
    if let Some(events) = value.as_array_mut() {
        for event in events {
            event.as_object_mut().unwrap().remove("timestamp_ms");
        }
    }
    value.sort_all_objects();
    go_marshal(&value.to_string())
}

fn async_call<T: Send + 'static>(call: impl FnOnce() -> T + Send + 'static) -> mpsc::Receiver<T> {
    let (send, receive) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = send.send(call());
    });
    receive
}

fn complete<T>(receive: mpsc::Receiver<T>) -> T {
    receive
        .recv_timeout(LIMIT)
        .expect("adapter operation did not settle")
}

struct Peer(BufReader<UnixStream>);

impl Peer {
    fn command(&mut self, value: Value) {
        writeln!(self.0.get_mut(), "{value}").unwrap();
    }

    fn response(&mut self) -> Value {
        let mut line = String::new();
        assert_ne!(self.0.read_line(&mut line).unwrap(), 0, "peer closed");
        serde_json::from_str(&line).unwrap()
    }

    fn compare_write(&mut self, expected: &Value, pending: &mut VecDeque<Value>) -> Value {
        self.command(json!(["read"]));
        let mut actual = self.response();
        if expected["type"] == "control_request" {
            let id = actual["request_id"].clone();
            assert!(!id.as_str().unwrap().is_empty());
            pending.push_back(id);
            actual["request_id"] = expected["request_id"].clone();
        }
        let uuid = actual["uuid"].clone();
        if expected["type"] == "user" {
            assert!(!uuid.as_str().unwrap().is_empty());
            assert_eq!(
                uuid.as_str().unwrap().rsplit('-').next(),
                expected["uuid"].as_str().unwrap().rsplit('-').next()
            );
            actual["uuid"] = expected["uuid"].clone();
        }
        assert_eq!(bytes(actual), bytes(expected.clone()), "native write");
        uuid
    }
}

struct Run {
    admission: MessageSubmitResponse,
    stream: EventStream,
    events: Vec<Envelope>,
}

fn replay(path: &Path, definition: &Value, native: &Path, mut expected: Value, historical: bool) {
    let fixture_home = tempfile::tempdir_in("/tmp").unwrap();
    let root = fixture_home.path();
    let peer = root.join("peer");
    fs::write(&peer, include_str!("catalog_replay_peer.py")).unwrap();
    fs::set_permissions(&peer, fs::Permissions::from_mode(0o700)).unwrap();
    let listener = UnixListener::bind(root.join("wire")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let clock = AtomicI64::new(0);
    let ids = AtomicI64::new(0);
    let adapter = Adapter::new(Config {
        executable: Some(peer.display().to_string()),
        model: Some("claude-test".into()),
        working_directory: Some(root.display().to_string()),
        environment: vec![
            format!("PATH={}", std::env::var("PATH").unwrap()),
            format!("HOME={}", root.display()),
            "NODE_ENV=test".into(),
        ],
        journal_capacity: 64,
        tools: ToolPosture::Unrestricted,
        ..Default::default()
    })
    .unwrap()
    .with_runtime(Runtime::new(
        move || clock.fetch_add(1, Ordering::Relaxed) + 1,
        move |kind| format!("{kind}-{}", ids.fetch_add(1, Ordering::Relaxed) + 1),
    ));
    let native_id = definition["identity_map"]["native_session_id"]
        .as_str()
        .unwrap_or_default();
    let reopened = !native_id.is_empty();
    let descriptor = block_on(adapter.probe()).unwrap();
    let request = OpenRequest {
        session_id: "session".into(),
        participant: serde_json::from_value(json!({"id":"user"})).unwrap(),
        reopen: reopened,
        native_session_id: if reopened {
            native_id.into()
        } else {
            String::new()
        },
        ..Default::default()
    };
    let opened = request.clone();
    let mut opening = Some(async_call(move || block_on(adapter.open(request))));
    let deadline = Instant::now() + LIMIT;
    let socket = loop {
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("peer connect: {error}"),
        }
    };
    socket.set_nonblocking(false).unwrap();
    socket.set_read_timeout(Some(LIMIT)).unwrap();
    socket.set_write_timeout(Some(LIMIT)).unwrap();
    let mut session: Option<Arc<dyn Session>> = None;
    let mut peer = Peer(BufReader::new(socket));
    let argv = peer.response();
    let binding = argv.as_array().unwrap().last().unwrap().as_str().unwrap();
    if reopened {
        assert_eq!(binding, native_id);
        assert_eq!(
            argv.as_array().unwrap().iter().rev().nth(1).unwrap(),
            "--resume"
        );
    }
    let mut requests = VecDeque::new();
    let mut submission: Option<mpsc::Receiver<Submission>> = None;
    let mut submitted = None;
    let mut running = None;
    let mut cancellation: Option<mpsc::Receiver<Cancellation>> = None;
    let mut cancelled = false;
    let mut catalog = None;
    let mut delivered: Vec<Envelope> = Vec::new();
    let mut last_run: Vec<Envelope> = Vec::new();
    let mut turns = Vec::new();
    let mut exited = false;
    let wire = fs::read_to_string(native).unwrap();
    for (line, text) in wire.lines().enumerate() {
        let frame: Value = serde_json::from_str(text).unwrap();
        let mut raw = frame["raw"].clone();
        let action = frame["action"].as_str().unwrap();
        let control = raw["op"].as_str().unwrap_or_default();
        if session.is_none() && (action == "submit" || action == "oap-control") {
            session = Some(Arc::from(complete(opening.take().unwrap()).unwrap()));
        }
        if (matches!(action, "wait-run" | "drain") || matches!(control, "resolve" | "cancel"))
            && running.is_none()
        {
            if let Some(submitted) = submission.take() {
                let (admission, stream) = complete(submitted).unwrap();
                running = Some(Run {
                    admission,
                    stream,
                    events: vec![],
                });
            }
        }
        match action {
            "expect-write" => {
                peer.compare_write(&raw, &mut requests);
            }
            "reply" => {
                raw["response"]["request_id"] = requests.pop_front().unwrap();
                peer.command(json!(["write", raw.to_string()]));
                if let Some(cancelled) = cancellation.take() {
                    let receipt = complete(cancelled).unwrap();
                    assert!(receipt.accepted);
                }
            }
            "submit" => {
                assert!(submission.is_none() && running.is_none());
                let session = session.clone().unwrap();
                let request: MessageSubmitRequest = serde_json::from_value(json!({
                    "session_id": "session", "delivery": "auto", "messages": [raw["message"].clone()]
                })).unwrap();
                submitted = Some(request.clone());
                submission = Some(async_call(move || {
                    block_on(session.submit(SubmitRequest {
                        request,
                        envelope_id: "submit".into(),
                    }))
                }));
                let uuid = peer.compare_write(&raw, &mut requests);
                turns.push((raw["uuid"].clone(), uuid));
            }
            "observe" => {
                for key in [
                    "uuid",
                    "user_message_uuid",
                    "user_message_uuids",
                    "command_uuid",
                ] {
                    let rewrite = |id: &mut Value| {
                        if let Some((_, actual)) = turns.iter().find(|(recorded, _)| recorded == id)
                        {
                            *id = actual.clone();
                        }
                    };
                    if let Some(id) = raw.get_mut(key) {
                        if let Some(ids) = id.as_array_mut() {
                            ids.iter_mut().for_each(rewrite);
                        } else {
                            rewrite(id);
                        }
                    }
                }
                peer.command(json!(["write", raw.to_string()]));
            }
            "process-exit" => {
                peer.command(json!(["exit"]));
                exited = true;
            }
            "decode-error" => {
                peer.command(json!(["write", raw.as_str().unwrap()]));
            }
            "drain" => {
                peer.command(json!([
                    "write",
                    json!({
                        "type": "control_request",
                        "request_id": "replay-barrier",
                        "request": {"subtype": "barrier"}
                    })
                    .to_string()
                ]));
                peer.command(json!(["read"]));
                assert_eq!(peer.response()["response"]["request_id"], "replay-barrier");
            }
            "wait-run" => {
                let mut run = running.take().unwrap();
                run.events.extend(drain(&mut run.stream, LIMIT));
                last_run = run.events;
                delivered.extend(last_run.iter().cloned());
                let mut trace = RunTrace::new(run.admission, descriptor.clone(), last_run.clone());
                trace.opened = Some(opened.clone());
                trace.submitted = submitted.take();
                trace.catalog = catalog.clone();
                trace.cancelled = std::mem::take(&mut cancelled);
                trace.assert_valid();
            }
            "oap-control" => {
                let session = session.as_ref().unwrap();
                match control {
                    "resolve" => {
                        let run = running.as_mut().unwrap();
                        let request = loop {
                            let event = next(&mut run.stream, LIMIT);
                            let request = (event.envelope_type.as_str() == "user.input.requested")
                                .then(|| {
                                    serde_json::from_str::<UserInputRequestedPayload>(
                                        event.payload.as_ref().unwrap().get(),
                                    )
                                    .unwrap()
                                });
                            run.events.push(event);
                            if let Some(request) = request {
                                break request;
                            }
                        };
                        block_on(session.resolve(InteractionResolution {
                            input: Some(UserInputResolveRequest {
                                session_id: "session".into(),
                                interaction_id: request.interaction_id,
                                responded_by: request.responded_by,
                                answers: vec![InputAnswer {
                                    question_id: "decision".into(),
                                    selected_option_ids: vec![
                                        raw["decision"].as_str().unwrap().into(),
                                    ],
                                    ..Default::default()
                                }],
                                ..Default::default()
                            }),
                            ..Default::default()
                        }))
                        .unwrap();
                    }
                    "cancel" => {
                        let session = session.clone();
                        let id = running.as_ref().unwrap().admission.run_id.clone().unwrap();
                        cancelled = true;
                        cancellation = Some(async_call(move || block_on(session.cancel(id))));
                        peer.compare_write(&json!({"type":"control_request","request_id":"@request","request":{"subtype":"interrupt"}}), &mut requests);
                    }
                    "assert-catalog" => {
                        let tools = session.tool_lister().unwrap();
                        let mut request: ToolsListRequest =
                            serde_json::from_value(json!({"session_id":"session"})).unwrap();
                        assert!(matches!(
                            block_on(tools.tools(request.clone())),
                            Err(Error::DegradedControl(_))
                        ));
                        request
                            .allow_degraded_features
                            .push(FEATURE_TOOLS_LIST.into());
                        let got = block_on(tools.tools(request.clone())).unwrap();
                        assert_eq!(
                            bytes(serde_json::to_value(&got.tools).unwrap()),
                            bytes(raw["catalog"].clone())
                        );
                        if got.tools.session_id.is_some() {
                            catalog = Some((request, got));
                        }
                    }
                    "assert-state" => assert_eq!(
                        block_on(session.state()).unwrap().status.as_str(),
                        raw["status"]
                    ),
                    "resume" if raw["expect"] == "run-not-found" => {
                        let request = ResumeRequest {
                            run_id: raw["run_id"].as_str().unwrap().into(),
                            ..Default::default()
                        };
                        assert!(matches!(
                            block_on(session.resume(request)),
                            Err(Error::RunNotFound)
                        ));
                    }
                    "resume" => {
                        let request = ResumeRequest {
                            run_id: last_run[0].run_id.clone().unwrap(),
                            ..Default::default()
                        };
                        let (recovery, mut stream) = block_on(session.resume(request)).unwrap();
                        assert!(recovery.replay_gap.is_none());
                        assert_eq!(
                            serde_json::to_vec(&drain(&mut stream, LIMIT)).unwrap(),
                            serde_json::to_vec(&last_run).unwrap()
                        );
                    }
                    "close" => {}
                    _ => panic!("unexpected control {control}"),
                }
            }
            _ => panic!("unexpected action {action} on line {}", line + 1),
        }
    }
    assert!(
        submission.is_none() && running.is_none() && cancellation.is_none() && requests.is_empty()
    );
    let session =
        session.get_or_insert_with(|| Arc::from(complete(opening.take().unwrap()).unwrap()));
    if !exited {
        peer.command(json!(["end"]));
    }
    let state = if reopened {
        Some(block_on(session.state()).unwrap())
    } else {
        None
    };
    block_on(session.close()).unwrap();
    if !exited {
        assert_eq!(peer.response(), "eof", "extra native output");
    }
    if let Some(state) = state {
        assert_eq!(session.native_session_id().as_deref(), Some(native_id));
        assert_eq!(state.status.as_str(), "idle");
        assert!(state.active_runs.is_empty());
        let state = serde_json::to_value(state).unwrap();
        assert_eq!(
            bytes(json!({
                "current_model_id": state["current_model_id"],
                "recovery": state["recovery"],
                "compaction_policy": state["compaction_policy"]
            })),
            bytes(expected)
        );
    } else {
        if historical {
            for event in expected.as_array_mut().unwrap() {
                event["capability_revision"] = json!(pin().capability_revision);
            }
        }
        let actual = serde_json::to_value(delivered).unwrap();
        assert_eq!(
            actual.as_array().unwrap().len(),
            expected.as_array().unwrap().len()
        );
        for (index, (actual, expected)) in actual
            .as_array()
            .unwrap()
            .iter()
            .zip(expected.as_array().unwrap())
            .enumerate()
        {
            assert_eq!(
                bytes(json!([actual])),
                bytes(json!([expected])),
                "{} event {}",
                path.display(),
                index + 1
            );
        }
    }
}

#[test]
fn catalog_cases_match_the_shared_trace() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let catalog = oxap_harness::Catalog::embedded()
        .harness("claude-code")
        .unwrap();
    let mut replayed_versions = HashSet::new();
    let mut total = 0;
    let mut failed = Vec::new();
    for version in &catalog.versions {
        let label = version.label.as_str();
        let dir = if version.corpus.is_empty() {
            root.join("fixtures/adapters")
                .join(format!("claude-code-{label}"))
        } else {
            root.join(&version.corpus)
        };
        if version.corpus.is_empty() && label != pin().corpus_from && !dir.exists() {
            continue;
        }
        assert!(replayed_versions.insert(label));
        let manifest = read(&dir.join("manifest.json"));
        assert_eq!(manifest["tag"], label);
        let cases = manifest["cases"].as_array().unwrap();
        assert!(!cases.is_empty());
        let mut ids = HashSet::new();
        for case in cases {
            let path = dir.join(case["path"].as_str().unwrap());
            let definition = read(&path.join("case.json"));
            assert_eq!(definition["id"], case["id"]);
            let native = path.join(definition["native"].as_str().unwrap());
            let expected = read(&path.join(definition["expected_oap"].as_str().unwrap()));
            let id = case["id"].as_str().unwrap();
            assert!(ids.insert(id), "duplicate case {label}/{id}");
            let historical = label != pin().label;
            println!("REPLAY {label}/{id}");
            if std::panic::catch_unwind(|| {
                replay(&path, &definition, &native, expected, historical)
            })
            .is_ok()
            {
                println!("PASS {label}/{id}");
            } else {
                failed.push(format!("{label}/{id}"));
            }
            total += 1;
        }
    }
    assert!(replayed_versions.contains(pin().label.as_str()));
    assert!(
        replayed_versions.len() >= 3,
        "current corpus and two retained corpora are required"
    );
    println!(
        "{}/{} Claude corpus cases passed",
        total - failed.len(),
        total
    );
    assert!(failed.is_empty(), "failed cases: {failed:?}");
}
