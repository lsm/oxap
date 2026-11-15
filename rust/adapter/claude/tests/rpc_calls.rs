use std::io::{BufRead, BufReader, PipeReader, PipeWriter, Write};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};
use oxap_adapter_claude::rpc::{parse_message, Client, ClientError, ClientOptions, Kind, Message};

const WAIT: Duration = Duration::from_secs(5);
const INITIALIZE: &str = r#"{"subtype":"initialize","hooks":null}"#;
const INTERRUPT: &str = r#"{"subtype":"interrupt"}"#;

type Call = JoinHandle<Result<Option<Vec<u8>>, ClientError>>;

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
}

fn success(id: &str, response: &str) -> String {
    format!(
        r#"{{"type":"control_response","response":{{"subtype":"success","request_id":"{id}","response":{response}}}}}"#
    )
}

fn call(client: &Client, request: &str, deadline: Option<Instant>) -> Call {
    let client = client.clone();
    let request = request.to_owned();
    std::thread::spawn(move || client.call(request.as_bytes(), deadline))
}

fn settled(handle: Call) -> Result<Option<Vec<u8>>, ClientError> {
    let (sent, received) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = sent.send(handle.join().unwrap());
    });
    received.recv_timeout(WAIT).expect("the call never settled")
}

fn ended(signal: Receiver<()>) -> bool {
    matches!(
        signal.recv_timeout(WAIT),
        Err(RecvTimeoutError::Disconnected)
    )
}

#[test]
fn a_control_call_writes_its_request_and_settles_with_the_response_bytes() {
    let mut peer = peer();
    let pending = call(&peer.client, INITIALIZE, None);
    let request = peer.read();
    assert_eq!(request.kind, Kind::ControlRequest);
    let (count, suffix) = request
        .request_id
        .strip_prefix("req_")
        .and_then(|id| id.split_once('_'))
        .expect("a minted id is req_<count>_<hex>");
    assert_eq!(count, "1");
    assert_eq!(suffix.len(), 8);
    assert!(suffix.bytes().all(|byte| byte.is_ascii_hexdigit()));
    peer.send(&success(&request.request_id, r#"{"pid":7}"#));
    assert_eq!(settled(pending).unwrap(), Some(br#"{"pid":7}"#.to_vec()));
    let _next = call(&peer.client, INTERRUPT, None);
    assert!(peer.read().request_id.starts_with("req_2_"));
}

#[test]
fn a_control_response_no_call_awaits_is_ignored() {
    let mut peer = peer();
    peer.send(&success("never-issued", "{}"));
    let pending = call(&peer.client, INTERRUPT, None);
    let request = peer.read();
    peer.send(&success(&request.request_id, "{}"));
    assert_eq!(settled(pending).unwrap(), Some(b"{}".to_vec()));
    assert_eq!(peer.client.err(), None);
}

#[test]
fn concurrent_calls_each_receive_their_own_response() {
    let mut peer = peer();
    let calls: Vec<_> = (1..=4)
        .map(|tag| {
            call(
                &peer.client,
                &format!(r#"{{"subtype":"interrupt","n":{tag}}}"#),
                None,
            )
        })
        .collect();
    let mut requests: Vec<_> = (0..4).map(|_| peer.read()).collect();
    requests.reverse();
    for request in requests {
        let sent: serde_json::Value = serde_json::from_slice(&request.raw).unwrap();
        let tag = sent["request"]["n"].as_i64().unwrap();
        peer.send(&success(&request.request_id, &format!(r#"{{"n":{tag}}}"#)));
    }
    for (index, pending) in calls.into_iter().enumerate() {
        assert_eq!(
            settled(pending).unwrap(),
            Some(format!(r#"{{"n":{}}}"#, index + 1).into_bytes())
        );
    }
}

#[test]
fn an_error_response_settles_the_call_with_the_remote_detail() {
    let mut peer = peer();
    let pending = call(&peer.client, INTERRUPT, None);
    let request = peer.read();
    peer.send(&format!(
        r#"{{"type":"control_response","response":{{"subtype":"error","request_id":"{}","error":"no run"}}}}"#,
        request.request_id
    ));
    let error = settled(pending).unwrap_err();
    assert_eq!(
        error,
        ClientError::Remote {
            id: request.request_id.clone(),
            detail: "no run".into()
        }
    );
    assert_eq!(
        error.to_string(),
        format!(
            "claude rpc error for request {}: no run",
            request.request_id
        )
    );
    assert_eq!(peer.client.err(), None);
}

#[test]
fn a_call_past_its_deadline_retires_the_client() {
    let peer = peer();
    let pending = call(
        &peer.client,
        INTERRUPT,
        Some(Instant::now() + Duration::from_millis(100)),
    );
    assert_eq!(settled(pending), Err(ClientError::DeadlineExceeded));
    assert!(ended(peer.client.done()));
    assert_eq!(peer.client.err(), Some(ClientError::DeadlineExceeded));
}

#[test]
fn a_deadline_on_one_call_fails_every_other_pending_call_without_a_closer() {
    let peer = peer();
    let expiring = call(
        &peer.client,
        INITIALIZE,
        Some(Instant::now() + Duration::from_millis(200)),
    );
    let other = call(&peer.client, INTERRUPT, None);
    assert_eq!(settled(expiring), Err(ClientError::DeadlineExceeded));
    assert_eq!(settled(other), Err(ClientError::DeadlineExceeded));
}

#[test]
fn a_peer_that_hangs_up_fails_a_pending_call_with_eof() {
    let mut peer = peer();
    let pending = call(&peer.client, INTERRUPT, None);
    peer.read();
    let Peer {
        client, to_client, ..
    } = peer;
    drop(to_client);
    assert_eq!(settled(pending), Err(ClientError::Transport("EOF".into())));
    assert!(ended(client.read_done()));
}

#[test]
fn a_call_after_close_is_refused_as_closed() {
    let peer = peer();
    peer.client.close();
    assert_eq!(
        peer.client.call(INTERRUPT.as_bytes(), None),
        Err(ClientError::Closed)
    );
}

#[test]
fn a_response_followed_by_a_malformed_frame_still_settles_its_call() {
    for run in 0..50 {
        let (from_server, mut to_client) = std::io::pipe().unwrap();
        let (from_client, to_server) = std::io::pipe().unwrap();
        let client = Client::new(from_server, to_server, ClientOptions::default());
        std::thread::spawn(move || {
            let Ok(request) = frames(from_client).recv_timeout(WAIT) else {
                return;
            };
            let lines = format!("{}\n[]\n", success(&request.request_id, "{}"));
            let _ = to_client.write_all(lines.as_bytes());
        });
        assert_eq!(
            client.call(INITIALIZE.as_bytes(), None),
            Ok(Some(b"{}".to_vec())),
            "run {run}: the response lost to the malformed frame after it"
        );
        assert!(ended(client.read_done()), "run {run}");
        assert!(client.err().is_some(), "run {run}");
    }
}
