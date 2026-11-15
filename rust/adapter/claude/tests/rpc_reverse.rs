use std::io::{BufRead, BufReader, PipeReader, PipeWriter, Write};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError};
use oxap_adapter_claude::native::{ControlRequest, CONTROL_CAN_USE_TOOL};
use oxap_adapter_claude::rpc::{
    parse_message, Client, ClientError, ClientOptions, Inbound, IncomingControl, Kind, Message,
};
use serde_json::value::RawValue;

const WAIT: Duration = Duration::from_secs(5);
const ASK: &str = r#"{"type":"control_request","request_id":"ask-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"t1"}}"#;

struct Peer {
    client: Client,
    frames: Receiver<Message>,
    to_client: PipeWriter,
}

fn frames(from_client: PipeReader) -> Receiver<Message> {
    let (sent, frames) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(from_client);
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            let Ok(message) = parse_message(line.trim_end_matches('\n').as_bytes()) else {
                return;
            };
            if sent.send(message).is_err() {
                return;
            }
            line.clear();
        }
    });
    frames
}

fn peer() -> Peer {
    let (from_server, to_client) = std::io::pipe().unwrap();
    let (from_client, to_server) = std::io::pipe().unwrap();
    Peer {
        client: Client::new(from_server, to_server, ClientOptions::default()),
        frames: frames(from_client),
        to_client,
    }
}

impl Peer {
    fn read(&mut self) -> Message {
        self.frames
            .recv_timeout(WAIT)
            .expect("the client never wrote a frame")
    }

    fn send(&mut self, line: &str) {
        self.to_client.write_all(line.as_bytes()).unwrap();
        self.to_client.write_all(b"\n").unwrap();
    }

    fn next(&self) -> Inbound {
        self.client
            .inbound()
            .recv_timeout(WAIT)
            .expect("nothing arrived")
    }

    fn control(&self) -> Arc<IncomingControl> {
        match self.next() {
            Inbound::Control(control) => control,
            other => panic!("expected a reverse request, got {other:?}"),
        }
    }
}

fn raw(text: &str) -> Box<RawValue> {
    RawValue::from_string(text.to_owned()).unwrap()
}

fn ended(signal: Receiver<()>) -> bool {
    matches!(
        signal.recv_timeout(WAIT),
        Err(RecvTimeoutError::Disconnected)
    )
}

#[test]
fn a_reverse_request_arrives_decoded_and_takes_exactly_one_answer() {
    let mut peer = peer();
    peer.send(ASK);
    let control = peer.control();
    assert_eq!(control.id, "ask-1");
    assert_eq!(control.subtype, CONTROL_CAN_USE_TOOL);
    assert_eq!(control.raw, ASK.as_bytes());
    match &control.value {
        ControlRequest::CanUseTool(ask) => {
            assert_eq!(ask.tool_name, "Bash");
            assert_eq!(ask.tool_use_id, "t1");
        }
        other => panic!("{other:?}"),
    }
    let answer = raw(r#"{"behavior":"allow","updatedInput":{"command":"ls"}}"#);
    control.respond(Some(&answer), None).unwrap();
    assert_eq!(
        control.respond(Some(&raw(r#"{"behavior":"deny"}"#)), None),
        Err(ClientError::ReverseResolved)
    );
    assert_eq!(
        ClientError::ReverseResolved.to_string(),
        "claude rpc: reverse control request already resolved"
    );
    let written = peer.read();
    assert_eq!(written.kind, Kind::ControlResponse);
    let envelope = written.response.unwrap();
    assert!(envelope.success);
    assert_eq!(envelope.request_id, "ask-1");
    assert_eq!(
        envelope.response.as_deref(),
        Some(&br#"{"behavior":"allow","updatedInput":{"command":"ls"}}"#[..])
    );
}

#[test]
fn an_answer_without_a_body_goes_back_as_an_empty_object() {
    let mut peer = peer();
    peer.send(ASK);
    peer.control().respond(None, None).unwrap();
    assert_eq!(
        peer.read().response.unwrap().response.as_deref(),
        Some(&b"{}"[..])
    );
}

#[test]
fn an_error_answer_goes_back_as_an_error_response() {
    let mut peer = peer();
    peer.send(ASK);
    peer.control().respond_error("denied <here>", None).unwrap();
    let written = peer.read();
    let envelope = written.response.as_ref().unwrap();
    assert!(!envelope.success);
    assert_eq!(envelope.request_id, "ask-1");
    assert_eq!(envelope.error, "denied <here>");
    assert_eq!(
        String::from_utf8(written.raw.clone()).unwrap(),
        r#"{"response":{"error":"denied \u003chere\u003e","request_id":"ask-1","subtype":"error"},"type":"control_response"}"#
    );
}

#[test]
fn a_cancel_retires_its_reverse_request_and_reaches_the_consumer() {
    let mut peer = peer();
    peer.send(ASK);
    let control = peer.control();
    peer.send(r#"{"type":"control_cancel_request","request_id":"ask-1"}"#);
    match peer.next() {
        Inbound::Cancel(notice) => assert_eq!(notice.request_id, "ask-1"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        control.respond(None, None),
        Err(ClientError::ReverseResolved)
    );
}

#[test]
fn an_answered_request_id_may_arrive_again_as_a_new_request() {
    let mut peer = peer();
    peer.send(ASK);
    peer.control().respond(None, None).unwrap();
    peer.read();
    peer.send(ASK);
    peer.control().respond(None, None).unwrap();
    assert_eq!(peer.read().response.unwrap().request_id, "ask-1");
    assert_eq!(peer.client.err(), None);
}

#[test]
fn a_request_id_still_awaiting_its_answer_retires_the_client_when_reused() {
    let mut peer = peer();
    peer.send(ASK);
    let _first = peer.control();
    peer.send(ASK);
    assert!(ended(peer.client.done()));
    assert_eq!(
        peer.client.err(),
        Some(ClientError::DuplicateRequestId("ask-1".into()))
    );
}

#[test]
fn a_reverse_request_its_subtype_refuses_retires_the_client() {
    let mut peer = peer();
    peer.send(r#"{"type":"control_request","request_id":"ask-1","request":{"subtype":"can_use_tool","input":{},"tool_use_id":"t1"}}"#);
    assert!(ended(peer.client.done()));
    assert_eq!(
        peer.client.err(),
        Some(ClientError::Transport(
            "claude native: invalid frame for a known type: can_use_tool requires tool_name, input, and tool_use_id".into()
        ))
    );
}

#[test]
fn an_unconfigured_reverse_request_arrives_as_unknown() {
    let mut peer = peer();
    peer.send(
        r#"{"type":"control_request","request_id":"e1","request":{"subtype":"elicitation","x":1}}"#,
    );
    match &peer.control().value {
        ControlRequest::Unknown { subtype } => assert_eq!(subtype, "elicitation"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_reverse_request_read_after_close_is_delivered_without_a_table_entry() {
    let mut peer = peer();
    peer.client.close();
    peer.send(ASK);
    let control = peer.control();
    assert_eq!(
        Arc::strong_count(&control),
        1,
        "the closed client kept the request in its table, so nothing will remove it"
    );
    assert_eq!(control.respond(None, None), Err(ClientError::Closed));
}
