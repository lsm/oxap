use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::RecvTimeoutError;

use super::{Client, ClientOptions};

const WAIT: Duration = Duration::from_secs(5);
const ASK: &str = r#"{"type":"control_request","request_id":"ask-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"t1"}}"#;

#[test]
fn a_reverse_request_left_in_the_queue_does_not_keep_the_client_alive() {
    let (from_server, mut to_client) = std::io::pipe().unwrap();
    let (_from_client, to_server) = std::io::pipe().unwrap();
    let client = Client::new(from_server, to_server, ClientOptions::default());
    to_client.write_all(format!("{ASK}\n").as_bytes()).unwrap();
    drop(to_client);
    assert_eq!(
        client.read_done().recv_timeout(WAIT),
        Err(RecvTimeoutError::Disconnected)
    );
    let inner = Arc::downgrade(&client.0);
    drop(client);
    let deadline = Instant::now() + WAIT;
    while inner.strong_count() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        inner.strong_count(),
        0,
        "the reverse request still in the queue kept the client alive after its reader and writer ended"
    );
}
