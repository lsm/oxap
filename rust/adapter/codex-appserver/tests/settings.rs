mod support;

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use oxap_adapter::gojson::go_marshal;
use oxap_adapter::runtime::Runtime;
use oxap_adapter::testkit::drain;
use oxap_adapter::{Error, OpenRequest};
use oxap_protocol::{
    Message, MessageContent, MessageRole, MessageSubmitRequest, Participant, RequestedDeliveryMode,
    SessionSettingsUpdateRequest,
};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{json, Value};
use support::{adapter, Fake, Reply};

#[derive(Deserialize)]
struct Step {
    settings: Option<SessionSettingsUpdateRequest>,
    #[serde(default)]
    submit: bool,
    #[serde(default)]
    submit_fail: String,
    #[serde(default)]
    complete: bool,
    #[serde(default)]
    close: bool,
}

#[derive(Deserialize)]
struct Scenario {
    name: String,
    #[serde(default)]
    reasoning: String,
    steps: Vec<Step>,
    results: Vec<Value>,
    turn_starts: Vec<Value>,
    events: Box<RawValue>,
}

fn submission() -> MessageSubmitRequest {
    MessageSubmitRequest {
        session_id: "session-1".into(),
        messages: vec![Message {
            role: MessageRole::USER,
            content: MessageContent::from_text("hello"),
            ..Message::default()
        }],
        delivery: RequestedDeliveryMode::AUTO,
        ..MessageSubmitRequest::default()
    }
}

fn refusal(error: &Error) -> String {
    let named = match error {
        Error::UnsupportedControl(control) => format!(
            "unsupported_control {} {} {}",
            control.feature, control.reason, control.field
        ),
        Error::SessionClosed => "session_closed".into(),
        Error::RunNotFound => "run_not_found".into(),
        Error::RunActive => "run_active".into(),
        other => format!("error: {other}"),
    };
    match named.split_once("start Codex turn: ") {
        Some((head, _)) => format!("{head}start Codex turn: "),
        None => named,
    }
}

fn shaped(result: &Value) -> Value {
    match result["error"].as_str() {
        Some(error) => match error.split_once("start Codex turn: ") {
            Some((head, _)) => json!({"error": format!("{head}start Codex turn: ")}),
            None => result.clone(),
        },
        None => result.clone(),
    }
}

#[test]
fn each_settings_scenario_answers_as_go_does() {
    let scenarios: Vec<Scenario> = serde_json::from_str(include_str!("go_settings.json")).unwrap();
    assert_eq!(scenarios.len(), 9);
    for scenario in scenarios {
        let turn: Reply = Ok(r#"{"turn":{"id":"native-turn","status":"inProgress"}}"#);
        let replies: Vec<Reply> = scenario
            .steps
            .iter()
            .filter(|step| step.submit || !step.submit_fail.is_empty())
            .map(|step| {
                if step.submit {
                    turn
                } else {
                    Err(&*Box::leak(step.submit_fail.clone().into_boxed_str()))
                }
            })
            .collect();
        let fake = Fake::new(
            [Ok(r#"{"thread":{"id":"native-thread"}}"#)]
                .into_iter()
                .chain(replies),
        );
        let clock = Arc::new(AtomicI64::new(0));
        let ids = Arc::new(AtomicU64::new(0));
        let adapter = adapter(&fake, |config| {
            config.model = "glm-test".into();
            config.journal_capacity = 32;
            config.runtime = Runtime::new(
                move || clock.fetch_add(1, Ordering::SeqCst) + 1,
                move |kind| format!("{kind}-{:02}", ids.fetch_add(1, Ordering::SeqCst) + 1),
            );
        });
        let session = adapter
            .open(OpenRequest {
                session_id: "session-1".into(),
                participant: Participant {
                    id: "user".into(),
                    ..Participant::default()
                },
                reasoning_level: scenario.reasoning.as_str().into(),
                ..OpenRequest::default()
            })
            .unwrap();
        let mut results = Vec::new();
        let mut events = Vec::new();
        let mut stream = None;
        let mut turn_starts: Vec<Value> = Vec::new();
        for step in scenario.steps {
            if let Some(settings) = step.settings {
                results.push(match session.update_settings(settings) {
                    Ok((response, state)) => json!({"response": response, "state": state}),
                    Err(error) => json!({"error": refusal(&error)}),
                });
            } else if step.submit || !step.submit_fail.is_empty() {
                match session.submit(submission()) {
                    Ok((_, submitted)) => {
                        let (_, params) = fake.calls().last().cloned().unwrap();
                        turn_starts.push(serde_json::from_str(&params).unwrap());
                        stream = Some(submitted);
                    }
                    Err(error) => results.push(json!({"error": refusal(&error)})),
                }
            } else if step.complete {
                fake.notify(
                    "turn/started",
                    r#"{"threadId":"native-thread","turn":{"id":"native-turn","status":"inProgress"}}"#,
                );
                fake.notify(
                    "turn/completed",
                    r#"{"threadId":"native-thread","turn":{"id":"native-turn","status":"completed"}}"#,
                );
                events.extend(drain(stream.as_mut().unwrap(), Duration::from_secs(5)));
            } else if step.close {
                session.close().unwrap();
            }
        }
        let expected: Vec<Value> = scenario.results.iter().map(shaped).collect();
        assert_eq!(results, expected, "{}", scenario.name);
        assert_eq!(turn_starts, scenario.turn_starts, "{}", scenario.name);
        assert_eq!(
            go_marshal(&serde_json::to_string(&events).unwrap()),
            go_marshal(scenario.events.get()),
            "{}",
            scenario.name
        );
    }
}
