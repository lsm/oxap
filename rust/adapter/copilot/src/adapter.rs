use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use oxap_adapter::{
    refuse_unadvertised_settings, refuse_unadvertised_tool_sources, refuse_unadvertised_tools,
    Descriptor, Error, JournalDescriptor, OpenRequest, UnsupportedControl,
};
use oxap_protocol::{
    CapabilityDescriptor, CapabilityLimits, EndpointDescriptor, FeatureSupport, SupportLevel,
    FEATURE_OPEN_REOPEN, PROFILE, VERSION,
};

use crate::{CAPABILITY_REVISION, PINNED_VERSION};

pub(crate) const MAX_QUEUED: usize = 8;
const DEFAULT_JOURNAL_CAPACITY: usize = 256;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const HEADLESS: [&str; 3] = ["--headless", "--stdio", "--no-auto-update"];

#[derive(Clone, Debug, Default)]
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
}

#[derive(Clone, Debug)]
pub struct Adapter {
    config: Config,
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
        Ok(Self { config })
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
}
