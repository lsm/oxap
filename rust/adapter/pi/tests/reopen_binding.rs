use std::path::Path;

use oxap_adapter::{Error, CONTROL_UNSATISFIABLE};
use oxap_adapter_pi::reopen::{read_binding, SessionBinding};
use oxap_protocol::FEATURE_OPEN_REOPEN;

const HEADER: &str =
    "{\"type\":\"session\",\"id\":\"bound-session\",\"version\":3,\"cwd\":\"/workspace\"}\n";

fn binding_text(file: &Path) -> String {
    serde_json::json!({"sessionId": "bound-session", "sessionFile": file}).to_string()
}

fn read(binding: &str, header: Option<&[u8]>) -> Result<SessionBinding, Error> {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("session.jsonl");
    if let Some(header) = header {
        std::fs::write(&file, header).unwrap();
    }
    read_binding(&binding.replace("{file}", file.to_str().unwrap()))
}

fn bound(header: &[u8]) -> Result<SessionBinding, Error> {
    read(
        r#"{"sessionId":"bound-session","sessionFile":"{file}"}"#,
        Some(header),
    )
}

fn assert_refused(outcome: Result<SessionBinding, Error>) {
    match outcome {
        Err(Error::UnsupportedControl(control)) => {
            assert_eq!(control.feature, FEATURE_OPEN_REOPEN);
            assert_eq!(control.reason, CONTROL_UNSATISFIABLE);
            assert!(control
                .detail
                .starts_with("Pi could not load its binding: "));
        }
        other => panic!("binding must be refused as an unsatisfiable reopen: {other:?}"),
    }
}

fn assert_bound(outcome: Result<SessionBinding, Error>) {
    let binding = outcome.unwrap();
    assert_eq!(binding.session_id, "bound-session");
    assert!(binding.session_file.ends_with("session.jsonl"));
}

#[test]
fn a_binding_to_its_own_session_file_is_read() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("session.jsonl");
    std::fs::write(&file, format!("{HEADER}{{\"type\":\"message\"}}\n")).unwrap();
    let binding = read_binding(&binding_text(&file)).unwrap();
    assert_eq!(
        binding,
        SessionBinding {
            session_id: "bound-session".into(),
            session_file: file.to_str().unwrap().into(),
        }
    );
}

#[test]
fn an_unusable_binding_is_refused_as_go_refuses_it() {
    let directory = tempfile::tempdir().unwrap();
    for binding in [
        "",
        "{",
        "null",
        r#"{"sessionId":"bound-session","sessionFile":"session.jsonl"}"#,
        r#"{"sessionId":"","sessionFile":"{file}"}"#,
        r#"{"sessionId":null,"sessionFile":"{file}"}"#,
        r#"{"sessionId":1,"sessionFile":"{file}"}"#,
        r#"{"sessionId":"bound-session","sessionFile":"{file}","x":1}"#,
        r#"{"sessionId":"bound-session","sessionId":"bound-session","sessionFile":"{file}"}"#,
        r#"{"sessionId":"bound-session","sessionFile":"{file}"} x"#,
    ] {
        assert_refused(read(binding, Some(HEADER.as_bytes())));
    }
    assert_refused(read(
        r#"{"sessionId":"bound-session","sessionFile":"{file}"}"#,
        None,
    ));
    assert_refused(read_binding(&binding_text(directory.path())));
}

#[test]
fn a_session_file_whose_header_is_not_its_session_is_refused() {
    for header in [
        "",
        "\n{\"type\":\"session\",\"id\":\"bound-session\"}\n",
        r#"{"type":"session","id":"someone-else"}"#,
        r#"{"type":"message","id":"bound-session"}"#,
        r#"{"type":"session","id":"bound-session","id":"bound-session"}"#,
        r#"{"type":"session""#,
        r#"{"type":null,"id":"bound-session"}"#,
        r#"{"type":1,"id":"bound-session"}"#,
        r#"{"type":"session","Type":"message","id":"bound-session"}"#,
        "[]",
    ] {
        assert_refused(bound(header.as_bytes()));
    }
}

#[test]
fn keys_match_case_insensitively_and_unknown_header_members_are_ignored() {
    assert_bound(read(
        r#"{"SessionId":"bound-session","SESSIONFILE":"{file}"}"#,
        Some(HEADER.as_bytes()),
    ));
    assert_bound(bound(
        br#"{"TYPE":"session","Id":"bound-session","extra":[1]}"#,
    ));
}

#[test]
fn each_invalid_header_byte_reads_as_one_replacement_character() {
    let header = b"{\"type\":\"session\",\"id\":\"\xe2\x82\"}";
    assert!(read(
        "{\"sessionId\":\"\u{FFFD}\u{FFFD}\",\"sessionFile\":\"{file}\"}",
        Some(header),
    )
    .is_ok());
    assert_refused(read(
        "{\"sessionId\":\"\u{FFFD}\",\"sessionFile\":\"{file}\"}",
        Some(header),
    ));
}

#[test]
fn the_header_is_the_first_line_with_or_without_a_terminator() {
    assert_bound(bound(
        b"{\"type\":\"session\",\"id\":\"bound-session\"}\r\n",
    ));
    assert_bound(bound(b"{\"type\":\"session\",\"id\":\"bound-session\"}"));
    assert_bound(bound(
        b"{\"type\":\"session\",\"id\":\"bound-session\"}\nnot json\n",
    ));
}

#[test]
fn a_header_line_longer_than_sixty_four_kibibytes_is_refused() {
    let line = |length: usize| {
        let base = r#"{"type":"session","id":"bound-session","pad":""}"#;
        format!(
            r#"{{"type":"session","id":"bound-session","pad":"{}"}}"#,
            "a".repeat(length - base.len())
        )
    };
    assert_bound(bound(format!("{}\n", line(64 << 10)).as_bytes()));
    assert_bound(bound(line(64 << 10).as_bytes()));
    assert_refused(bound(format!("{}\n", line((64 << 10) + 1)).as_bytes()));
    assert_refused(bound(line((64 << 10) + 1).as_bytes()));
    assert_refused(bound(format!("{}\r\n", line(64 << 10)).as_bytes()));
    assert_refused(bound(format!("{}\r", line(64 << 10)).as_bytes()));
}
