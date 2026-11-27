use std::collections::BTreeMap;
use std::path::Path;

use oxap_adapter::{
    refuse_unadvertised_settings, refuse_unadvertised_tool_sources, refuse_unadvertised_tools,
    Adapter as AdapterContract, BoxFuture, Descriptor, Error, JournalDescriptor, OpenRequest,
    Session as SessionContract, UnsupportedControl, CONTROL_UNSATISFIABLE,
};
use oxap_protocol::{
    CapabilityDescriptor, CompactionKind, EndpointDescriptor, FeatureSupport, SupportLevel,
    FEATURE_COMPACTION_POLICY, FEATURE_DELIVERY_QUEUE, FEATURE_DELIVERY_STEER, FEATURE_OPEN_REOPEN,
    FEATURE_RUN_COMPACTION, FEATURE_SESSION_COMPACT, FEATURE_SESSION_REASONING, MODE_SESSION_LIVE,
    MODE_SESSION_OPEN, PROFILE, VERSION,
};

use crate::process::ProcessConfig;

const ENDPOINT_ID: &str = "pi.rpc";
const DEFAULT_JOURNAL_CAPACITY: usize = 256;

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub executable: String,
    pub args: Vec<String>,
    pub environment: Option<Vec<String>>,
    pub working_directory: String,
    pub journal_capacity: usize,
}

#[derive(Clone, Debug)]
pub struct Adapter {
    config: Config,
}

pub fn pin() -> &'static oxap_harness::Version {
    oxap_harness::current("pi").expect("embedded catalog pins pi")
}

fn refused(detail: &str) -> Error {
    Error::Harness(format!("pi adapter: {detail}"))
}

impl Adapter {
    pub fn new(mut config: Config) -> Result<Self, Error> {
        for arg in &config.args {
            if arg == "--" {
                return Err(refused(
                    "standalone -- prevents enforced extension disabling",
                ));
            }
            if arg == "--extension" || arg == "-e" || arg.starts_with("--extension=") {
                return Err(refused(
                    "explicit extensions are incompatible with canonical prompt admission",
                ));
            }
        }
        if config.executable.is_empty() {
            return Err(refused("factory or executable is required"));
        }
        if !Path::new(&config.working_directory).is_absolute() {
            return Err(refused("absolute working directory is required"));
        }
        if config.journal_capacity == 0 {
            config.journal_capacity = DEFAULT_JOURNAL_CAPACITY;
        }
        Ok(Self { config })
    }

    pub fn process_config(&self) -> ProcessConfig {
        let mut args = self.config.args.clone();
        args.push("--no-extensions".to_owned());
        ProcessConfig {
            path: self.config.executable.clone(),
            args,
            dir: self.config.working_directory.clone(),
            env: self.config.environment.clone(),
            ..ProcessConfig::default()
        }
    }

    pub fn admit(&self, request: &OpenRequest) -> Result<(), Error> {
        refuse_unadvertised_tool_sources(request, &[])?;
        refuse_unadvertised_tools(request, &[])?;
        if request.participant.id.as_str().is_empty() {
            return Err(Error::InvalidParticipant);
        }
        refuse_unadvertised_settings(request, &self.describe().capabilities)?;
        match &request.compaction_policy {
            Some(policy)
                if policy.kind != CompactionKind::AUTO && policy.kind != CompactionKind::OFF =>
            {
                Err(Error::UnsupportedControl(Box::new(UnsupportedControl {
                    feature: FEATURE_COMPACTION_POLICY.to_owned(),
                    reason: CONTROL_UNSATISFIABLE,
                    field: "compaction_policy".to_owned(),
                    detail: "Pi switches its own compaction on or off; its threshold lives in a settings file".to_owned(),
                    ..UnsupportedControl::default()
                })))
            }
            _ => Ok(()),
        }
    }

    pub fn describe(&self) -> Descriptor {
        let feature = |level, reason: &str| FeatureSupport {
            level,
            reason: reason.into(),
            ..FeatureSupport::default()
        };
        let opening = |level, reason: &str| FeatureSupport {
            modes: vec![MODE_SESSION_OPEN.into(), MODE_SESSION_LIVE.into()],
            ..feature(level, reason)
        };
        let features: BTreeMap<String, FeatureSupport> = [
            ("protocol.initialize", feature(SupportLevel::EMULATED, "Pi has no negotiation; readiness is a get_state handshake")),
            ("capabilities", feature(SupportLevel::EMULATED, "conservative descriptor synthesized for the pinned RPC vocabulary")),
            (FEATURE_OPEN_REOPEN, feature(SupportLevel::NATIVE, "switch_session loads the bound session file; an absent, empty or mismatched file is refused")),
            ("session.open", feature(SupportLevel::EMULATED, "one ready Pi process is associated with one OAP session")),
            ("session.state", feature(SupportLevel::EMULATED, "adapter projection reconciled with get_state")),
            ("session.message.submit", feature(SupportLevel::EMULATED, "successful prompt response proves admission only")),
            ("session.message.delivery.auto", feature(SupportLevel::EMULATED, "idle auto is normalized to native prompt/start")),
            (FEATURE_DELIVERY_QUEUE, feature(SupportLevel::UNAVAILABLE, "v0.1 admission cannot expose Pi queued prompt semantics safely")),
            (FEATURE_DELIVERY_STEER, feature(SupportLevel::EMULATED, "guidance rides Pi's native steer command and settles at the turn boundary Pi injects it")),
            ("run.streaming", feature(SupportLevel::NATIVE, "")),
            ("run.status", feature(SupportLevel::EMULATED, "")),
            ("run.cancel", feature(SupportLevel::DEGRADED, "abort intent is local; agent_settled remains terminal authority")),
            ("run.resume", feature(SupportLevel::DEGRADED, "bounded process-memory replay")),
            ("run.reconciliation", feature(SupportLevel::EMULATED, "get_state reconciles streaming state")),
            ("run.replay", feature(SupportLevel::DEGRADED, "bounded adapter journal; gaps explicit")),
            ("action.tools", feature(SupportLevel::DEGRADED, "observed tool lifecycle only; no portable catalog")),
            ("action.tools.execute", feature(SupportLevel::UNAVAILABLE, "Pi executes tools internally")),
            ("action.permissions", feature(SupportLevel::UNAVAILABLE, "extension dialogs are generic user input, not permissions")),
            (FEATURE_SESSION_REASONING, opening(SupportLevel::NATIVE, "set_thinking_level once the process is ready and again between runs, confirmed by get_state; a level Pi does not run the model at is refused, and a live change restores the level it replaced")),
            (FEATURE_SESSION_COMPACT, feature(SupportLevel::NATIVE, "a compaction request on an idle session runs Pi's compact command, with focus as its custom instructions; Pi never continues the turn, so continue is refused")),
            (FEATURE_RUN_COMPACTION, feature(SupportLevel::NATIVE, "Pi's compaction_start and compaction_end inside a prompt run, threshold and overflow alike, become the run's compaction events; it compacts before agent_settled, so the run is still open")),
            (FEATURE_COMPACTION_POLICY, opening(SupportLevel::NATIVE, "set_auto_compaction switches Pi's own threshold on or off, at open and between runs; its threshold is a settings-file reserve, so share and tokens are refused")),
        ]
        .into_iter()
        .map(|(key, support)| (key.to_owned(), support))
        .collect();
        Descriptor {
            capabilities: CapabilityDescriptor {
                endpoint: EndpointDescriptor {
                    id: ENDPOINT_ID.into(),
                    name: "Pi RPC Adapter".into(),
                    version: pin().endpoint_version.clone(),
                    adapter: "pi-rpc-stdio".into(),
                },
                protocol_versions: vec![VERSION.into()],
                profiles: vec![PROFILE.into()],
                features,
                ..CapabilityDescriptor::default()
            },
            capability_revision: pin().capability_revision.clone(),
            journal: JournalDescriptor {
                scope: "session".into(),
                persistence: "process_memory".into(),
                replay: SupportLevel::DEGRADED,
                capacity: self.config.journal_capacity,
            },
            max_active_runs_per_session: 1,
            interactive_gates: false,
            cancellation_target: "run".into(),
            cancellation_implementation: "native_abort_with_agent_settled_authority".into(),
        }
    }
}

impl AdapterContract for Adapter {
    fn probe(&self) -> BoxFuture<'_, Descriptor> {
        Box::pin(async { Ok(self.describe()) })
    }

    fn open(&self, request: OpenRequest) -> BoxFuture<'_, Box<dyn SessionContract>> {
        Box::pin(async move {
            self.admit(&request)?;
            Err(refused("opening a session is not implemented yet"))
        })
    }
}
