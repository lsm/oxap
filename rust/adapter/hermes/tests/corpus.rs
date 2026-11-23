#![cfg(unix)]

use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use futures_executor::block_on;
use oxap_adapter::{
    gojson::go_marshal,
    testkit::{drain, next},
    Adapter as _, Error, EventStream, InteractionResolution, OpenRequest, ResumeRequest, Session,
    SubmitRequest,
};
use oxap_adapter_hermes::{process::ProcessConfig, Adapter};
use oxap_protocol::{
    Envelope, EnvelopeType, InputAnswer, InputQuestionKind, Message, MessageContent, MessageRole,
    MessageSubmitRequest, MessageSubmitResponse, RequestedDeliveryMode, RunId,
    UserInputRequestedPayload, UserInputResolveRequest,
};
use serde::Deserialize;
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(5);
type Submission = Result<(MessageSubmitResponse, EventStream), Error>;
type SharedSession = Arc<dyn Session>;

#[derive(Deserialize)]
struct Manifest {
    cases: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    path: String,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    native: String,
    expected_oap: String,
    advertised_capabilities: BTreeMap<String, String>,
    #[serde(default)]
    journal_capacity: usize,
    #[serde(default)]
    open_error: bool,
    #[serde(default)]
    reopen: bool,
    server_mode: Option<String>,
}

#[derive(Deserialize)]
struct Frame {
    action: String,
    raw: Value,
}

impl Frame {
    fn object(&self) -> Value {
        self.raw.as_str().map_or_else(
            || self.raw.clone(),
            |wire| serde_json::from_str(wire).unwrap(),
        )
    }
}

struct Peer(BufReader<UnixStream>);

impl Peer {
    fn connect(listener: UnixListener) -> Self {
        listener.set_nonblocking(true).unwrap();
        let stream = eventually(|| match listener.accept() {
            Ok((stream, _)) => Some(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(error) => panic!("{error}"),
        });
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream.set_write_timeout(Some(WAIT)).unwrap();
        Self(BufReader::new(stream))
    }

    fn command(&mut self, command: &str, value: impl serde::Serialize) {
        writeln!(
            self.0.get_mut(),
            "{}",
            serde_json::to_string(&(command, value)).unwrap()
        )
        .unwrap();
    }

    fn send(&mut self, frame: &Frame) {
        match frame.raw.as_str() {
            Some(text) => self.command("send", text),
            None => self.command("send", frame.raw.to_string()),
        }
    }

    fn receive(&mut self) -> String {
        let mut line = String::new();
        self.0.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn expect(&mut self, frame: &Frame) {
        let actual = self.receive();
        match frame.raw.as_str() {
            Some(raw) => assert_eq!(actual, raw),
            None => assert_eq!(serde_json::from_str::<Value>(&actual).unwrap(), frame.raw),
        }
    }
}

fn eventually<T>(mut attempt: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(result) = attempt() {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the adapter"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

fn open(adapter: Arc<Adapter>, stored: Option<String>) -> JoinHandle<Result<SharedSession, Error>> {
    thread::spawn(move || {
        block_on(adapter.open(OpenRequest {
            session_id: "session".into(),
            participant: oxap_protocol::Participant {
                id: "user".into(),
                ..Default::default()
            },
            reopen: stored.is_some(),
            native_session_id: stored.unwrap_or_default(),
            ..Default::default()
        }))
        .map(Arc::from)
    })
}

fn submit(session: &dyn Session, text: &str) -> Submission {
    block_on(session.submit(SubmitRequest {
        request: MessageSubmitRequest {
            session_id: "session".into(),
            delivery: RequestedDeliveryMode::AUTO,
            messages: vec![Message {
                role: MessageRole::USER,
                content: MessageContent::from_text(text),
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    }))
}

#[derive(Default)]
struct Replay {
    pending: Option<JoinHandle<Submission>>,
    refusal: bool,
    run: Option<(RunId, EventStream)>,
    events: Vec<Envelope>,
    resumed: Vec<(RunId, u64, EventStream)>,
    control: Option<JoinHandle<()>>,
}

impl Replay {
    fn admit(&mut self) {
        if let Some(pending) = self.pending.take() {
            match pending.join().unwrap() {
                Ok((receipt, stream)) => {
                    assert!(!self.refusal, "refusal admitted a run");
                    assert!(receipt.accepted);
                    assert!(self.run.is_none());
                    self.run = Some((receipt.run_id.unwrap(), stream));
                }
                Err(error) => {
                    assert!(self.refusal, "submit failed: {error:?}");
                    assert!(matches!(error, Error::Harness(_)), "{error:?}");
                }
            }
            self.refusal = false;
        }
    }

    fn finish(&mut self) {
        self.admit();
        if let Some((_, mut stream)) = self.run.take() {
            self.events.extend(drain(&mut stream, WAIT));
        }
    }

    fn join_control(&mut self) {
        if let Some(control) = self.control.take() {
            control.join().unwrap();
        }
    }

    fn control(&mut self, data: &Value, session: &SharedSession) {
        match data["op"].as_str().unwrap() {
            "resolve" => {
                self.admit();
                let (run, stream) = self.run.as_mut().unwrap();
                let request = loop {
                    let event = next(stream, WAIT);
                    let requested = event.envelope_type == EnvelopeType::USER_INPUT_REQUESTED;
                    self.events.push(event);
                    if requested {
                        break serde_json::from_str::<UserInputRequestedPayload>(
                            self.events.last().unwrap().payload.as_ref().unwrap().get(),
                        )
                        .unwrap();
                    }
                };
                let mut inputs = data["answers"].as_array().cloned().unwrap_or_default();
                if inputs.is_empty() {
                    inputs.push(data["answer"].clone());
                }
                assert_eq!(request.questions.len(), inputs.len(), "{data}");
                let answers = request
                    .questions
                    .into_iter()
                    .zip(inputs)
                    .map(|(question, input)| {
                        let value = input.as_str().unwrap().to_owned();
                        let mut answer = InputAnswer {
                            question_id: question.id,
                            ..Default::default()
                        };
                        if question.kind == InputQuestionKind::TEXT {
                            answer.text = value;
                        } else {
                            answer.selected_option_ids = vec![value];
                        }
                        answer
                    })
                    .collect();
                let session = session.clone();
                let run = run.clone();
                self.control = Some(thread::spawn(move || {
                    block_on(session.resolve(InteractionResolution {
                        run_id: run.clone(),
                        responded_by: "user".into(),
                        input: Some(UserInputResolveRequest {
                            interaction_id: request.interaction_id,
                            requested_by: request.requested_by,
                            responded_by: "user".into(),
                            session_id: "session".into(),
                            run_id: run,
                            answers,
                        }),
                        ..Default::default()
                    }))
                    .unwrap();
                }));
            }
            "cancel" => {
                self.admit();
                let run = self.run.as_ref().unwrap().0.clone();
                let session = session.clone();
                self.control = Some(thread::spawn(move || {
                    assert!(block_on(session.cancel(run)).unwrap().accepted);
                }));
            }
            "overlap-submit" => {
                assert!(matches!(
                    submit(session.as_ref(), "hello again"),
                    Err(Error::RunActive)
                ));
            }
            "submit-closed" => {
                self.finish();
                eventually(|| match block_on(session.state()) {
                    Err(Error::SessionClosed) => Some(()),
                    Ok(state) if matches!(state.status.as_str(), "error" | "idle") => {
                        if state.status.as_str() == "error" {
                            Some(())
                        } else {
                            None
                        }
                    }
                    result => panic!("unexpected retired-session state: {result:?}"),
                });
                assert!(matches!(
                    submit(session.as_ref(), "hello again"),
                    Err(Error::SessionClosed)
                ));
            }
            "assert-state" => {
                let status = data["status"].as_str().unwrap();
                if status == "idle" {
                    self.finish();
                }
                assert_eq!(block_on(session.state()).unwrap().status.as_str(), status);
            }
            "resume" => {
                let after = data["after"].as_u64().unwrap_or_default();
                let run = data["run_id"].as_str().map_or_else(
                    || {
                        self.run
                            .as_ref()
                            .map(|(id, _)| id.clone())
                            .or_else(|| self.events.last()?.run_id.clone())
                            .unwrap()
                    },
                    Into::into,
                );
                let result = block_on(session.resume(ResumeRequest {
                    run_id: run.clone(),
                    after_sequence: after,
                }));
                match data["expect"].as_str().unwrap() {
                    "run-not-found" => assert!(matches!(result, Err(Error::RunNotFound))),
                    "gap" => {
                        let Err(Error::ReplayGap(gap)) = result else {
                            panic!("evicted cursor did not return a replay gap");
                        };
                        assert_eq!(gap.requested_after, after);
                        assert!(gap.oldest_available > after + 1);
                    }
                    "replay" => {
                        let (recovery, stream) = result.unwrap();
                        assert!(recovery.replay_gap.is_none());
                        self.resumed.push((run, after, stream));
                    }
                    other => panic!("unexpected resume expectation {other}"),
                }
            }
            "close" => {
                self.finish();
                block_on(session.close()).unwrap();
            }
            other => panic!("unexpected control {other}"),
        }
    }

    fn check_resumed(&mut self) {
        for (run, after, mut stream) in self.resumed.drain(..) {
            let suffix: Vec<_> = self
                .events
                .iter()
                .filter(|event| {
                    event.run_id.as_ref() == Some(&run) && event.sequence.unwrap() > after
                })
                .collect();
            assert!(!suffix.is_empty());
            assert_eq!(serialized(&drain(&mut stream, WAIT)), serialized(&suffix));
        }
    }
}

fn serialized(items: &impl serde::Serialize) -> String {
    go_marshal(&serde_json::to_string(items).unwrap())
}

fn replay(root: &Path, entry: &Entry) -> usize {
    let dir = root.join(&entry.path);
    let case: Case = serde_json::from_slice(&fs::read(dir.join("case.json")).unwrap()).unwrap();
    assert_eq!(case.id, entry.id);
    let expected = fs::read_to_string(dir.join(case.expected_oap)).unwrap();
    let frames: Vec<Frame> = fs::read_to_string(dir.join(case.native))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let workspace = tempfile::Builder::new()
        .prefix("hm-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = workspace.path().join("peer");
    let listener = UnixListener::bind(&socket).unwrap();
    let process = ProcessConfig {
        path: "/usr/bin/env".into(),
        args: vec![
            "python3".into(),
            "-I".into(),
            "-c".into(),
            include_str!("corpus_peer.py").into(),
            socket.display().to_string(),
        ],
        dir: workspace.path().display().to_string(),
        env: Some(vec![
            format!("HOME={}", workspace.path().display()),
            format!("PATH={}", std::env::var("PATH").unwrap()),
            "NODE_ENV=test".into(),
        ]),
        ..Default::default()
    };
    let adapter = Arc::new(
        Adapter::new(process, "hermes-test".into()).with_journal_capacity(
            if case.journal_capacity == 0 {
                64
            } else {
                case.journal_capacity
            },
        ),
    );
    let descriptor = block_on(adapter.probe()).unwrap();
    for (feature, level) in case.advertised_capabilities {
        assert_eq!(
            descriptor
                .capabilities
                .features
                .get(&feature)
                .unwrap_or_else(|| panic!("missing capability {feature}"))
                .level
                .as_str(),
            level,
            "{feature}"
        );
    }
    let stored = case.reopen.then(|| {
        let call: Value = serde_json::from_str(frames[1].raw.as_str().unwrap()).unwrap();
        call["params"]["session_id"].as_str().unwrap().to_owned()
    });
    let opening = open(adapter, stored);
    let mut peer = Peer::connect(listener);
    if case.reopen {
        peer.send(&frames[0]);
        peer.expect(&frames[1]);
        peer.send(&frames[2]);
        let result = opening.join().unwrap();
        let actual = match result {
            Ok(session) => {
                let mut state = block_on(session.state()).unwrap();
                state.updated_at_ms = 0;
                block_on(session.close()).unwrap();
                serialized(&state)
            }
            Err(Error::UnsupportedControl(control)) => {
                serialized(&json!({"feature": control.feature, "reason": control.reason}))
            }
            Err(error) => panic!("reopen failed: {error:?}"),
        };
        assert_eq!(actual, compact(&expected));
        assert!(peer.receive().is_empty());
        return 0;
    }
    let mut frames = frames.iter().peekable();
    if case.server_mode.is_some() {
        peer.send(frames.next().unwrap());
        let create = frames.next().unwrap();
        assert_eq!(create.action, "open");
        if !case.open_error {
            peer.expect(create);
            peer.send(frames.next().unwrap());
        }
    } else {
        let native_id = frames
            .clone()
            .find_map(|frame| {
                (frame.action == "submit").then(|| {
                    let request = frame.object();
                    request["params"]["session_id"].as_str().unwrap().to_owned()
                })
            })
            .unwrap();
        let handshake: Frame = serde_json::from_str(
            fs::read_to_string(root.join("ready-handshake/native.jsonl"))
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        peer.send(&handshake);
        assert_eq!(
            peer.receive(),
            r#"{"id":1,"jsonrpc":"2.0","method":"session.create","params":{"model":"hermes-test"}}"#
        );
        peer.command("send", json!({"jsonrpc":"2.0","id":1,"result":{"session_id":native_id,"stored_session_id":"stored"}}).to_string());
    }
    let result = opening.join().unwrap();
    if case.open_error {
        assert!(
            matches!(result, Err(Error::Harness(ref message)) if message.contains("gateway.ready"))
        );
        assert_eq!(compact(&expected), "null");
        assert!(peer.receive().is_empty());
        return 0;
    }
    let session = result.unwrap();
    let mut replay = Replay::default();
    let mut disconnected = false;
    for frame in frames {
        match frame.action.as_str() {
            "submit" => {
                replay.finish();
                let caller = session.clone();
                replay.pending = Some(thread::spawn(move || submit(caller.as_ref(), "hello")));
                peer.expect(frame);
            }
            "auto" | "observe" | "decode-error" => peer.send(frame),
            "reply" | "reply-error" => {
                if replay.pending.is_some() {
                    let reply = frame.object();
                    replay.refusal = reply["result"]["status"] != "streaming";
                }
                peer.send(frame);
                replay.join_control();
            }
            "expect-write" => {
                peer.expect(frame);
                if frame.object().get("result").is_some() {
                    replay.join_control();
                }
            }
            "wait-submit" => replay.admit(),
            "oap-control" => replay.control(&frame.raw, &session),
            "process-exit" => {
                peer.command("disconnect", ());
                disconnected = true;
            }
            other => panic!("unexpected frame action {other}"),
        }
    }
    replay.finish();
    replay.check_resumed();
    let actual = if expected.trim() == "null" && replay.events.is_empty() {
        "null".to_owned()
    } else {
        serialized(&replay.events)
    };
    let mut expected = compact(&expected);
    if disconnected {
        assert!(expected.contains("\"message\":\"gateway died\""));
        expected = expected.replace("\"message\":\"gateway died\"", "\"message\":\"EOF\"");
    }
    assert_eq!(actual, expected);
    assert!(peer.receive().is_empty());
    replay.events.len()
}

fn compact(json: &str) -> String {
    let raw: Box<serde_json::value::RawValue> = serde_json::from_str(json).unwrap();
    let mut compact = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    for byte in raw.get().bytes() {
        if !quoted && byte.is_ascii_whitespace() {
            continue;
        }
        compact.push(byte);
        if escaped {
            escaped = false;
        } else if quoted && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        }
    }
    String::from_utf8(compact).unwrap()
}

#[test]
fn hermes_corpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let pin = oxap_harness::current("hermes").unwrap();
    let root = root.join(&pin.corpus);
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert!(!manifest.cases.is_empty());
    let mut failed = Vec::new();
    let mut envelopes = 0;
    for entry in &manifest.cases {
        match std::panic::catch_unwind(|| replay(&root, entry)) {
            Ok(count) => {
                println!("PASS {} ({count} envelopes)", entry.id);
                envelopes += count;
            }
            Err(_) => {
                eprintln!("FAIL {}", entry.id);
                failed.push(&entry.id);
            }
        }
    }
    println!(
        "{}/{} cases passed, {envelopes} byte-matched envelopes",
        manifest.cases.len() - failed.len(),
        manifest.cases.len()
    );
    assert!(failed.is_empty(), "failed cases: {failed:?}");
}
