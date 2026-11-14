use std::io::Cursor;
use std::path::{Path, PathBuf};

use oxap_adapter_claude::rpc::{
    control_request_message, control_response_message, user_turn_message, Decoder, Encoder, Kind,
};

use serde_json::value::RawValue;
use serde_json::Value;
use std::collections::HashMap;

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/adapters/claude-code-2.1.322")
}

fn traces() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![corpus()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn frame_bytes(record: &Value) -> Option<Vec<u8>> {
    let frame = record.get("raw").unwrap_or(record);
    frame.is_object().then(|| frame.to_string().into_bytes())
}

#[test]
fn every_corpus_frame_decodes() {
    let traces = traces();
    assert!(traces.len() >= 10, "{} traces", traces.len());
    let mut frames = 0;
    for trace in traces {
        let text = std::fs::read_to_string(&trace).unwrap();
        for line in text.lines() {
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(bytes) = frame_bytes(&record) else {
                continue;
            };
            frames += 1;
            Decoder::new(Cursor::new([bytes.as_slice(), b"\n"].concat()), 0)
                .decode()
                .unwrap_or_else(|error| panic!("{}: {line}: {error}", trace.display()));
        }
    }
    assert!(frames >= 250, "{frames} frames");
}

#[test]
fn every_host_write_is_written_as_go_writes_it() {
    let fixture = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/go-rpc-writes.jsonl"),
    )
    .unwrap();
    let mut writes = 0;
    for line in fixture.lines() {
        let record: Value = serde_json::from_str(line).unwrap();
        let frame = record["frame"].as_str().unwrap().as_bytes();
        let want = record["written"].as_str().unwrap();
        let message = Decoder::new(Cursor::new([frame, b"\n"].concat()), 0)
            .decode()
            .unwrap_or_else(|error| panic!("{line}: {error}"));
        let object: HashMap<String, Box<RawValue>> = serde_json::from_slice(frame).unwrap();
        let write = match message.kind {
            Kind::ControlRequest => control_request_message(
                message.request_id.as_str(),
                object["request"].get().as_bytes(),
            ),
            Kind::ControlResponse => control_response_message(object["response"].get().as_bytes()),
            Kind::Observation => user_turn_message(frame),
            Kind::ControlCancel => continue,
        };
        let mut out = Vec::new();
        Encoder::new(&mut out, 0)
            .encode(&write)
            .unwrap_or_else(|error| panic!("{line}: {error}"));
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{want}\n"),
            "{line}"
        );
        writes += 1;
    }
    assert!(writes >= 40, "{writes} writes");
}
