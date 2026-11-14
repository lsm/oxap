use std::io::{BufRead, BufReader, PipeReader, PipeWriter, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use oxap_adapter_claude::rpc::{parse_message, Client, ClientError, ClientOptions, Kind, Message};

const WAIT: Duration = Duration::from_secs(5);
const TURN: &str = r#"{"message":{"content":"hello","role":"user"},"origin":{"kind":"human"},"parent_tool_use_id":null,"session_id":"default","type":"user","uuid":"u1"}"#;

struct Peer {
    client: Client,
    from_client: BufReader<PipeReader>,
    to_client: PipeWriter,
}

fn peer() -> Peer {
    let (from_server, to_client) = std::io::pipe().unwrap();
    let (from_client, to_server) = std::io::pipe().unwrap();
    Peer {
        client: Client::new(from_server, to_server, ClientOptions::default()),
        from_client: BufReader::new(from_client),
        to_client,
    }
}

fn read_line(reader: &mut impl BufRead) -> Message {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    parse_message(line.trim_end_matches('\n').as_bytes()).unwrap()
}

fn ended(signal: Receiver<()>) -> bool {
    matches!(
        signal.recv_timeout(WAIT),
        Err(RecvTimeoutError::Disconnected)
    )
}

fn write_user(client: &Client, deadline: Option<Instant>) -> JoinHandle<Result<(), ClientError>> {
    let client = client.clone();
    std::thread::spawn(move || client.write_user(TURN.as_bytes(), deadline))
}

fn settled<T: Send + 'static>(handle: JoinHandle<T>) -> T {
    let (sent, received) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = sent.send(handle.join().unwrap());
    });
    received
        .recv_timeout(WAIT)
        .expect("the write never settled")
}

struct Stuck {
    entered: Sender<()>,
    release: Receiver<()>,
}

impl Write for Stuck {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let _ = self.entered.send(());
        let _ = self.release.recv();
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

type Closer = Option<Box<dyn FnOnce() + Send>>;

fn stuck(closer: Closer) -> (Client, PipeWriter, Receiver<()>, Sender<()>) {
    let (from_server, to_client) = std::io::pipe().unwrap();
    let (entered, inside) = crossbeam_channel::bounded(1);
    let (release_writer, release) = crossbeam_channel::bounded::<()>(1);
    let client = Client::new(
        from_server,
        Stuck { entered, release },
        ClientOptions {
            closer,
            ..ClientOptions::default()
        },
    );
    (client, to_client, inside, release_writer)
}

#[test]
fn a_user_turn_goes_out_as_a_user_frame() {
    let mut peer = peer();
    peer.client.write_user(TURN.as_bytes(), None).unwrap();
    let written = read_line(&mut peer.from_client);
    assert_eq!(written.kind, Kind::Observation);
    assert_eq!(written.message_type, "user");
    assert_eq!(written.raw, TURN.as_bytes());
}

#[test]
fn a_write_past_its_deadline_retires_the_client() {
    let (client, _to_client, inside, _release) = stuck(None);
    let pending = write_user(&client, Some(Instant::now() + Duration::from_millis(100)));
    inside.recv_timeout(WAIT).unwrap();
    assert_eq!(settled(pending), Err(ClientError::DeadlineExceeded));
    assert!(ended(client.done()));
    assert_eq!(client.err(), Some(ClientError::DeadlineExceeded));
    assert_eq!(
        ClientError::DeadlineExceeded.to_string(),
        "context deadline exceeded"
    );
}

#[test]
fn a_write_stuck_in_the_writer_returns_when_the_reader_retires_without_a_closer() {
    let (client, mut to_client, inside, _release) = stuck(None);
    let pending = write_user(&client, None);
    inside.recv_timeout(WAIT).unwrap();
    to_client.write_all(b"[]\n").unwrap();
    assert!(settled(pending).is_err());
}

#[test]
fn a_write_stuck_in_the_writer_waits_for_its_own_result_when_a_closer_unblocks_the_writer() {
    let (unblock, unblocked) = crossbeam_channel::bounded::<Sender<()>>(1);
    let (client, mut to_client, inside, release) = stuck(Some(Box::new(move || {
        if let Ok(release) = unblocked.try_recv() {
            let _ = release.send(());
        }
    })));
    unblock.send(release).unwrap();
    let pending = write_user(&client, None);
    inside.recv_timeout(WAIT).unwrap();
    to_client.write_all(b"[]\n").unwrap();
    assert_eq!(settled(pending), Ok(()));
}

#[test]
fn a_deadline_write_stuck_in_the_writer_returns_when_the_closer_fails_to_unblock_it() {
    let (client, mut to_client, inside, _release) = stuck(Some(Box::new(|| {})));
    let deadline = Instant::now() + Duration::from_millis(200);
    let pending = write_user(&client, Some(deadline));
    inside.recv_timeout(WAIT).unwrap();
    to_client.write_all(b"[]\n").unwrap();
    assert_eq!(settled(pending), Err(ClientError::DeadlineExceeded));
    assert!(Instant::now() < deadline + Duration::from_secs(1));
    assert!(ended(client.done()));
}

#[test]
fn a_frame_the_reader_refuses_is_the_client_s_error() {
    let mut peer = peer();
    peer.to_client.write_all(b"[]\n").unwrap();
    assert!(ended(peer.client.read_done()));
    assert_eq!(
        peer.client.err(),
        Some(ClientError::Transport(
            "claude rpc: invalid stream-json message: frame must be exactly one JSON object".into()
        ))
    );
    assert_eq!(
        peer.client.write_user(TURN.as_bytes(), None),
        Err(peer.client.err().unwrap())
    );
}

#[test]
fn a_peer_that_hangs_up_ends_the_client_with_eof() {
    let Peer {
        client, to_client, ..
    } = peer();
    drop(to_client);
    assert!(ended(client.read_done()));
    assert!(ended(client.done()));
    assert_eq!(client.err(), Some(ClientError::Transport("EOF".into())));
}

#[test]
fn a_write_after_close_is_refused_as_closed() {
    let peer = peer();
    peer.client.close();
    assert!(ended(peer.client.done()));
    assert_eq!(
        peer.client.write_user(TURN.as_bytes(), None),
        Err(ClientError::Closed)
    );
    assert_eq!(ClientError::Closed.to_string(), "claude rpc: client closed");
}

#[test]
fn the_closer_runs_exactly_once_however_the_client_ends() {
    let (from_server, mut to_client) = std::io::pipe().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let counted = runs.clone();
    let client = Client::new(
        from_server,
        std::io::sink(),
        ClientOptions {
            closer: Some(Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            })),
            ..ClientOptions::default()
        },
    );
    to_client.write_all(b"[]\n").unwrap();
    assert!(ended(client.read_done()));
    client.close();
    client.close();
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}
