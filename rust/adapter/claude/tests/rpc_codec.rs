use std::io::Cursor;

use oxap_adapter_claude::rpc::{
    control_response_message, user_turn_message, Decoder, Encoder, DEFAULT_FRAME_LIMIT,
};

fn refused(input: &[u8], limit: usize) -> String {
    Decoder::new(Cursor::new(input.to_vec()), limit)
        .decode()
        .unwrap_err()
        .to_string()
}

#[test]
fn a_frame_is_one_lf_terminated_line() {
    let mut decoder = Decoder::new(
        Cursor::new(b"{\"type\":\"user\"}\n{\"type\":\"keep_alive\"}\n".to_vec()),
        0,
    );
    assert_eq!(decoder.decode().unwrap().message_type, "user");
    assert_eq!(decoder.decode().unwrap().message_type, "keep_alive");
    assert_eq!(decoder.decode().unwrap_err().to_string(), "EOF");
}

#[test]
fn a_carriage_return_an_empty_line_an_unterminated_tail_or_bad_utf8_is_refused() {
    for (raw, detail) in [
        (
            &b"{\"type\":\"user\"}\r\n"[..],
            "claude rpc: invalid stream-json message: carriage return is not valid framing",
        ),
        (
            b"\n{\"type\":\"user\"}\n",
            "claude rpc: invalid stream-json message: empty frame",
        ),
        (
            b"{\"type\":\"user\"",
            "claude rpc: invalid stream-json message: unterminated frame",
        ),
        (
            b"{\"type\":\"user\",\"bad\":\"\xff\xfe\"}\n",
            "claude rpc: invalid stream-json message: frame is not UTF-8",
        ),
    ] {
        assert_eq!(refused(raw, 0), detail);
    }
}

#[test]
fn the_frame_limit_counts_the_bytes_before_the_newline() {
    let frame = b"{\"type\":\"user\",\"pad\":\"xxxxxxxxxx\"}";
    let mut allowed = frame.to_vec();
    allowed.push(b'\n');
    assert!(Decoder::new(Cursor::new(allowed), frame.len())
        .decode()
        .is_ok());
    assert_eq!(
        refused(&frame_with_newline(frame), frame.len() - 1),
        "claude rpc: frame exceeds configured limit"
    );
}

fn frame_with_newline(frame: &[u8]) -> Vec<u8> {
    let mut out = frame.to_vec();
    out.push(b'\n');
    out
}

#[test]
fn an_oversize_frame_is_refused_before_it_is_parsed() {
    let over = br#"{"type":"user","pad":"xxxxxxxxxxxxxxxxxxxx"}"#;
    assert_eq!(
        refused(&frame_with_newline(over), 10),
        "claude rpc: frame exceeds configured limit"
    );
    assert_eq!(
        refused(b"{bad}\n", 0),
        "claude rpc: invalid stream-json message: invalid JSON"
    );
    assert_eq!(DEFAULT_FRAME_LIMIT, 8 << 20);
}

#[test]
fn an_encoder_writes_one_lf_terminated_frame_or_nothing() {
    let message =
        user_turn_message(br#"{"type":"user","message":{"role":"user","content":"hello"}}"#);
    let mut out = Vec::new();
    Encoder::new(&mut out, 0).encode(&message).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "{\"message\":{\"role\":\"user\",\"content\":\"hello\"},\"type\":\"user\"}\n"
    );

    let mut out = Vec::new();
    assert_eq!(
        Encoder::new(&mut out, 40)
            .encode(&user_turn_message(
                br#"{"type":"user","message":{"role":"user","content":"hello"},"pad":"xxxxxxxxxx"}"#
            ))
            .unwrap_err()
            .to_string(),
        "claude rpc: frame exceeds configured limit"
    );
    assert!(out.is_empty());
}

#[test]
fn an_encoder_limits_the_frame_without_its_newline() {
    let message =
        user_turn_message(br#"{"type":"user","message":{"role":"user","content":"hello"}}"#);
    let frame = "{\"message\":{\"role\":\"user\",\"content\":\"hello\"},\"type\":\"user\"}";
    let mut out = Vec::new();
    Encoder::new(&mut out, frame.len())
        .encode(&message)
        .unwrap();
    assert_eq!(out.len(), frame.len() + 1);

    let mut out = Vec::new();
    assert_eq!(
        Encoder::new(&mut out, frame.len() - 1)
            .encode(&message)
            .unwrap_err()
            .to_string(),
        "claude rpc: frame exceeds configured limit"
    );
    assert!(out.is_empty());
}

#[test]
fn an_encoder_refuses_a_message_it_cannot_write_as_go_writes_it() {
    let mut out = Vec::new();
    assert_eq!(
        Encoder::new(&mut out, 0)
            .encode(&control_response_message(br#"{"subtype":"weird","request_id":"r"}"#))
            .unwrap_err()
            .to_string(),
        "claude rpc: invalid control-plane message: control response subtype must be success or error"
    );
    assert!(out.is_empty());
}
