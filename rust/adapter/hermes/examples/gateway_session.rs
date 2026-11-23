#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};
    use std::time::{Duration, Instant};

    use oxap_adapter::{gojson::go_marshal, runtime::Runtime, Error};
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
        [python, source, state, mode, model, answers]
            if matches!(mode.as_str(), "--run" | "--shared-input") =>
        {
            (
                python,
                source,
                state,
                mode.as_str(),
                model.as_str(),
                Some(answers),
            )
        }
        [python, source, state, mode, model]
            if matches!(
                mode.as_str(),
                "--run"
                    | "--reopen"
                    | "--reopen-deadline"
                    | "--shared-reopen"
                    | "--shared-submit"
                    | "--shared-cancel"
                    | "--shared-resume"
                    | "--shared-settings-reopen"
            ) =>
        {
            (python, source, state, mode.as_str(), model.as_str(), None)
        }
        [python, source, state, mode]
            if matches!(
                mode.as_str(),
                "--prompt"
                    | "--observe"
                    | "--start"
                    | "--project"
                    | "--run"
                    | "--shared-open"
                    | "--shared-settings"
                    | "--native-list"
                    | "--native-list-reuse"
            ) =>
        {
            (python, source, state, mode.as_str(), "", None)
        }
        _ => {
            return Err(
                "usage: gateway_session <python> <hermes-source> <isolated-state> [--prompt|--observe|--start|--project|--run [model [answers-file]]|--reopen <stored-id>|--shared-open|--shared-reopen <stored-id>|--shared-submit <model>|--shared-cancel <model>|--shared-input <model> <answers-file>|--shared-resume <model>|--shared-settings|--shared-settings-reopen <stored-id>|--native-list|--native-list-reuse]"
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
    if mode == "--native-list" {
        return native_list(config);
    }
    if mode == "--native-list-reuse" {
        return native_list_reuse(config, state);
    }
    if matches!(mode, "--shared-settings" | "--shared-settings-reopen") {
        return shared_settings(
            config,
            state,
            (mode == "--shared-settings-reopen").then_some(model),
        );
    }
    if matches!(mode, "--shared-open" | "--shared-reopen") {
        return shared_session(config, (mode == "--shared-reopen").then_some(model));
    }
    if matches!(
        mode,
        "--shared-submit" | "--shared-cancel" | "--shared-input" | "--shared-resume"
    ) {
        return shared_submit(config, state, model, mode, answers);
    }
    let timeout = if mode == "--reopen-deadline" {
        Duration::from_secs(1)
    } else {
        Duration::from_secs(30)
    };
    let deadline = Some(Instant::now() + timeout);
    if matches!(mode, "--reopen" | "--reopen-deadline") {
        match GatewaySession::reopen(&config, model, "session".into(), "", deadline) {
            Ok((session, state)) => {
                println!("runtime session ID: {}", session.runtime_id());
                println!("stored session ID: {}", session.stored_id());
                println!("state: {}", go_marshal(&serde_json::to_string(&state)?));
                session.close()?;
                println!("gateway exited cleanly");
            }
            Err(Error::UnsupportedControl(refusal)) => {
                println!("detail: {}", refusal.detail);
                println!(
                    "refusal: {}",
                    serde_json::json!({
                        "feature": refusal.feature, "reason": refusal.reason,
                    })
                );
            }
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    }
    let mut session = GatewaySession::create(&config, model, &ReasoningLevel::default(), deadline)?;
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

#[cfg(unix)]
fn shared_session(
    config: oxap_adapter_hermes::process::ProcessConfig,
    stored: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use futures_executor::block_on;
    use oxap_adapter::{Adapter, Error, OpenRequest};
    use oxap_protocol::{CompactionPolicy, Participant, SupportLevel};

    let adapter = oxap_adapter_hermes::Adapter::new(config, "configured".into());
    let adapter: &dyn Adapter = &adapter;
    let descriptor = block_on(adapter.probe())?;
    assert_eq!(
        descriptor.capabilities.endpoint.id.as_str(),
        "hermes.gateway"
    );
    assert!(!descriptor.capability_revision.is_empty());
    assert_eq!(
        descriptor.capabilities.features["session.open"].level,
        SupportLevel::NATIVE
    );
    let request = OpenRequest {
        session_id: "session".into(),
        participant: Participant {
            id: "user".into(),
            ..Participant::default()
        },
        reopen: stored.is_some(),
        native_session_id: stored.unwrap_or_default().into(),
        ..OpenRequest::default()
    };
    let with_compaction = OpenRequest {
        compaction_policy: Some(CompactionPolicy::default()),
        ..request.clone()
    };
    let refused = block_on(adapter.open(with_compaction));
    assert!(matches!(refused, Err(Error::UnsupportedControl(refusal))
        if refusal.feature == oxap_protocol::FEATURE_COMPACTION_POLICY));
    let session = block_on(adapter.open(request))?;
    let mut state = block_on(session.state())?;
    assert!(state.updated_at_ms > 0);
    state.updated_at_ms = 0;
    println!("state: {}", serde_json::to_string(&state)?);
    println!(
        "stored session ID: {}",
        session.native_session_id().ok_or("missing binding")?
    );
    block_on(session.close())?;
    block_on(session.close())?;
    assert!(matches!(
        block_on(session.state()),
        Err(Error::SessionClosed)
    ));
    println!("gateway exited cleanly");
    Ok(())
}

#[cfg(unix)]
fn native_list(
    config: oxap_adapter_hermes::process::ProcessConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    use futures_executor::block_on;
    use oxap_adapter::{Adapter, NativeListRequest, NativeListing};

    let adapter = oxap_adapter_hermes::Adapter::new(config, "configured".into());
    let lister = adapter.native_lister().ok_or("missing native lister")?;
    let listed = block_on(lister.native_list(NativeListRequest {
        limit: 2,
        ..NativeListRequest::default()
    }))?;
    assert_eq!(
        listed,
        [
            NativeListing {
                native_id: "stored-a".into(),
                title: "Named".into(),
                updated_at_ms: 1250,
                ..NativeListing::default()
            },
            NativeListing {
                native_id: "stored-b".into(),
                title: "first words".into(),
                updated_at_ms: 2000,
                ..NativeListing::default()
            },
        ]
    );
    block_on(adapter.close_listing_gateway())?;
    println!("NATIVE SESSIONS: {}\ngateway exited cleanly", listed.len());
    Ok(())
}

#[cfg(unix)]
fn native_list_reuse(
    config: oxap_adapter_hermes::process::ProcessConfig,
    state_dir: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use futures_executor::block_on;
    use oxap_adapter::{Adapter, Error, NativeListRequest};
    use std::{
        path::Path,
        task::{Context, Poll, Waker},
        time::{Duration, Instant},
    };

    let adapter = oxap_adapter_hermes::Adapter::new(config.clone(), "configured".into());
    let lister = adapter.native_lister().ok_or("missing native lister")?;
    let list = || {
        lister.native_list(NativeListRequest {
            limit: 1,
            ..NativeListRequest::default()
        })
    };
    assert_eq!(block_on(list())?[0].native_id, "stored-1");
    assert!(
        matches!(block_on(list()), Err(Error::Harness(detail)) if detail.contains("list refused")),
        "second listing must reach the retained gateway"
    );
    let mut held = list();
    assert!(matches!(
        held.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    let state_dir = Path::new(state_dir);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !state_dir.join("listing-held").exists() {
        assert!(Instant::now() < deadline, "retained listing did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(block_on(list())?[0].native_id, "stored-2");
    assert!(state_dir.join("listing-eof-2").exists());
    std::fs::write(state_dir.join("release-listing"), "")?;
    assert_eq!(block_on(held)?[0].native_id, "stored-1");
    assert_eq!(block_on(list())?[0].native_id, "stored-3");
    drop(adapter);
    assert!(state_dir.join("listing-eof-3").exists());
    let fresh = oxap_adapter_hermes::Adapter::new(config, "configured".into());
    let lister = fresh.native_lister().ok_or("missing native lister")?;
    assert!(matches!(
        block_on(lister.native_list(NativeListRequest {
            limit: 1,
            ..NativeListRequest::default()
        })),
        Err(Error::Harness(_))
    ));
    println!("REUSED, ISOLATED, AND RETRIED LISTING");
    Ok(())
}

#[cfg(unix)]
fn shared_settings(
    config: oxap_adapter_hermes::process::ProcessConfig,
    state_dir: &str,
    stored: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use futures_executor::block_on;
    use oxap_adapter::{Adapter, Error, OpenRequest, SubmitRequest};
    use oxap_protocol::{
        Message, MessageContent, MessageRole, MessageSubmitRequest, ReasoningLevel,
        SessionSettingsUpdateRequest, SessionStatus,
    };
    use std::{
        path::Path,
        task::{Context, Poll, Waker},
        time::{Duration, Instant},
    };

    let adapter = oxap_adapter_hermes::Adapter::new(config, "configured".into());
    let session = block_on(adapter.open(OpenRequest {
        session_id: "session".into(),
        native_session_id: stored.unwrap_or_default().into(),
        reopen: stored.is_some(),
        reasoning_level: ReasoningLevel::OFF,
        ..OpenRequest::default()
    }))?;
    assert_eq!(
        block_on(session.state())?.reasoning_level,
        ReasoningLevel::OFF
    );
    if stored.is_none() {
        let settings = session
            .settings_updater()
            .ok_or("missing settings updater")?;
        let update = |reasoning_level| {
            settings.update_settings(SessionSettingsUpdateRequest {
                session_id: "session".into(),
                reasoning_level,
                ..SessionSettingsUpdateRequest::default()
            })
        };
        assert!(
            matches!(block_on(update(ReasoningLevel::HIGH)), Err(Error::UnsupportedControl(refusal))
            if refusal.detail.contains("effort rejected"))
        );
        assert_eq!(
            block_on(session.state())?.reasoning_level,
            ReasoningLevel::OFF
        );
        let state_dir = Path::new(state_dir);
        let mut context = Context::from_waker(Waker::noop());
        let mut updating = update(ReasoningLevel::LOW);
        assert!(matches!(
            updating.as_mut().poll(&mut context),
            Poll::Pending
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !state_dir.join("settings-received").exists() {
            assert!(
                Instant::now() < deadline,
                "gateway did not receive settings update"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut submit = session.submit(SubmitRequest {
            request: MessageSubmitRequest {
                session_id: "session".into(),
                messages: vec![Message {
                    content: MessageContent::from_text("hello"),
                    role: MessageRole::USER,
                    ..Message::default()
                }],
                ..MessageSubmitRequest::default()
            },
            ..SubmitRequest::default()
        });
        assert!(matches!(submit.as_mut().poll(&mut context), Poll::Pending));
        std::thread::sleep(Duration::from_millis(100));
        assert!(matches!(submit.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(block_on(session.state())?.status, SessionStatus::IDLE);
        std::fs::write(state_dir.join("release-settings"), "")?;
        let (changed, state) = block_on(updating)?;
        assert_eq!(changed.previous_reasoning_level, ReasoningLevel::OFF);
        assert_eq!(changed.reasoning_level, ReasoningLevel::LOW);
        assert_eq!(state.reasoning_level, ReasoningLevel::LOW);
        assert_eq!(state.status, SessionStatus::IDLE);
        let (_, stream) = block_on(submit)?;
        assert!(matches!(
            block_on(update(ReasoningLevel::HIGH)),
            Err(Error::RunActive)
        ));
        std::fs::write(state_dir.join("release-settings-run"), "")?;
        assert_eq!(replay_wires(stream)?.len(), 2);
        assert_eq!(
            block_on(session.state())?.reasoning_level,
            ReasoningLevel::LOW
        );
        println!("LIVE UPDATES: 1\nREFUSED UPDATES: 2");
    }
    block_on(session.close())?;
    println!("OPEN REASONING: off\ngateway exited cleanly");
    Ok(())
}

#[cfg(unix)]
fn shared_submit(
    config: oxap_adapter_hermes::process::ProcessConfig,
    state_dir: &str,
    model: &str,
    mode: &str,
    answers: Option<std::vec::IntoIter<Vec<oxap_protocol::InputAnswer>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::future::poll_fn;
    use std::path::Path;
    use std::task::{Context, Poll, Waker};

    use futures_executor::block_on;
    use oxap_adapter::{Adapter, Error, OpenRequest, SubmitRequest};
    use oxap_protocol::{
        EnvelopeType, Message, MessageContent, MessageRole, MessageSubmitRequest, Participant,
        RequestedDeliveryMode, SessionStatus,
    };

    let state_dir = Path::new(state_dir);
    let mut adapter = oxap_adapter_hermes::Adapter::new(config, model.to_owned());
    if mode == "--shared-resume" {
        let capacity = if state_dir.join("shared-resume-tiny").exists() {
            1
        } else {
            256
        };
        if capacity == 1 {
            adapter = adapter.with_journal_capacity(capacity);
        }
        let descriptor = block_on(adapter.probe())?;
        assert_eq!(descriptor.journal.capacity, capacity);
    }
    let session = block_on(
        adapter.open(OpenRequest {
            session_id: "session".into(),
            participant: Participant {
                id: if mode == "--shared-input" {
                    "operator"
                } else {
                    "user"
                }
                .into(),
                ..Participant::default()
            },
            ..OpenRequest::default()
        }),
    )?;
    let request = SubmitRequest {
        envelope_id: "caller-submit".into(),
        request: MessageSubmitRequest {
            session_id: "session".into(),
            messages: vec![Message {
                role: MessageRole::USER,
                content: MessageContent::from_text("hello"),
                ..Message::default()
            }],
            delivery: RequestedDeliveryMode::AUTO,
            ..MessageSubmitRequest::default()
        },
    };
    if let Some(answers) = answers {
        return shared_input(session.as_ref(), request, answers);
    }
    if mode == "--shared-cancel" {
        return shared_cancel(session.as_ref(), request, state_dir);
    }
    if mode == "--shared-resume" {
        return shared_resume(session.as_ref(), request, state_dir);
    }
    let mut unsupported = request.clone();
    unsupported.request.model_id = Some("other".into());
    assert!(matches!(
        block_on(session.submit(unsupported)),
        Err(Error::UnsupportedControl(_))
    ));
    let mut pending = session.submit(request.clone());
    assert!(matches!(
        pending
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !state_dir.join("prompt-received").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "gateway did not receive prompt"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let state = block_on(session.state())?;
    assert!(state.active_run_id.is_none() && state.active_runs.is_empty());
    assert!(matches!(
        block_on(session.submit(request.clone())),
        Err(Error::RunActive)
    ));
    std::fs::write(state_dir.join("release-refusal"), "")?;
    assert!(matches!(block_on(pending), Err(Error::Harness(_))));
    assert_eq!(
        (state.status, block_on(session.state())?.status),
        (SessionStatus::QUEUED, SessionStatus::IDLE)
    );
    println!("PENDING ADMISSION: queued");
    for expected in ["queued", "voice session active"] {
        let Err(error) = block_on(session.submit(request.clone())) else {
            panic!("gateway refusal admitted a run");
        };
        assert!(error.to_string().contains(expected), "{error}");
        let state = block_on(session.state())?;
        assert_eq!(state.status, SessionStatus::IDLE);
        assert!(state.active_run_id.is_none() && state.active_runs.is_empty());
    }
    println!("REFUSALS RETURNED TO IDLE: 3");
    for index in 1..=2 {
        let (admission, mut stream) = block_on(session.submit(request.clone()))?;
        let run_id = admission.run_id.clone().ok_or("missing admitted run")?;
        println!("admission: {}", serde_json::to_string(&admission)?);
        let state = block_on(session.state())?;
        assert_eq!(state.status, SessionStatus::RUNNING);
        assert_eq!(state.active_run_id.as_ref(), Some(&run_id));
        assert_eq!(state.active_runs.len(), 1);
        assert_eq!(
            state.active_runs[0].admitted_submit_requests[0].as_str(),
            "caller-submit"
        );
        assert!(matches!(
            block_on(session.submit(request.clone())),
            Err(Error::RunActive)
        ));
        let first = block_on(poll_fn(|cx| stream.as_mut().poll_next(cx)))
            .ok_or("missing run.started")??;
        assert_eq!(first.envelope_type, EnvelopeType::RUN_STARTED);
        println!("envelope: {}", serde_json::to_string(&first)?);
        std::fs::write(state_dir.join(format!("release-run-{index}")), "")?;
        if index == 1 {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while block_on(session.state())?.status != SessionStatus::IDLE {
                assert!(
                    std::time::Instant::now() < deadline,
                    "run did not settle while the stream was unread"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        let mut original = vec![serde_json::to_string(&first)?];
        while let Some(event) = block_on(poll_fn(|cx| stream.as_mut().poll_next(cx))) {
            let wire = serde_json::to_string(&event?)?;
            println!("envelope: {wire}");
            original.push(wire);
        }
        if index == 1 && state_dir.join("check-resume-window").exists() {
            check_resume_window(session.as_ref(), run_id, &original)?;
        }
        let state = block_on(session.state())?;
        assert_eq!(state.status, SessionStatus::IDLE);
        assert!(state.active_run_id.is_none() && state.active_runs.is_empty());
    }
    block_on(session.close())?;
    println!("gateway exited cleanly");
    Ok(())
}

#[cfg(unix)]
fn replay_wires(
    mut stream: oxap_adapter::EventStream,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    use std::future::poll_fn;

    let mut wires = Vec::new();
    while let Some(event) = futures_executor::block_on(poll_fn(|cx| stream.as_mut().poll_next(cx)))
    {
        wires.push(serde_json::to_string(&event?)?);
    }
    Ok(wires)
}

#[cfg(unix)]
fn shared_resume(
    session: &dyn oxap_adapter::Session,
    request: oxap_adapter::SubmitRequest,
    state_dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use futures_executor::block_on;
    use oxap_adapter::{Error, ResumeRequest};
    use oxap_protocol::SessionStatus;

    assert!(matches!(
        block_on(session.resume(ResumeRequest {
            run_id: "sess0001".into(),
            after_sequence: 0
        })),
        Err(Error::RunNotFound)
    ));
    let (admission, original) = block_on(session.submit(request))?;
    let run_id = admission.run_id.ok_or("missing admitted run")?;
    let resume = |after_sequence| {
        block_on(session.resume(ResumeRequest {
            run_id: run_id.clone(),
            after_sequence,
        }))
    };
    let (live, follow) = resume(0)?;
    assert_eq!(live.run_id, run_id);
    assert_eq!(live.requested_after, 0);
    assert_eq!((live.replayed_from, live.replayed_through), (1, 1));
    assert_eq!(live.state.status, SessionStatus::RUNNING);
    assert_eq!(live.state.active_run_id, Some(run_id.clone()));
    assert!(matches!(resume(2), Err(Error::ReplayCursorFuture)));
    std::fs::write(state_dir.join("release-run-1"), "")?;
    let original = replay_wires(original)?;
    assert_eq!(original.len(), 2);
    assert_eq!(replay_wires(follow)?, original);
    println!("LIVE REPLAY ENVELOPES: {}", original.len());
    let (ended, suffix) = resume(1)?;
    if state_dir.join("shared-resume-exit").exists() {
        assert_eq!(ended.state.status, SessionStatus::ERROR);
        let terminal: serde_json::Value = serde_json::from_str(&original[1])?;
        assert_eq!(terminal["type"], "run.failed");
        assert_eq!(terminal["payload"]["error"]["code"], "hermes_process_exit");
        println!("REPLAY AFTER GATEWAY EXIT: 1");
    } else {
        assert_eq!(ended.state.status, SessionStatus::IDLE);
    }
    if state_dir.join("shared-resume-tiny").exists() {
        let Err(Error::ReplayGap(gap)) = resume(0) else {
            return Err("expected a gap in the one-entry journal".into());
        };
        assert_eq!((gap.oldest_available, gap.latest_available), (2, 2));
        println!("REPLAY WINDOW: 1");
    }
    assert_eq!((ended.replayed_from, ended.replayed_through), (2, 2));
    assert_eq!(replay_wires(suffix)?, original[1..]);
    let (at_end, empty) = resume(2)?;
    assert_eq!((at_end.replayed_from, at_end.replayed_through), (2, 2));
    assert!(replay_wires(empty)?.is_empty());
    assert!(matches!(resume(3), Err(Error::ReplayCursorFuture)));
    block_on(session.close())?;
    assert!(matches!(resume(0), Err(Error::SessionClosed)));
    println!("COMPLETED REPLAY ENVELOPES: 1\ngateway exited cleanly");
    Ok(())
}

#[cfg(unix)]
fn check_resume_window(
    session: &dyn oxap_adapter::Session,
    run_id: oxap_protocol::RunId,
    original: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    use futures_executor::block_on;
    use oxap_adapter::{Error, ResumeRequest};

    let resume = |after_sequence| {
        block_on(session.resume(ResumeRequest {
            run_id: run_id.clone(),
            after_sequence,
        }))
    };
    let Err(Error::ReplayGap(gap)) = resume(0) else {
        return Err("expected a replay gap for the evicted prefix".into());
    };
    assert_eq!(gap.requested_after, 0);
    assert_eq!(gap.latest_available, u64::try_from(original.len())?);
    let after = gap.oldest_available - 1;
    let (recovery, stream) = resume(after)?;
    assert_eq!(
        (recovery.replayed_from, recovery.replayed_through),
        (gap.oldest_available, gap.latest_available)
    );
    assert_eq!(replay_wires(stream)?, original[usize::try_from(after)?..]);
    assert_eq!(gap.latest_available - after, 256);
    println!("REPLAY WINDOW: 256");
    Ok(())
}

#[cfg(unix)]
fn shared_input(
    session: &dyn oxap_adapter::Session,
    request: oxap_adapter::SubmitRequest,
    mut answers: impl Iterator<Item = Vec<oxap_protocol::InputAnswer>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::future::poll_fn;

    use futures_executor::block_on;
    use oxap_adapter::{Error, InteractionResolution};
    use oxap_protocol::{
        EnvelopeType, RunStatus, SessionStatus, UserInputRequestedPayload, UserInputResolveRequest,
    };

    let (_, mut stream) = block_on(session.submit(request))?;
    let mut gates = 0;
    while let Some(event) = block_on(poll_fn(|cx| stream.as_mut().poll_next(cx))) {
        let event = event?;
        println!("envelope: {}", serde_json::to_string(&event)?);
        assert_ne!(
            event.envelope_type,
            EnvelopeType::RUN_FAILED,
            "shared gate failed the run"
        );
        if event.envelope_type != EnvelopeType::USER_INPUT_REQUESTED {
            continue;
        }
        let input: UserInputRequestedPayload = event.decode_payload()?;
        assert_eq!(input.responded_by.as_str(), "operator");
        let state = block_on(session.state())?;
        assert_eq!(state.status, SessionStatus::WAITING_FOR_INPUT);
        assert_eq!(state.active_runs[0].status, RunStatus::WAITING_FOR_INPUT);
        assert_eq!(
            state.active_runs[0].pending_interactions.as_slice(),
            std::slice::from_ref(&input.interaction_id)
        );
        let resolution = InteractionResolution {
            run_id: input.run_id,
            responded_by: input.responded_by,
            input: Some(UserInputResolveRequest {
                interaction_id: input.interaction_id.clone(),
                answers: answers.next().ok_or("missing replay answers")?,
                ..UserInputResolveRequest::default()
            }),
            ..InteractionResolution::default()
        };
        let mut foreign = resolution.clone();
        foreign.responded_by = "foreign".into();
        assert!(matches!(
            block_on(session.resolve(foreign)),
            Err(Error::InvalidResolution)
        ));
        let mut invalid = resolution.clone();
        invalid.input.as_mut().unwrap().answers.clear();
        assert!(matches!(
            block_on(session.resolve(invalid)),
            Err(Error::InvalidResolution)
        ));
        block_on(session.resolve(resolution.clone()))?;
        assert!(!block_on(session.state())?
            .active_runs
            .iter()
            .any(|run| run.pending_interactions.contains(&input.interaction_id)));
        assert!(matches!(
            block_on(session.resolve(resolution)),
            Err(Error::InteractionNotFound)
        ));
        gates += 1;
    }
    assert_eq!(gates, 4);
    assert_eq!(block_on(session.state())?.status, SessionStatus::IDLE);
    block_on(session.close())?;
    println!("SHARED INPUTS: {gates}\ngateway exited cleanly");
    Ok(())
}

#[cfg(unix)]
fn shared_cancel(
    session: &dyn oxap_adapter::Session,
    request: oxap_adapter::SubmitRequest,
    state_dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        future::poll_fn,
        task::{Context, Poll, Waker},
        time::Duration,
    };

    use futures_executor::block_on;
    use oxap_adapter::{Error, EventStream};
    use oxap_protocol::{Envelope, EnvelopeType, RunStatus, SessionStatus};

    fn next(stream: &mut EventStream) -> Result<Envelope, Box<dyn std::error::Error>> {
        Ok(block_on(poll_fn(|cx| stream.as_mut().poll_next(cx))).ok_or("missing envelope")??)
    }

    assert!(matches!(
        block_on(session.cancel("stale".into())),
        Err(Error::RunNotFound)
    ));
    let (admission, mut stream) = block_on(session.submit(request.clone()))?;
    let run = admission.run_id.ok_or("missing run ID")?;
    assert_eq!(next(&mut stream)?.envelope_type, EnvelopeType::RUN_STARTED);
    let race = state_dir.join("cancel-race").exists();
    let mut context = Context::from_waker(Waker::noop());
    let mut cancellation = session.cancel(run.clone());
    let mut following = None;
    if race {
        assert!(matches!(
            cancellation.as_mut().poll(&mut context),
            Poll::Pending
        ));
        assert_eq!(
            next(&mut stream)?.envelope_type,
            EnvelopeType::RUN_COMPLETED
        );
        let mut submit = session.submit(request.clone());
        assert!(matches!(submit.as_mut().poll(&mut context), Poll::Pending));
        std::thread::sleep(Duration::from_millis(100));
        assert!(matches!(submit.as_mut().poll(&mut context), Poll::Pending));
        following = Some(submit);
        std::fs::write(state_dir.join("release-interrupt"), "")?;
    }
    let receipt = block_on(cancellation)?;
    assert!(receipt.accepted);
    assert_eq!(receipt.session_id.as_str(), "session");
    assert_eq!(receipt.run_id, run);
    assert_eq!(receipt.status, RunStatus::CANCELLING);
    println!("CANCEL INTENT: accepted");
    if !race {
        assert_eq!(block_on(session.cancel(run.clone()))?, receipt);
        assert_eq!(block_on(session.state())?.status, SessionStatus::RUNNING);
        assert!(matches!(
            stream.as_mut().poll_next(&mut context),
            Poll::Pending
        ));
        std::fs::write(state_dir.join("release-interrupt"), "")?;
        let terminal = next(&mut stream)?;
        assert_eq!(terminal.envelope_type, EnvelopeType::RUN_FAILED);
        let payload: serde_json::Value =
            serde_json::from_str(terminal.payload.ok_or("missing failure payload")?.get())?;
        assert_eq!(payload["error"]["code"], "hermes_interrupted");
    }
    assert!(block_on(poll_fn(|cx| stream.as_mut().poll_next(cx))).is_none());
    assert!(matches!(
        block_on(session.cancel(run)),
        Err(Error::RunNotFound)
    ));
    let (_, mut stream) = block_on(following.unwrap_or_else(|| session.submit(request)))?;
    assert_eq!(next(&mut stream)?.envelope_type, EnvelopeType::RUN_STARTED);
    assert_eq!(
        next(&mut stream)?.envelope_type,
        EnvelopeType::RUN_COMPLETED
    );
    assert!(block_on(poll_fn(|cx| stream.as_mut().poll_next(cx))).is_none());
    assert_eq!(block_on(session.state())?.status, SessionStatus::IDLE);
    block_on(session.close())?;
    assert!(matches!(
        block_on(session.cancel("stale".into())),
        Err(Error::SessionClosed)
    ));
    println!("TERMINALS: 2\ngateway exited cleanly");
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("the Hermes stdio gateway requires Unix");
    std::process::exit(1);
}
