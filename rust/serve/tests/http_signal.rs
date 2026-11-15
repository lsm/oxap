use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use oxap_protocol::{
    Envelope, EnvelopeType, Message, MessageContent, MessageRole, MessageSubmitRequest,
    RequestedDeliveryMode, SessionOpenRequest,
};
use serde_json::Value;

struct Daemon {
    child: Child,
    addr: String,
    err: Receiver<String>,
}

impl Daemon {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_oap-rust-hub"))
            .args(["serve", "--addr", "127.0.0.1:0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let addr = line
            .trim()
            .strip_prefix("listening on http://")
            .unwrap_or_else(|| panic!("{line:?}"))
            .to_owned();
        let (logged, err) = std::sync::mpsc::channel();
        let stderr = BufReader::new(child.stderr.take().unwrap());
        std::thread::spawn(move || {
            for line in stderr.lines().map_while(Result::ok) {
                if logged.send(line).is_err() {
                    return;
                }
            }
        });
        Self { child, addr, err }
    }

    fn post(&self, path: &str, body: &str) -> (u16, Value) {
        let mut stream = TcpStream::connect(&self.addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            self.addr,
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).unwrap();
        let status = out[9..12].parse().unwrap();
        let body = out.split_once("\r\n\r\n").map_or("", |(_, body)| body);
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }

    fn logged(&self, wanted: &str) -> String {
        loop {
            let line = self.err.recv_timeout(Duration::from_secs(10)).unwrap();
            if line.contains(wanted) {
                return line;
            }
        }
    }

    fn signal(&self, name: &str) {
        let sent = Command::new("kill")
            .args([format!("-{name}"), self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(sent.success());
    }

    fn exits(&mut self) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the hub went on serving"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn open(daemon: &Daemon, session: &str) -> (u16, Value) {
    let request = Envelope::new(
        EnvelopeType::SESSION_OPEN_REQUEST,
        "req",
        &SessionOpenRequest {
            session_id: Some(session.into()),
            ..SessionOpenRequest::default()
        },
    )
    .unwrap();
    daemon.post(
        "/adapters/memory/sessions",
        &serde_json::to_string(&request).unwrap(),
    )
}

fn running(daemon: &Daemon) -> String {
    assert_eq!(open(daemon, "s").0, 200);
    let mut request = Envelope::new(
        EnvelopeType::SESSION_MESSAGE_SUBMIT_REQUEST,
        "req",
        &MessageSubmitRequest {
            session_id: "s".into(),
            delivery: RequestedDeliveryMode::AUTO,
            messages: vec![Message {
                role: MessageRole::USER,
                content: MessageContent::from_text("run"),
                ..Message::default()
            }],
            ..MessageSubmitRequest::default()
        },
    )
    .unwrap();
    request.session_id = Some("s".into());
    let (status, answer) = daemon.post(
        "/sessions/s/submit",
        &serde_json::to_string(&request).unwrap(),
    );
    assert_eq!(status, 200, "{answer}");
    answer["payload"]["run_id"].as_str().unwrap().to_owned()
}

#[test]
fn a_first_signal_drains_naming_the_live_run_and_a_second_forces_the_http_hub_out() {
    let mut daemon = Daemon::start();
    let run = running(&daemon);
    daemon.signal("INT");
    assert_eq!(
        daemon.logged("shutting down gracefully"),
        format!("oap-rust-hub: shutting down gracefully; still active: s/{run}; 0 queued run(s) lost; signal again to force")
    );
    let (status, refused) = open(&daemon, "t");
    assert_eq!(
        (status, refused["payload"]["error"]["code"].clone()),
        (503, "shutting_down".into())
    );
    daemon.signal("TERM");
    assert_eq!(
        daemon.logged("forced"),
        format!("oap-rust-hub: forced shutdown; cut off: s/{run}")
    );
    assert!(daemon.exits().success());
}

#[test]
fn a_signal_with_nothing_running_stops_the_http_hub_at_once() {
    let mut daemon = Daemon::start();
    assert_eq!(open(&daemon, "s").0, 200);
    daemon.signal("TERM");
    assert_eq!(
        daemon.logged("shutting down"),
        "oap-rust-hub: shutting down gracefully; still active: none; 0 queued run(s) lost; signal again to force"
    );
    assert!(daemon.exits().success());
}
