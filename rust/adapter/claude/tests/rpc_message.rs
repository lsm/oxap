use oxap_adapter_claude::rpc::{
    control_request_message, control_response_message, parse_message, user_turn_message, Fault,
    Kind,
};

fn refused(frame: &str) -> String {
    parse_message(frame.as_bytes()).unwrap_err().to_string()
}

fn written(message: oxap_adapter_claude::rpc::Message) -> String {
    message.marshal_json().unwrap()
}

fn refusal(message: oxap_adapter_claude::rpc::Message) -> String {
    message.marshal_json().unwrap_err().to_string()
}

#[test]
fn a_frame_is_classified_by_the_type_it_carries() {
    let cases = [
        (
            r#"{"type":"user","message":{"role":"user","content":"hi"},"parent_tool_use_id":null,"session_id":"default","uuid":"u1"}"#,
            Kind::Observation,
            "user",
            "",
        ),
        (
            r#"{"type":"assistant","message":{"id":"m1","model":"claude-sonnet-4-5","content":[{"type":"text","text":"hi"}]},"parent_tool_use_id":null,"uuid":"a1","session_id":"s1"}"#,
            Kind::Observation,
            "assistant",
            "",
        ),
        (
            r#"{"type":"system","subtype":"init","session_id":"s1","model":"m","tools":[],"uuid":"i1"}"#,
            Kind::Observation,
            "system",
            "init",
        ),
        (
            r#"{"type":"result","subtype":"success","duration_ms":1,"duration_api_ms":1,"is_error":false,"num_turns":1,"session_id":"s1"}"#,
            Kind::Observation,
            "result",
            "success",
        ),
        (
            r#"{"type":"stream_event","event":{"type":"message_start"},"session_id":"s1","uuid":"e1"}"#,
            Kind::Observation,
            "stream_event",
            "",
        ),
        (
            r#"{"type":"command_lifecycle","command_uuid":"u1","state":"queued","session_id":"s1","uuid":"c1"}"#,
            Kind::Observation,
            "command_lifecycle",
            "",
        ),
        (
            r#"{"type":"keep_alive"}"#,
            Kind::Observation,
            "keep_alive",
            "",
        ),
        (
            r#"{"type":"future_frame","payload":{"anything":true}}"#,
            Kind::Observation,
            "future_frame",
            "",
        ),
        (
            r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{},"tool_use_id":"t1"}}"#,
            Kind::ControlRequest,
            "control_request",
            "can_use_tool",
        ),
        (
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"r1","response":{"still_queued":[]}}}"#,
            Kind::ControlResponse,
            "control_response",
            "",
        ),
        (
            r#"{"type":"control_cancel_request","request_id":"r1"}"#,
            Kind::ControlCancel,
            "control_cancel_request",
            "",
        ),
    ];
    for (frame, kind, name, subtype) in cases {
        let message = parse_message(frame.as_bytes()).unwrap();
        assert_eq!(message.kind, kind, "{frame}");
        assert_eq!(message.message_type, name, "{frame}");
        assert_eq!(message.subtype, subtype, "{frame}");
    }
}

#[test]
fn a_control_response_carries_its_receipt_or_its_error() {
    let message = parse_message(
        br#"{"type":"control_response","response":{"subtype":"success","request_id":"r1","response":{"still_queued":["a","b"]}}}"#,
    )
    .unwrap();
    let envelope = message.response.unwrap();
    assert!(envelope.success);
    assert_eq!(envelope.request_id, "r1");
    assert_eq!(
        envelope.response.as_deref(),
        Some(&br#"{"still_queued":["a","b"]}"#[..])
    );
    assert_eq!(envelope.error, "");

    let message = parse_message(
        br#"{"type":"control_response","response":{"subtype":"error","request_id":"r2","error":"boom"}}"#,
    )
    .unwrap();
    let envelope = message.response.unwrap();
    assert!(!envelope.success);
    assert_eq!(envelope.error, "boom");
    assert_eq!(envelope.response, None);

    let null = parse_message(
        br#"{"type":"control_response","response":{"subtype":"success","request_id":"r3","response": null }}"#,
    )
    .unwrap();
    assert_eq!(null.response.unwrap().response, None);
}

#[test]
fn a_frame_that_is_not_one_object_is_refused() {
    for (frame, detail) in [
        ("", "frame must be exactly one JSON object"),
        ("null", "frame must be exactly one JSON object"),
        (r#"["user"]"#, "frame must be exactly one JSON object"),
        (
            r#" {"type":"user"}"#,
            "frame must be exactly one JSON object",
        ),
        (
            r#"{"type":"user"} "#,
            "frame must be exactly one JSON object",
        ),
        (r#"{"type":"user"} {"type":"user"}"#, "trailing JSON value"),
        (
            r#"{"type":"user","type":"user"}"#,
            r#"duplicate object key "type""#,
        ),
        (
            r#"{"type":"user","m":{"a":1,"a":2}}"#,
            r#"duplicate object key "a""#,
        ),
    ] {
        assert_eq!(
            refused(frame),
            format!("claude rpc: invalid stream-json message: {detail}"),
            "{frame}"
        );
    }
}

#[test]
fn a_required_member_that_is_missing_or_the_wrong_shape_is_refused() {
    for (frame, detail) in [
        (r#"{"message":{}}"#, "type is required"),
        (r#"{"type":""}"#, "type must be a non-empty string"),
        (r#"{"type":42}"#, "type must be a non-empty string"),
        (r#"{"type":null}"#, "type must be a non-empty string"),
        (
            r#"{"type":"system","subtype":""}"#,
            "subtype must be a non-empty string",
        ),
        (
            r#"{"type":"system","subtype":null}"#,
            "subtype must be a non-empty string",
        ),
    ] {
        assert_eq!(
            refused(frame),
            format!("claude rpc: invalid stream-json message: {detail}"),
            "{frame}"
        );
    }
    for (frame, detail) in [
        (
            r#"{"type":"control_request","request":{"subtype":"interrupt"}}"#,
            "control_request requires a non-empty request_id",
        ),
        (
            r#"{"type":"control_request","request_id":5,"request":{"subtype":"x"}}"#,
            "control_request requires a non-empty request_id",
        ),
        (r#"{"type":"control_request","request_id":"r1"}"#, "request is required"),
        (
            r#"{"type":"control_request","request_id":"r1","request":{}}"#,
            "control request requires a non-empty subtype",
        ),
        (
            r#"{"type":"control_request","request_id":"r1","request":"x"}"#,
            "request must be one object: claude rpc: invalid stream-json message: frame must be exactly one JSON object",
        ),
        (r#"{"type":"control_response"}"#, "response is required"),
        (
            r#"{"type":"control_response","response":{"subtype":"weird","request_id":"r1"}}"#,
            r#"control response subtype "weird" is not success or error"#,
        ),
        (
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"r1","error":""}}"#,
            "error control response requires a non-empty error",
        ),
        (
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"r1","error":null}}"#,
            "error control response requires a non-empty error",
        ),
        (
            r#"{"type":"control_response","response":{"subtype":"success"}}"#,
            "control response requires a non-empty request_id",
        ),
        (
            r#"{"type":"control_cancel_request"}"#,
            "control_cancel_request requires a non-empty request_id",
        ),
        (
            r#"{"type":"control_cancel_request","request_id":null}"#,
            "control_cancel_request requires a non-empty request_id",
        ),
    ] {
        assert_eq!(
            refused(frame),
            format!("claude rpc: invalid control-plane message: {detail}"),
            "{frame}"
        );
    }
}

#[test]
fn a_user_turn_is_written_with_its_members_sorted() {
    let frame = br#"{"type":"user","message":{"role":"user","content":"hi"},"parent_tool_use_id":null,"session_id":"default","uuid":"u1","origin":{"kind":"human"}}"#;
    assert_eq!(
        written(user_turn_message(frame)),
        r#"{"message":{"role":"user","content":"hi"},"origin":{"kind":"human"},"parent_tool_use_id":null,"session_id":"default","type":"user","uuid":"u1"}"#
    );
}

#[test]
fn a_control_request_is_written_as_go_marshals_one() {
    assert_eq!(
        written(control_request_message(
            "req_1_ab",
            br#"{"subtype":"interrupt"}"#
        )),
        r#"{"request":{"subtype":"interrupt"},"request_id":"req_1_ab","type":"control_request"}"#
    );
    assert_eq!(
        written(control_request_message(
            "r1",
            br#"{ "subtype" : "x" , "deep" : { "b" : 1 , "a" : [ 1 , { "d" : 2 , "c" : 3 } ] } , "arr" : [ 1 , 2 ] }"#
        )),
        r#"{"request":{"arr":[1,2],"deep":{"b":1,"a":[1,{"d":2,"c":3}]},"subtype":"x"},"request_id":"r1","type":"control_request"}"#
    );
    assert_eq!(
        written(control_request_message(
            "id<&>\u{2028}\u{1}\n\t",
            br#"{"subtype":"x"}"#
        )),
        r#"{"request":{"subtype":"x"},"request_id":"id\u003c\u0026\u003e\u2028\u0001\n\t","type":"control_request"}"#
    );
}

#[test]
fn a_control_response_is_written_as_go_marshals_one() {
    assert_eq!(
        written(control_response_message(
            br#"{"subtype":"success","request_id":"req_1_ab","response":{"behavior":"allow"}}"#
        )),
        r#"{"response":{"request_id":"req_1_ab","response":{"behavior":"allow"},"subtype":"success"},"type":"control_response"}"#
    );
    assert_eq!(
        written(control_response_message(
            br#"{"subtype":"error","request_id":"r","error":"e <&>","extra": { "b" : 2, "a" : [ 1, "x" ] }}"#
        )),
        r#"{"response":{"error":"e \u003c\u0026\u003e","extra":{"b":2,"a":[1,"x"]},"request_id":"r","subtype":"error"},"type":"control_response"}"#
    );
}

#[test]
fn a_raw_member_is_compacted_and_escaped_while_its_own_members_keep_their_order() {
    let frame = br#"{"type":"user","message":{"role":"user","content":"a <b> & \"c\""},"pad":{"b": 2, "a": [1, 2]}}"#;
    assert_eq!(
        written(user_turn_message(frame)),
        r#"{"message":{"role":"user","content":"a \u003cb\u003e \u0026 \"c\""},"pad":{"b":2,"a":[1,2]},"type":"user"}"#
    );
    let numbers =
        br#"{"type":"user","message":{},"n":1.50,"e":1E+5,"z":-0,"q":0.000001,"r":1e21,"x":"\/"}"#;
    assert_eq!(
        written(user_turn_message(numbers)),
        r#"{"e":1E+5,"message":{},"n":1.50,"q":0.000001,"r":1e21,"type":"user","x":"\/","z":-0}"#
    );
    let separators = "{\"type\":\"user\",\"message\":{},\"s\":\"x\u{2028}y\",\"t\":\"x\u{2029}y\"}";
    assert_eq!(
        written(user_turn_message(separators.as_bytes())),
        r#"{"message":{},"s":"x\u2028y","t":"x\u2029y","type":"user"}"#
    );
}

#[test]
fn a_key_is_sorted_and_escaped_as_go_writes_it() {
    let frame = "{\"type\":\"user\",\"message\":{},\"a\\u0008b\":1,\"a\\u000cb\":2,\"c\\nd\":3,\"\\u0001\":4,\"A\":5,\"a\":6,\"\":7,\"\u{e9}\":8,\"z<y\":9,\"\u{2028}\":10,\"s\":\"x\",\"d\u{7f}\":11}";
    assert_eq!(
        written(user_turn_message(frame.as_bytes())),
        "{\"\":7,\"\\u0001\":4,\"A\":5,\"a\":6,\"a\\bb\":1,\"a\\fb\":2,\"c\\nd\":3,\"d\u{7f}\":11,\"message\":{},\"s\":\"x\",\"type\":\"user\",\"z\\u003cy\":9,\"\u{e9}\":8,\"\\u2028\":10}"
    );
}

#[test]
fn a_raw_member_keeps_a_lone_surrogate_escape_while_a_key_reads_the_replacement() {
    let frame = r#"{"type":"user","message":{},"s":"\ud800","k\ud800":1}"#;
    assert_eq!(
        written(user_turn_message(frame.as_bytes())),
        "{\"k\u{fffd}\":1,\"message\":{},\"s\":\"\\ud800\",\"type\":\"user\"}"
    );
    let request = r#"{"subtype":"interrupt","s":"\ud800"}"#;
    assert_eq!(
        written(control_request_message("r", request.as_bytes())),
        r#"{"request":{"s":"\ud800","subtype":"interrupt"},"request_id":"r","type":"control_request"}"#
    );
}

#[test]
fn a_value_that_is_not_written_as_go_writes_it_is_refused() {
    assert_eq!(
        refusal(control_request_message("", br#"{"subtype":"interrupt"}"#)),
        "claude rpc: invalid stream-json message"
    );
    assert_eq!(
        refusal(control_request_message("r", b"")),
        "claude rpc: invalid stream-json message"
    );
    assert_eq!(
        refusal(control_request_message("r", b"{}")),
        "claude rpc: invalid control-plane message: control request requires a subtype"
    );
    assert_eq!(
        refusal(control_request_message("r", br#"[1]"#)),
        "claude rpc: invalid stream-json message: frame must be exactly one JSON object"
    );
    assert_eq!(
        refusal(control_request_message(
            "r",
            br#"{"subtype":"a","subtype":"b"}"#
        )),
        r#"claude rpc: invalid stream-json message: duplicate object key "subtype""#
    );
    assert_eq!(
        refusal(control_response_message(b"")),
        "claude rpc: invalid stream-json message"
    );
    assert_eq!(
        refusal(control_response_message(br#"{"subtype":"success"}"#)),
        "claude rpc: invalid control-plane message: control response requires a non-empty request_id"
    );
    assert_eq!(
        refusal(control_response_message(br#"{"subtype":"weird","request_id":"r"}"#)),
        "claude rpc: invalid control-plane message: control response subtype must be success or error"
    );
    assert_eq!(
        refusal(control_response_message(br#""x""#)),
        "claude rpc: invalid stream-json message: frame must be exactly one JSON object"
    );
    let assistant = parse_message(br#"{"type":"assistant","message":{}}"#).unwrap();
    assert_eq!(
        refusal(assistant),
        "claude rpc: invalid stream-json message: only user frames are written as observations"
    );
    assert_eq!(
        refusal(user_turn_message(br#"{"type":"user"}"#)),
        "claude rpc: invalid stream-json message: user frame requires a message"
    );
}

#[test]
fn a_parsed_observation_is_written_back_as_go_writes_it() {
    let message =
        parse_message(br#"{"type":"user","message":{"role":"user","content":"hi"},"x":{"a":1}}"#)
            .unwrap();
    assert_eq!(
        message.raw.as_slice(),
        br#"{"type":"user","message":{"role":"user","content":"hi"},"x":{"a":1}}"#
    );
    assert_eq!(
        written(message),
        r#"{"message":{"role":"user","content":"hi"},"type":"user","x":{"a":1}}"#
    );
}

#[test]
fn a_parsed_control_frame_is_written_only_when_its_whole_bytes_carry_the_member_go_checks() {
    let request = parse_message(
        br#"{"type":"control_request","request_id":"r","request":{"subtype":"interrupt","b": 2, "a": 1}}"#,
    )
    .unwrap();
    assert_eq!(
        refusal(request),
        "claude rpc: invalid control-plane message: control request requires a subtype"
    );
    let nested = parse_message(
        br#"{"type":"control_request","subtype":"top","request_id":"r","request":{"subtype":"interrupt"}}"#,
    )
    .unwrap();
    assert_eq!(
        written(nested),
        r#"{"request":{"request":{"subtype":"interrupt"},"request_id":"r","subtype":"top","type":"control_request"},"request_id":"r","type":"control_request"}"#
    );
    let response = parse_message(
        br#"{"type":"control_response","response":{"subtype":"success","request_id":"r","response":{"b": 2, "a": 1}}}"#,
    )
    .unwrap();
    assert_eq!(
        refusal(response),
        "claude rpc: invalid control-plane message: control response subtype must be success or error"
    );
}

#[test]
fn a_refusal_names_the_class_the_fault_belongs_to() {
    assert!(matches!(
        parse_message(b"null").unwrap_err().fault,
        Fault::InvalidMessage(_)
    ));
    assert!(matches!(
        parse_message(br#"{"type":"control_response"}"#)
            .unwrap_err()
            .fault,
        Fault::InvalidControl(_)
    ));
    assert!(matches!(
        parse_message(br#"{"type":"system","subtype":""}"#)
            .unwrap_err()
            .fault,
        Fault::InvalidMessage(_)
    ));
}
