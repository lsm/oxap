#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use oxap_adapter::testkit::drain;
use oxap_adapter::{Error, OpenRequest};
use oxap_adapter_copilot::adapter::{Adapter, Config};
use oxap_adapter_copilot::PINNED_VERSION;
use oxap_protocol::{Message, MessageContent, MessageRole, MessageSubmitRequest, Participant};

const WAIT: Duration = Duration::from_secs(5);
const CHILD: &str = r#"
import json, os, sys
source, sink = sys.stdin.buffer, sys.stdout.buffer
mode = os.environ.get("MODE", "")
def read():
    length = None
    while True:
        line = source.readline()
        if not line:
            sys.exit(0)
        if line == b"\r\n":
            break
        name, _, value = line.partition(b":")
        if name.strip().lower() == b"content-length":
            length = int(value.strip())
    return json.loads(source.read(length))
def write(body):
    data = json.dumps(body).encode()
    sink.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
    sink.flush()
def reply(message, result):
    write({"jsonrpc": "2.0", "id": message["id"], "result": result})
def event(kind, data):
    write({"jsonrpc": "2.0", "method": "session.event", "params": {"sessionId": "native-1", "event": {"type": kind, "data": data}}})
while True:
    message = read()
    method = message["method"]
    if method == "ping":
        reply(message, None if mode == "null" else {"protocolVersion": 2 if mode == "old-protocol" else 3})
    elif method == "status.get":
        reply(message, {"version": "0.0.0" if mode == "old-version" else os.environ["VERSION"]})
    elif method == "session.create":
        expected = json.loads(os.environ["CREATE"])
        if message["params"] != expected:
            write({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32602, "message": json.dumps(message["params"], sort_keys=True)}})
        elif mode == "anonymous":
            reply(message, {})
        else:
            reply(message, {"sessionId": "native-1"})
    elif method == "session.send":
        reply(message, {"messageId": "native-message-1"})
        event("user.message", {"messageId": "native-message-1", "interactionId": "i1"})
        event("assistant.message_delta", {"interactionId": "i1", "deltaContent": message["params"]["prompt"]})
        event("session.idle", {})
"#;

fn python() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH is set");
    std::env::split_paths(&path)
        .map(|dir| dir.join("python3"))
        .find(|candidate| candidate.is_file())
        .expect("the synthetic Copilot child needs python3 on PATH")
}

fn directory() -> String {
    std::env::temp_dir().display().to_string()
}

fn adapter(mode: &str, model: &str) -> Adapter {
    let mut create = serde_json::json!({
        "clientName": "open-agent-protocol",
        "workingDirectory": directory(),
        "streaming": true,
        "requestPermission": false,
        "requestUserInput": false,
        "envValueMode": "direct",
    });
    if !model.is_empty() {
        create["model"] = model.into();
    }
    Adapter::new(Config {
        executable: python().display().to_string(),
        args: vec!["-c".into(), CHILD.into()],
        environment: vec![
            format!("MODE={mode}"),
            format!("VERSION={}", *PINNED_VERSION),
            format!("CREATE={create}"),
        ],
        working_directory: directory(),
        model: model.into(),
        request_timeout: WAIT,
        ..Config::default()
    })
    .unwrap()
}

fn request(session_id: &str) -> OpenRequest {
    OpenRequest {
        session_id: session_id.into(),
        participant: Participant {
            id: "user".into(),
            ..Participant::default()
        },
        ..OpenRequest::default()
    }
}

fn refusal(mode: &str, model: &str) -> String {
    match adapter(mode, model).open(request("s1")) {
        Err(Error::Harness(text)) => text,
        Err(other) => panic!("{mode} refused with {other:?}"),
        Ok(_) => panic!("{mode} opened"),
    }
}

#[test]
fn open_creates_a_native_session_that_carries_a_prompt() {
    let session = adapter("", "").open(request("s1")).unwrap();
    assert_eq!(session.native_session_id(), "native-1");
    assert_eq!(session.state().unwrap().session_id.as_str(), "s1");
    let (ack, mut stream) = session
        .submit(MessageSubmitRequest {
            session_id: "s1".into(),
            messages: vec![Message {
                role: MessageRole::USER,
                content: MessageContent::from_text("hello"),
                ..Message::default()
            }],
            ..MessageSubmitRequest::default()
        })
        .unwrap();
    let events = drain(&mut stream, WAIT);
    let kinds: Vec<&str> = events.iter().map(|e| e.envelope_type.as_str()).collect();
    assert_eq!(kinds, ["run.started", "content.delta", "run.completed"]);
    assert_eq!(events[0].run_id, ack.run_id);
    session.close();
}

#[test]
fn open_names_the_session_and_sends_the_configured_model() {
    let session = adapter("", "gpt-5").open(request("")).unwrap();
    assert_eq!(session.state().unwrap().session_id.as_str(), "session-1");
    session.close();
}

#[test]
fn the_contract_opens_the_same_session() {
    let adapter = adapter("", "");
    let opened = oxap_adapter::Adapter::open(&adapter, request("s1"));
    let session = futures::executor::block_on(opened).unwrap();
    assert_eq!(session.native_session_id().as_deref(), Some("native-1"));
    futures::executor::block_on(session.close()).unwrap();
}

#[test]
fn open_refuses_a_server_off_the_pinned_protocol_or_version() {
    for mode in ["old-protocol", "null"] {
        assert_eq!(
            refusal(mode, ""),
            "copilot: server does not speak pinned protocol 3"
        );
    }
    assert_eq!(
        refusal("old-version", ""),
        format!(
            "copilot: server does not report pinned CLI version {}",
            *PINNED_VERSION
        )
    );
    assert_eq!(
        refusal("anonymous", ""),
        "copilot: session.create returned no sessionId"
    );
}

#[test]
fn open_refuses_before_starting_the_server_when_admission_fails() {
    let mut anonymous = request("s1");
    anonymous.participant.id = "".into();
    let missing = Adapter::new(Config {
        executable: "/nonexistent/copilot".into(),
        working_directory: directory(),
        ..Config::default()
    })
    .unwrap();
    assert!(matches!(
        missing.open(anonymous),
        Err(Error::InvalidParticipant)
    ));
    assert!(matches!(
        missing.open(request("s1")),
        Err(Error::Harness(_))
    ));
}
