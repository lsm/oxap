#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};

    use oxap_adapter_hermes::{process::ProcessConfig, GatewaySession};
    use oxap_protocol::ReasoningLevel;

    let [python, source, state]: [String; 3] = std::env::args()
        .skip(1)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "usage: gateway_session <python> <hermes-source> <isolated-state>")?;
    let config = ProcessConfig {
        path: python,
        args: vec!["-m".into(), "tui_gateway.entry".into()],
        dir: source,
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
    let session = GatewaySession::create(
        &config,
        "",
        &ReasoningLevel::default(),
        Some(Instant::now() + Duration::from_secs(30)),
    )?;
    println!("runtime session ID: {}", session.runtime_id());
    println!("stored session ID: {}", session.stored_id());
    session.close()?;
    println!("gateway exited cleanly");
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("the Hermes stdio gateway requires Unix");
    std::process::exit(1);
}
