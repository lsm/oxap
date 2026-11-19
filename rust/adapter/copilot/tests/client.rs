#![cfg(unix)]

use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use crossbeam_channel::RecvTimeoutError;
use oxap_adapter_copilot::client::{Client, Error};
use oxap_adapter_copilot::native::{parse, Kind};
use oxap_adapter_copilot::rpc::{Decoder, Encoder};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(5);

struct Wire {
    decoder: Decoder<UnixStream>,
    encoder: Encoder<UnixStream>,
    stream: UnixStream,
}

impl Wire {
    fn read(&mut self) -> Value {
        let body = self.decoder.read().expect("the client wrote a frame");
        serde_json::from_slice(&body).unwrap()
    }

    fn write(&mut self, body: &str) {
        self.encoder.write(body.as_bytes()).unwrap();
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn peer(capacity: usize) -> (Client, Wire) {
    let (near, far) = UnixStream::pair().unwrap();
    let shut = near.try_clone().unwrap();
    let client = Client::new(
        near.try_clone().unwrap(),
        near,
        Box::new(move || {
            let _ = shut.shutdown(Shutdown::Both);
        }),
        0,
        capacity,
    );
    let wire = Wire {
        decoder: Decoder::new(far.try_clone().unwrap(), 0),
        encoder: Encoder::new(far.try_clone().unwrap(), 0),
        stream: far,
    };
    (client, wire)
}

fn soon() -> Option<Instant> {
    Some(Instant::now() + WAIT)
}

fn reply(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

#[test]
fn a_call_keeps_notifications_and_refuses_reverse_requests() {
    let (client, mut wire) = peer(2);
    let server = std::thread::spawn(move || {
        let call = wire.read();
        assert_eq!(call["jsonrpc"], "2.0");
        assert_eq!(call["method"], "session.send");
        assert_eq!(call["params"], json!({"prompt": "synthetic"}));
        wire.write(r#"{"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"ses","event":{"type":"session.idle","data":{}}}}"#);
        wire.write(
            r#"{"jsonrpc":"2.0","id":"reverse-1","method":"gitHubToken.getToken","params":{}}"#,
        );
        let body = wire.decoder.read().unwrap();
        let refused = parse(&body).unwrap();
        assert_eq!(refused.kind, Kind::Failure);
        assert_eq!(refused.code, -32601);
        assert_eq!(refused.id.unwrap().get(), r#""reverse-1""#);
        wire.write(&reply(&call["id"], json!({"messageId": "m1"})));
        wire
    });
    let result = client
        .call("session.send", &json!({"prompt": "synthetic"}), soon())
        .unwrap()
        .unwrap();
    assert_eq!(result.get(), r#"{"messageId":"m1"}"#);
    let note = client.notifications().recv_timeout(WAIT).unwrap();
    assert_eq!(note.method, "session.event");
    let _wire = server.join().unwrap();
    client.close();
}

#[test]
fn concurrent_calls_match_their_own_replies() {
    let (client, mut wire) = peer(2);
    let server = std::thread::spawn(move || {
        let calls = [wire.read(), wire.read()];
        for call in calls.iter().rev() {
            wire.write(&reply(&call["id"], call["params"].clone()));
        }
        wire
    });
    let callers: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|name| {
            let client = client.clone();
            std::thread::spawn(move || {
                let got = client.call("synthetic", &name, soon()).unwrap().unwrap();
                assert_eq!(got.get(), format!("{name:?}"));
            })
        })
        .collect();
    for caller in callers {
        caller.join().unwrap();
    }
    let _wire = server.join().unwrap();
    client.close();
}

#[test]
fn a_remote_error_fails_only_its_own_call() {
    let (client, mut wire) = peer(2);
    let server = std::thread::spawn(move || {
        let call = wire.read();
        wire.write(
            &json!({"jsonrpc": "2.0", "id": call["id"], "error": {"code": -32000, "message": "boom"}})
                .to_string(),
        );
        wire
    });
    let failure = client.call("synthetic", &(), soon()).unwrap_err();
    assert_eq!(
        failure,
        Error::Remote {
            code: -32000,
            message: "boom".into()
        }
    );
    assert_eq!(failure.to_string(), "Copilot RPC error -32000: boom");
    assert_eq!(client.err(), None);
    let _wire = server.join().unwrap();
    client.close();
}

#[test]
fn a_deadline_unblocks_a_stalled_write_and_closes_the_client() {
    let (client, _wire) = peer(2);
    let blob = "x".repeat(4 << 20);
    let started = Instant::now();
    let expired = client
        .call(
            "ping",
            &blob,
            Some(Instant::now() + Duration::from_millis(100)),
        )
        .unwrap_err();
    assert_eq!(expired, Error::DeadlineExceeded);
    assert!(started.elapsed() < WAIT);
    assert_eq!(
        client.done().recv_timeout(WAIT),
        Err(RecvTimeoutError::Disconnected)
    );
    assert_eq!(client.err(), Some(Error::DeadlineExceeded));
    client.close();
}

#[test]
fn a_reply_arriving_before_eof_remains_readable() {
    for iteration in 0..40 {
        let (client, mut wire) = peer(2);
        let server = std::thread::spawn(move || {
            let call = wire.read();
            wire.write(&reply(&call["id"], json!("last reply")));
        });
        let got = client.call("synthetic", &(), soon());
        assert_eq!(
            got.map(|raw| raw.map(|raw| raw.get().to_owned())),
            Ok(Some(r#""last reply""#.to_owned())),
            "iteration {iteration}"
        );
        server.join().unwrap();
        client.close();
    }
}

#[test]
fn malformed_input_and_backpressure_close_the_client() {
    let cases: [&[&str]; 3] = [
        &[r#"{"jsonrpc":"1.0","method":"x"}"#],
        &[r#"{"jsonrpc":"2.0","id":999,"result":{}}"#],
        &[
            r#"{"jsonrpc":"2.0","method":"one"}"#,
            r#"{"jsonrpc":"2.0","method":"two"}"#,
            r#"{"jsonrpc":"2.0","method":"three"}"#,
        ],
    ];
    for (bodies, expected) in cases
        .into_iter()
        .zip([Error::Wire, Error::Wire, Error::Backpressure])
    {
        let (client, mut wire) = peer(2);
        for body in bodies {
            wire.write(body);
        }
        assert_eq!(
            client.done().recv_timeout(WAIT),
            Err(RecvTimeoutError::Disconnected),
            "{bodies:?}"
        );
        assert_eq!(client.err(), Some(expected), "{bodies:?}");
        client.close();
    }
}

#[test]
fn close_ends_both_threads_and_fails_a_later_call() {
    let (client, _wire) = peer(2);
    client.close();
    assert_eq!(client.err(), Some(Error::Closed));
    assert_eq!(client.call("late", &(), soon()).unwrap_err(), Error::Closed);
    client.close();
}
