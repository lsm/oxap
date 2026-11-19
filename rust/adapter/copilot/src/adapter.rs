use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use oxap_adapter::memory::{Clock, IdGenerator};
use oxap_adapter::{
    refuse_unadvertised_settings, refuse_unadvertised_tool_sources, refuse_unadvertised_tools,
    Adapter as AdapterContract, BoxFuture, Descriptor, Error, JournalDescriptor, OpenRequest,
    Session as SessionContract, UnsupportedControl,
};
use oxap_protocol::{
    CapabilityDescriptor, CapabilityLimits, EndpointDescriptor, FeatureSupport, SupportLevel,
    FEATURE_OPEN_REOPEN, PROFILE, VERSION,
};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{json, Value};

use crate::process::{Process, ProcessConfig};
use crate::reducer::Reducer;
use crate::session::Session;
use crate::{CAPABILITY_REVISION, PINNED_VERSION};

pub(crate) const MAX_QUEUED: usize = 8;
const DEFAULT_JOURNAL_CAPACITY: usize = 256;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const HEADLESS: [&str; 3] = ["--headless", "--stdio", "--no-auto-update"];
const PINNED_PROTOCOL: i64 = 3;

#[derive(Clone, Default)]
pub struct Config {
    pub executable: String,
    pub args: Vec<String>,
    pub environment: Vec<String>,
    pub working_directory: String,
    pub model: String,
    pub journal_capacity: usize,
    pub frame_limit: usize,
    pub queue_capacity: usize,
    pub request_timeout: Duration,
    pub clock: Option<Arc<dyn Clock>>,
    pub ids: Option<Arc<dyn IdGenerator>>,
}

#[derive(Clone)]
pub struct Adapter {
    config: Config,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

struct WallClock;

impl Clock for WallClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as i64)
    }
}

#[derive(Default)]
struct SequenceIds(AtomicU64);

impl IdGenerator for SequenceIds {
    fn new_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::SeqCst) + 1)
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Handshake {
    protocol_version: i64,
    version: String,
    session_id: String,
}

impl Adapter {
    pub fn new(mut config: Config) -> Result<Self, Error> {
        if config.executable.is_empty() || !Path::new(&config.working_directory).is_absolute() {
            return Err(Error::Harness(
                "copilot: executable and absolute working directory are required".into(),
            ));
        }
        if config.args.iter().any(|arg| arg == "--") {
            return Err(Error::Harness(
                "copilot: standalone -- prevents enforced headless options".into(),
            ));
        }
        config.args.extend(HEADLESS.map(String::from));
        if config.journal_capacity == 0 {
            config.journal_capacity = DEFAULT_JOURNAL_CAPACITY;
        }
        if config.request_timeout.is_zero() {
            config.request_timeout = DEFAULT_REQUEST_TIMEOUT;
        }
        let clock = config.clock.clone().unwrap_or_else(|| Arc::new(WallClock));
        let ids = config.ids.clone();
        let ids = ids.unwrap_or_else(|| Arc::new(SequenceIds::default()));
        Ok(Self { config, clock, ids })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn describe(&self) -> Descriptor {
        let feature = |level, reason: &str| FeatureSupport {
            level,
            reason: reason.into(),
            ..FeatureSupport::default()
        };
        let features: BTreeMap<String, FeatureSupport> = [
            ("protocol.initialize", feature(SupportLevel::EMULATED, "the adapter checks the server's protocol version with ping and creates the native session")),
            ("capabilities", feature(SupportLevel::EMULATED, "a conservative descriptor for the pinned headless server")),
            ("session.open", feature(SupportLevel::NATIVE, "session.create in one headless server per OAP session")),
            ("session.state", feature(SupportLevel::EMULATED, "adapter-owned projection of the runs it admitted")),
            ("session.message.submit", feature(SupportLevel::EMULATED, "session.send answers a message id at once; the run starts at its own user.message")),
            ("session.message.delivery.auto", feature(SupportLevel::EMULATED, "start when no run is open, otherwise queue behind it, as session.send enqueues")),
            ("session.message.delivery.queue", feature(SupportLevel::EMULATED, "session.send enqueues behind an open run; an explicit queue on an idle session is admitted as a reservation that Copilot promotes at once")),
            ("run.streaming", feature(SupportLevel::NATIVE, "")),
            ("run.status", feature(SupportLevel::EMULATED, "")),
            ("action.tools", feature(SupportLevel::NATIVE, "tool.execution_start and tool.execution_complete with progress between")),
            ("action.tools.execute", feature(SupportLevel::UNAVAILABLE, "Copilot executes its own tools")),
        ]
        .into_iter()
        .map(|(key, support)| (key.to_owned(), support))
        .collect();
        Descriptor {
            capabilities: CapabilityDescriptor {
                endpoint: EndpointDescriptor {
                    id: "copilot.headless".into(),
                    name: "GitHub Copilot CLI Adapter".into(),
                    version: PINNED_VERSION.clone(),
                    adapter: "copilot-headless-stdio".into(),
                },
                protocol_versions: vec![VERSION.into()],
                profiles: vec![PROFILE.into()],
                features,
                limits: Some(CapabilityLimits {
                    max_active_runs_per_session: Some(MAX_QUEUED as i64 + 1),
                    max_queued_runs_per_session: Some(MAX_QUEUED as i64),
                }),
                ..CapabilityDescriptor::default()
            },
            capability_revision: CAPABILITY_REVISION.clone(),
            journal: JournalDescriptor {
                scope: "session".into(),
                persistence: "process_memory".into(),
                replay: SupportLevel::EMULATED,
                capacity: self.config.journal_capacity,
            },
            max_active_runs_per_session: MAX_QUEUED + 1,
            interactive_gates: false,
            cancellation_target: "none".into(),
            cancellation_implementation: "unavailable".into(),
        }
    }

    pub fn admit(&self, request: &OpenRequest) -> Result<(), Error> {
        if request.reopen || !request.native_session_id.is_empty() || request.adopted {
            return Err(Error::UnsupportedControl(Box::new(
                UnsupportedControl::unadvertised(FEATURE_OPEN_REOPEN),
            )));
        }
        if request.participant.id.as_str().is_empty() {
            return Err(Error::InvalidParticipant);
        }
        refuse_unadvertised_settings(request, &self.describe().capabilities)?;
        refuse_unadvertised_tool_sources(request, &[])?;
        refuse_unadvertised_tools(request, &[])
    }

    pub fn open(&self, request: OpenRequest) -> Result<Session, Error> {
        self.admit(&request)?;
        let config = &self.config;
        let process = Process::start(&ProcessConfig {
            path: config.executable.clone(),
            args: config.args.clone(),
            dir: config.working_directory.clone(),
            env: config.environment.clone(),
            frame_limit: config.frame_limit,
            queue_capacity: config.queue_capacity,
        })
        .map_err(Error::harness)?;
        let native = match self.handshake(&process) {
            Ok(native) => native,
            Err(error) => {
                process.close();
                return Err(error);
            }
        };
        let mut id = request.session_id;
        if id.as_str().is_empty() {
            id = self.ids.new_id("session").into();
        }
        let reducer = Reducer::new(id, native, self.ids.clone(), self.clock.clone());
        let timeout = config.request_timeout;
        Ok(Session::new(
            reducer,
            process,
            config.journal_capacity,
            timeout,
        ))
    }

    fn handshake(&self, process: &Process) -> Result<String, Error> {
        let config = &self.config;
        let call = |method: &str, params: &Value| -> Result<Handshake, Error> {
            decode(process, method, params, config.request_timeout)
        };
        if call("ping", &json!({}))?.protocol_version != PINNED_PROTOCOL {
            let message = "copilot: server does not speak pinned protocol 3";
            return Err(Error::Harness(message.into()));
        }
        if call("status.get", &json!({}))?.version != *PINNED_VERSION {
            let message = "copilot: server does not report pinned CLI version";
            return Err(Error::Harness(format!("{message} {}", *PINNED_VERSION)));
        }
        let mut params = json!({
            "clientName": "open-agent-protocol",
            "workingDirectory": config.working_directory,
            "streaming": true,
            "requestPermission": false,
            "requestUserInput": false,
            "envValueMode": "direct",
        });
        if !config.model.is_empty() {
            params["model"] = json!(config.model);
        }
        let created = call("session.create", &params)?.session_id;
        if created.is_empty() {
            let message = "copilot: session.create returned no sessionId";
            return Err(Error::Harness(message.into()));
        }
        Ok(created)
    }
}

fn decode<T: DeserializeOwned + Default>(
    process: &Process,
    method: &str,
    params: &Value,
    timeout: Duration,
) -> Result<T, Error> {
    let deadline = Some(Instant::now() + timeout);
    let raw = process.client.call(method, params, deadline);
    let raw = raw.map_err(Error::harness)?;
    let text = raw.as_deref().map_or("null", RawValue::get);
    serde_json::from_str::<Option<T>>(text)
        .map(Option::unwrap_or_default)
        .map_err(|error| Error::Harness(format!("copilot: decode {method}: {error}")))
}

impl AdapterContract for Adapter {
    fn probe(&self) -> BoxFuture<'_, Descriptor> {
        let descriptor = self.describe();
        Box::pin(async move { Ok(descriptor) })
    }

    fn open(&self, request: OpenRequest) -> BoxFuture<'_, Box<dyn SessionContract>> {
        Box::pin(async move {
            let session = Adapter::open(self, request)?;
            Ok(Box::new(session) as Box<dyn SessionContract>)
        })
    }
}
