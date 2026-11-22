#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};
    use std::time::{Duration, Instant};

    use oxap_adapter::{gojson::go_marshal, runtime::Runtime};
    use oxap_adapter_hermes::{
        native::EVENT_MESSAGE_COMPLETE, process::ProcessConfig, GatewaySession, Observation,
        PromptObservation, RunObservation,
    };
    use oxap_protocol::ReasoningLevel;

    let args: Vec<_> = std::env::args().skip(1).collect();
    let (python, source, state, mode) = match args.as_slice() {
        [python, source, state] => (python, source, state, ""),
        [python, source, state, mode]
            if matches!(
                mode.as_str(),
                "--prompt" | "--observe" | "--start" | "--project" | "--run"
            ) =>
        {
            (python, source, state, mode.as_str())
        }
        _ => {
            return Err(
                "usage: gateway_session <python> <hermes-source> <isolated-state> [--prompt|--observe|--start|--project|--run]"
                    .into(),
            );
        }
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
        "",
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
    } else if !mode.is_empty() {
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
