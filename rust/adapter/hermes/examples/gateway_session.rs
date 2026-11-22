#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};
    use std::time::{Duration, Instant};

    use oxap_adapter::{gojson::go_marshal, runtime::Runtime};
    use oxap_adapter_hermes::{
        native::EVENT_MESSAGE_COMPLETE, process::ProcessConfig, GatewaySession, Observation,
        PromptObservation, RunObservation,
    };
    use oxap_protocol::{
        EnvelopeType, InputAnswer, ReasoningLevel, UserInputRequestedPayload,
        UserInputResolveRequest,
    };

    let args: Vec<_> = std::env::args().skip(1).collect();
    let (python, source, state, mode, model, answers_file) = match args.as_slice() {
        [python, source, state] => (python, source, state, "", "", None),
        [python, source, state, mode, model, answers] if mode == "--run" => (
            python,
            source,
            state,
            mode.as_str(),
            model.as_str(),
            Some(answers),
        ),
        [python, source, state, mode, model] if matches!(mode.as_str(), "--run" | "--reopen") => {
            (python, source, state, mode.as_str(), model.as_str(), None)
        }
        [python, source, state, mode]
            if matches!(
                mode.as_str(),
                "--prompt" | "--observe" | "--start" | "--project" | "--run"
            ) =>
        {
            (python, source, state, mode.as_str(), "", None)
        }
        _ => {
            return Err(
                "usage: gateway_session <python> <hermes-source> <isolated-state> [--prompt|--observe|--start|--project|--run [model [answers-file]]]"
                    .into(),
            );
        }
    };
    let mut answers = if let Some(path) = answers_file {
        let answers: Vec<Vec<InputAnswer>> = serde_json::from_reader(std::fs::File::open(path)?)?;
        Some(answers.into_iter())
    } else {
        None
    };
    let config = ProcessConfig {
        path: python.clone(),
        args: vec!["-m".into(), "tui_gateway.entry".into()],
        dir: source.clone(),
        env: Some(vec![
            "PATH=/usr/bin:/bin".into(),
            "NODE_ENV=test".into(),
            format!("HOME={state}"),
            format!("HERMES_HOME={state}/hermes"),
            format!("TMPDIR={state}"),
            "PYTHONUNBUFFERED=1".into(),
            "PYTHONDONTWRITEBYTECODE=1".into(),
            "HTTP_PROXY=http://127.0.0.1:1".into(),
            "HTTPS_PROXY=http://127.0.0.1:1".into(),
            "ALL_PROXY=http://127.0.0.1:1".into(),
        ]),
        ..ProcessConfig::default()
    };
    let mut session = GatewaySession::create(
        &config,
        if mode == "--reopen" { "" } else { model },
        &ReasoningLevel::default(),
        Some(Instant::now() + Duration::from_secs(30)),
    )?;
    println!("runtime session ID: {}", session.runtime_id());
    println!("stored session ID: {}", session.stored_id());
    let mut start_error = None;
    if mode == "--run" {
        let now = AtomicI64::new(2);
        let next = AtomicU8::new(b'a');
        let runtime = Runtime::new(
            move || now.fetch_add(1, Ordering::Relaxed),
            move |kind| {
                format!(
                    "{kind}-{}",
                    char::from(next.fetch_add(1, Ordering::Relaxed))
                )
            },
        );
        let deadline = Some(Instant::now() + Duration::from_secs(30));
        let mut run =
            session.start_run("hello", "session".into(), "hermes-test", runtime, deadline)?;
        while let Some(observation) = run.next_observation(deadline)? {
            match observation {
                RunObservation::Event(event) => {
                    println!("envelope: {}", go_marshal(&serde_json::to_string(&event)?));
                    if let Some(answers) = answers.as_mut() {
                        if event.envelope_type == EnvelopeType::USER_INPUT_REQUESTED {
                            let input: UserInputRequestedPayload = event.decode_payload()?;
                            run.resolve_input(UserInputResolveRequest {
                                interaction_id: input.interaction_id,
                                requested_by: input.requested_by,
                                responded_by: input.responded_by,
                                session_id: input.session_id,
                                run_id: input.run_id,
                                answers: answers.next().ok_or("missing replay answers")?,
                            })?;
                        }
                    }
                }
                RunObservation::Other(Observation::Event(_)) => {}
                RunObservation::Other(Observation::Request(request)) => {
                    return Err(format!("unexpected gateway request: {}", request.method).into());
                }
            }
        }
    } else if mode == "--project" {
        let deadline = Some(Instant::now() + Duration::from_secs(30));
        let start = session.start_prompt("hello", deadline)?;
        println!("prompt started at native sequence {}", start.seq);
        loop {
            match session.next_prompt_observation(deadline)? {
                PromptObservation::Delta(part) => {
                    println!("delta: {}", serde_json::to_string(&part)?);
                }
                PromptObservation::Completed { text, usage } => {
                    println!(
                        "completed: {}",
                        serde_json::json!({"text": text, "usage": usage})
                    );
                    break;
                }
                PromptObservation::Failed(error) => {
                    return Err(serde_json::to_string(&error)?.into());
                }
                PromptObservation::Other(Observation::Event(_)) => {}
                PromptObservation::Other(Observation::Request(request)) => {
                    return Err(format!("unexpected gateway request: {}", request.method).into());
                }
            }
        }
    } else if mode == "--start" {
        let deadline = Some(Instant::now() + Duration::from_secs(30));
        match session.start_prompt("Hello from the Rust Hermes adapter.", deadline) {
            Ok(event) => println!("prompt started at native sequence {}", event.seq),
            Err(error) => start_error = Some(error),
        }
    } else if !mode.is_empty() && mode != "--reopen" {
        let deadline = Some(Instant::now() + Duration::from_secs(30));
        session.submit_prompt("Hello from the Rust Hermes adapter.", deadline)?;
        println!("prompt status: streaming");
        if mode == "--observe" {
            loop {
                match session.next_observation(deadline)? {
                    Observation::Event(event) => {
                        println!("session observation {}: {}", event.seq, event.kind);
                        if event.kind == EVENT_MESSAGE_COMPLETE {
                            break;
                        }
                    }
                    Observation::Request(request) => {
                        return Err(
                            format!("unexpected gateway request: {}", request.method).into()
                        );
                    }
                }
            }
        } else {
            let interrupted = session.interrupt(deadline)?;
            println!("interrupt status: {}", interrupted.status);
            if interrupted.status != "interrupted" {
                return Err("expected the gateway to confirm interruption".into());
            }
        }
    }
    session.close()?;
    println!("gateway exited cleanly");
    if let Some(error) = start_error {
        return Err(error.to_string().into());
    }
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("the Hermes stdio gateway requires Unix");
    std::process::exit(1);
}
