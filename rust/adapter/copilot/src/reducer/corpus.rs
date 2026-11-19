use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use oxap_adapter::memory::{Clock, IdGenerator};
use serde::Deserialize;
use serde_json::value::RawValue;

use super::Reducer;
use crate::native::{decode_event, parse, text, Kind, Object};
use crate::CORPUS_DIRECTORY;

const LIFECYCLE: [&str; 6] = [
    "completed-text",
    "metadata-107",
    "queued-busy-period",
    "unknown-event",
    "session-error",
    "foreign-session",
];
const USAGE: [&str; 11] = [
    "reasoning-usage",
    "usage-sparse",
    "usage-precision",
    "usage-wide",
    "usage-partial-call",
    "usage-overflow",
    "usage-negative-zero",
    "cost-wide",
    "cost-unrepresentable",
    "cost-unknown",
    "cost-partial",
];
const TOOLS: [&str; 5] = [
    "tool-success",
    "tool-denied",
    "tool-orphan",
    "tool-incomplete",
    "tool-duplicate-end",
];
const CODEC: [&str; 2] = ["invalid-wire", "invalid-event"];

#[derive(Default)]
struct Counter(AtomicU64);

impl IdGenerator for Counter {
    fn new_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::SeqCst) + 1)
    }
}

#[derive(Default)]
struct Ticks(AtomicI64);

impl Clock for Ticks {
    fn now_ms(&self) -> i64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }
}

#[derive(Deserialize)]
struct Step {
    action: String,
    direction: String,
    raw: Box<RawValue>,
}

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(&*CORPUS_DIRECTORY)
}

fn reducer() -> Reducer {
    let ids = Arc::new(Counter::default());
    Reducer::new("s1".into(), "ses".into(), ids, Arc::new(Ticks::default()))
}

fn apply(reducer: &mut Reducer, admitted: Option<usize>, step: &Step) -> Result<Option<usize>, String> {
    let direction = if step.action == "admit" {
        "host-to-copilot"
    } else {
        "copilot-to-host"
    };
    if step.direction != direction {
        return Err("invalid corpus direction".into());
    }
    let message = parse(step.raw.get().as_bytes());
    if step.action == "reject-wire" {
        return match message {
            Ok(_) => Err("accepted malformed wire".into()),
            Err(_) => Ok(admitted),
        };
    }
    let message = message.map_err(|error| error.to_string())?;
    match step.action.as_str() {
        "reject-event" => match decode_event(&message) {
            Ok(_) => Err("accepted malformed event".into()),
            Err(_) => Ok(admitted),
        },
        "admit" => {
            if message.kind != Kind::Request || message.method != "session.send" {
                return Err("invalid admission".into());
            }
            reducer.admit(false).map(Some).map_err(|error| error.to_string())
        }
        "bind" => {
            let result = message.result.as_deref().map_or("null", RawValue::get);
            let result: Option<Object> = serde_json::from_str(result).map_err(|e| e.to_string())?;
            let id = result.as_ref().map(|result| text(result, "messageId"));
            match (admitted, message.kind, id) {
                (Some(index), Kind::Response, Some(id)) if !id.is_empty() => {
                    reducer.runs[index].native_message = id;
                    Ok(admitted)
                }
                _ => Err("unbound reply".into()),
            }
        }
        "observe" => {
            let event = decode_event(&message).map_err(|error| error.to_string())?;
            let event = event.ok_or("missing event")?;
            reducer.observe(&event).map_err(|error| error.to_string())?;
            Ok(admitted)
        }
        other => Err(format!("unhandled action {other}")),
    }
}

fn canonical(raw: &str) -> String {
    let trimmed = raw.trim_start();
    if trimmed.starts_with('{') {
        let object: BTreeMap<String, Box<RawValue>> = serde_json::from_str(raw).unwrap();
        let members = object
            .iter()
            .map(|(key, value)| format!("{key:?}:{}", canonical(value.get())));
        return format!("{{{}}}", members.collect::<Vec<_>>().join(","));
    }
    if trimmed.starts_with('[') {
        let items: Vec<Box<RawValue>> = serde_json::from_str(raw).unwrap();
        let items = items.iter().map(|item| canonical(item.get()));
        return format!("[{}]", items.collect::<Vec<_>>().join(","));
    }
    if trimmed.starts_with('"') {
        let text: String = serde_json::from_str(raw).unwrap();
        return format!("{text:?}");
    }
    let literal = trimmed.trim_end();
    if literal.starts_with('-') || literal.starts_with(|c: char| c.is_ascii_digit()) {
        return exact(literal);
    }
    literal.to_owned()
}

fn exact(number: &str) -> String {
    let (sign, unsigned) = match number.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", number),
    };
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().unwrap()),
        None => (unsigned, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return "0".into();
    }
    let trimmed = digits.trim_end_matches('0');
    let exponent = exponent - fraction.len() as i64 + (digits.len() - trimmed.len()) as i64;
    format!("{sign}{trimmed}e{exponent}")
}

fn replay(name: &str) {
    let directory = corpus().join(name);
    let native = std::fs::read_to_string(directory.join("native.jsonl")).unwrap();
    let mut reducer = reducer();
    let mut admitted = None;
    for line in native.lines().filter(|line| !line.trim().is_empty()) {
        let step: Step = serde_json::from_str(line).unwrap();
        admitted = apply(&mut reducer, admitted, &step).unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    let want = std::fs::read_to_string(directory.join("expected-oap.json")).unwrap();
    let mut got = serde_json::to_string(&reducer.events).unwrap();
    if reducer.events.is_empty() {
        got = "null".into();
    }
    assert_eq!(canonical(&got), canonical(&want), "{name}");
}

#[test]
fn the_lifecycle_corpus_replays_to_its_expectation() {
    LIFECYCLE.iter().for_each(|name| replay(name));
}

#[test]
fn the_usage_corpus_replays_to_its_expectation() {
    USAGE.iter().for_each(|name| replay(name));
}

#[test]
fn the_tool_corpus_replays_to_its_expectation() {
    TOOLS.iter().for_each(|name| replay(name));
}

#[test]
fn the_codec_corpus_replays_to_its_expectation() {
    CODEC.iter().for_each(|name| replay(name));
}

#[test]
fn replay_rejects_direction_and_binding_corruption() {
    let steps = [
        ("bind", "host-to-copilot", r#"{"jsonrpc":"2.0","id":1,"result":{"messageId":"m1"}}"#),
        ("observe", "host-to-copilot", r#"{"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"ses","event":{"type":"session.idle","data":{}}}}"#),
        ("bind", "copilot-to-host", r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
        ("bind", "copilot-to-host", r#"{"jsonrpc":"2.0","id":1,"result":null}"#),
    ];
    for (action, direction, raw) in steps {
        let mut reducer = reducer();
        let index = reducer.admit(false).unwrap();
        let step = Step {
            action: action.into(),
            direction: direction.into(),
            raw: RawValue::from_string(raw.into()).unwrap(),
        };
        assert!(apply(&mut reducer, Some(index), &step).is_err(), "accepted corrupted {action} step: {raw}");
    }
}

#[derive(Deserialize)]
struct Manifest {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    path: String,
}

#[test]
fn every_manifest_case_is_replayed_exactly_once() {
    let mut known = BTreeSet::new();
    for name in LIFECYCLE.iter().chain(&USAGE).chain(&TOOLS).chain(&CODEC) {
        assert!(known.insert(*name), "repeated replay {name}");
    }
    let manifest = std::fs::read_to_string(corpus().join("manifest.json")).unwrap();
    let manifest: Manifest = serde_json::from_str(&manifest).unwrap();
    for case in manifest.cases {
        assert!(
            case.path == case.id && known.remove(case.id.as_str()),
            "uncovered or repeated manifest case {}",
            case.id
        );
    }
    assert!(known.is_empty(), "replay cases absent from manifest: {known:?}");
}
