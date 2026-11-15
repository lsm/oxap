use std::io::{BufRead, BufReader, PipeReader, PipeWriter, Write};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError};
use oxap_adapter_claude::native::Observation;
use oxap_adapter_claude::rpc::{
    parse_message, Client, ClientError, ClientOptions, Inbound, Message,
};

const WAIT: Duration = Duration::from_secs(5);
const INITIALIZE: &str = r#"{"subtype":"initialize","hooks":null}"#;
const LIFECYCLE: &str = r#"{"type":"command_lifecycle","command_uuid":"u1","state":"queued","session_id":"s1","uuid":"c1"}"#;

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

fn peer(options: ClientOptions) -> Peer {
    let (from_server, to_client) = std::io::pipe().unwrap();
    let (from_client, to_server) = std::io::pipe().unwrap();
    Peer {
        client: Client::new(from_server, to_server, options),
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

fn success(id: &str) -> String {
    format!(
        r#"{{"type":"control_response","response":{{"subtype":"success","request_id":"{id}","response":{{}}}}}}"#
    )
}

fn call(client: &Client) -> Call {
    let client = client.clone();
    std::thread::spawn(move || client.call(INITIALIZE.as_bytes(), None))
}

fn settled(handle: Call) -> Result<Option<Vec<u8>>, ClientError> {
    let (sent, received) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = sent.send(handle.join().unwrap());
    });
    received.recv_timeout(WAIT).expect("the call never settled")
}

fn next(client: &Client) -> Inbound {
    client
        .inbound()
        .recv_timeout(WAIT)
        .expect("nothing arrived")
}

fn ended(signal: Receiver<()>) -> bool {
    matches!(
        signal.recv_timeout(WAIT),
        Err(RecvTimeoutError::Disconnected)
    )
}

fn lifecycles(count: usize) -> String {
    (0..count)
        .map(|index| {
            format!(
                "{{\"type\":\"command_lifecycle\",\"command_uuid\":\"u{index}\",\"state\":\"queued\",\"session_id\":\"s1\",\"uuid\":\"c{index}\"}}\n"
            )
        })
        .collect()
}

#[test]
fn an_observation_carries_its_type_bytes_and_decoded_value() {
    let mut peer = peer(ClientOptions::default());
    peer.send(LIFECYCLE);
    let Inbound::Observation(observation) = next(&peer.client) else {
        panic!("expected an observation")
    };
    assert_eq!(observation.message_type, "command_lifecycle");
    assert_eq!(observation.raw, LIFECYCLE.as_bytes());
    match observation.value {
        Observation::CommandLifecycle(frame) => assert_eq!(frame.command_uuid, "u1"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_frame_its_type_refuses_retires_the_client() {
    let mut peer = peer(ClientOptions::default());
    peer.send(r#"{"type":"result","subtype":"success","duration_ms":1,"duration_api_ms":1,"is_error":false,"num_turns":1}"#);
    assert!(ended(peer.client.done()));
    assert!(matches!(peer.client.err(), Some(ClientError::Transport(_))));
}

#[test]
fn a_response_settles_its_call_only_after_the_observations_the_wire_carried_before_it() {
    let mut peer = peer(ClientOptions::default());
    let pending = call(&peer.client);
    let request = peer.read();
    peer.send(LIFECYCLE);
    peer.send(&success(&request.request_id));
    assert!(matches!(next(&peer.client), Inbound::Observation(_)));
    let Inbound::Barrier(ack) = next(&peer.client) else {
        panic!("the response did not wait behind a barrier")
    };
    ack.send(()).unwrap();
    assert_eq!(settled(pending), Ok(Some(b"{}".to_vec())));
}

#[test]
fn a_response_no_call_awaits_queues_no_barrier() {
    let mut peer = peer(ClientOptions::default());
    peer.send(&success("never-issued"));
    peer.send(r#"{"type":"keep_alive"}"#);
    assert!(matches!(next(&peer.client), Inbound::Observation(_)));
}

#[test]
fn a_response_parked_behind_its_barrier_settles_its_call_when_the_client_closes() {
    let mut peer = peer(ClientOptions::default());
    let pending = call(&peer.client);
    let request = peer.read();
    peer.send(&success(&request.request_id));
    let Inbound::Barrier(_ack) = next(&peer.client) else {
        panic!("expected the response barrier first")
    };
    peer.client.close();
    assert_eq!(settled(pending), Ok(Some(b"{}".to_vec())));
}

#[test]
fn a_response_whose_barrier_cannot_be_queued_fails_its_call_with_the_queue_overflow() {
    let mut peer = peer(ClientOptions {
        queue_capacity: 1,
        queue_stall: Some(Duration::from_millis(50)),
        ..ClientOptions::default()
    });
    let pending = call(&peer.client);
    let request = peer.read();
    peer.send(LIFECYCLE);
    peer.send(&success(&request.request_id));
    assert_eq!(settled(pending), Err(ClientError::ObservationQueue));
    assert_eq!(
        ClientError::ObservationQueue.to_string(),
        "claude rpc: observation queue is full"
    );
}

#[test]
fn a_burst_waits_for_a_slow_consumer_within_the_stall_bound() {
    let peer = peer(ClientOptions {
        queue_capacity: 2,
        queue_stall: Some(WAIT),
        ..ClientOptions::default()
    });
    let burst = 40;
    let mut to_client = peer.to_client.try_clone().unwrap();
    std::thread::spawn(move || to_client.write_all(lifecycles(burst).as_bytes()));
    for received in 0..burst {
        assert!(
            matches!(next(&peer.client), Inbound::Observation(_)),
            "frame {received}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(peer.client.err(), None);
}

#[test]
fn a_consumer_stalled_past_the_bound_fails_the_client_with_the_queue_overflow() {
    let mut peer = peer(ClientOptions {
        queue_capacity: 1,
        queue_stall: Some(Duration::from_millis(100)),
        ..ClientOptions::default()
    });
    peer.to_client.write_all(lifecycles(2).as_bytes()).unwrap();
    assert!(ended(peer.client.done()));
    assert_eq!(peer.client.err(), Some(ClientError::ObservationQueue));
}

struct FailsAfterDelivery {
    inner: PipeWriter,
    release: Receiver<()>,
}

impl Write for FailsAfterDelivery {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.inner.write_all(data)?;
        let _ = self.release.recv();
        Err(std::io::Error::other(
            "the write failed after its frame left",
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_call_settles_on_its_response_when_its_write_fails_after_delivery() {
    let (from_server, mut to_client) = std::io::pipe().unwrap();
    let (from_client, to_server) = std::io::pipe().unwrap();
    let (release_write, release) = crossbeam_channel::bounded::<()>(1);
    let client = Client::new(
        from_server,
        FailsAfterDelivery {
            inner: to_server,
            release,
        },
        ClientOptions {
            queue_capacity: 8,
            ..ClientOptions::default()
        },
    );
    let pending = call(&client);
    let request = frames(from_client)
        .recv_timeout(WAIT)
        .expect("the request never left");
    to_client
        .write_all(format!("{}\n", success(&request.request_id)).as_bytes())
        .unwrap();
    let Inbound::Barrier(_ack) = next(&client) else {
        panic!("expected the response barrier first")
    };
    release_write.send(()).unwrap();
    assert_eq!(settled(pending), Ok(Some(b"{}".to_vec())));
}
