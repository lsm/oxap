use std::{collections::BTreeMap, fmt};

use oxap_adapter::runtime::Runtime;
use oxap_adapter::{
    refuse_unadvertised_settings, refuse_unadvertised_tool_sources, refuse_unadvertised_tools,
    Adapter as AdapterContract, BoxFuture, Descriptor, Error, JournalDescriptor, OpenRequest,
    Session as SessionContract, UnsupportedControl, CONTROL_UNSATISFIABLE,
};
use oxap_protocol::{
    CapabilityDescriptor, EndpointDescriptor, FeatureSupport, SupportLevel,
    FEATURE_COMPACTION_POLICY, FEATURE_DELIVERY_QUEUE, FEATURE_DELIVERY_STEER, FEATURE_OPEN_REOPEN,
    FEATURE_RUN_COMPACTION, FEATURE_SESSION_REASONING, FEATURE_TOOLS_LIST, FEATURE_TOOL_SELECTION,
    MODE_SESSION_LIVE, MODE_SESSION_OPEN, PROFILE, SCOPE_RUN, VERSION,
};
use oxap_protocol::{SessionState, SessionStatus};

use crate::{
    catalog::{self, native_source},
    native::{apply_flag_settings_request, initialize_request, settings_flags, FlagSettings},
    provider::{configure_environment, Header, Provider},
    rpc::{Process, ProcessConfig},
    session::{bind, OpenedSession},
};

pub(crate) const ENDPOINT_ID: &str = "claude-code.cli";
const DEFAULT_JOURNAL_CAPACITY: usize = 256;

#[derive(Clone, Debug, Default)]
pub enum ToolPosture {
    #[default]
    Unstated,
    Unrestricted,
    Allow(Vec<String>),
}

impl ToolPosture {
    pub fn allow(names: &[String]) -> Self {
        Self::Allow(names.to_vec())
    }

    fn validate(&self) -> Result<(), Error> {
        let Self::Allow(allowed) = self else {
            return match self {
                Self::Unstated => Err(Error::Harness(
                    "claude adapter: Config.tools must state a posture: ToolPosture::allow or ToolPosture::unrestricted"
                        .into(),
                )),
                _ => Ok(()),
            };
        };
        if allowed.is_empty() {
            return Err(Error::Harness(
                "claude adapter: ToolPosture::allow names no tool; name at least one, or state ToolPosture::unrestricted"
                    .into(),
            ));
        }
        for name in allowed {
            if name.is_empty() {
                return Err(Error::Harness(
                    "claude adapter: ToolPosture::allow names an empty tool".into(),
                ));
            }
            if tool_of(name).is_empty() {
                return Err(Error::Harness(format!(
                    "claude adapter: ToolPosture::allow rule {name:?} names no tool"
                )));
            }
        }
        Ok(())
    }

    fn argv(&self) -> Vec<String> {
        let Self::Allow(allowed) = self else {
            return Vec::new();
        };
        let mut surface: Vec<&str> = Vec::new();
        for rule in allowed {
            let tool = tool_of(rule);
            if !surface.contains(&tool) {
                surface.push(tool);
            }
        }
        let mut argv = vec!["--tools".to_owned(), surface.join(",")];
        argv.push("--allowedTools".to_owned());
        argv.extend(allowed.iter().cloned());
        argv
    }
}

fn tool_of(rule: &str) -> &str {
    rule.split_once('(').map_or(rule, |(tool, _)| tool).trim()
}

#[derive(Clone, Default)]
pub struct Config {
    pub executable: Option<String>,
    pub model: Option<String>,
    pub tools: ToolPosture,
    pub expand_prompts: bool,
    pub environment: Vec<String>,
    pub provider: Option<Provider>,
    pub headers: Vec<Header>,
    pub working_directory: Option<String>,
    pub journal_capacity: usize,
}

#[derive(Clone, Default)]
pub struct SpawnPlan {
    pub path: String,
    pub args: Vec<String>,
    pub dir: String,
    pub env: Vec<String>,
}

impl fmt::Debug for SpawnPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SpawnPlan")
            .field("path", &self.path)
            .field("args", &self.args)
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct Adapter {
    config: Config,
    runtime: Runtime,
}

pub fn pin() -> &'static oxap_harness::Version {
    oxap_harness::current("claude-code").expect("embedded catalog pins claude-code")
}

impl Adapter {
    pub fn new(mut config: Config) -> Result<Self, Error> {
        configure_environment(
            &mut config.environment,
            config.provider.take(),
            &config.headers,
        )?;
        config.tools.validate()?;
        if config.journal_capacity == 0 {
            config.journal_capacity = DEFAULT_JOURNAL_CAPACITY;
        }
        Ok(Self {
            config,
            runtime: Runtime::default(),
        })
    }

    pub fn spawn_plan(&self) -> Result<SpawnPlan, Error> {
        let Some(path) = self.config.executable.clone() else {
            return Err(Error::Harness(
                "claude adapter: an executable is required to spawn Claude Code".into(),
            ));
        };
        let mut args = vec![
            "--output-format".to_owned(),
            "stream-json".to_owned(),
            "--verbose".to_owned(),
            "--input-format".to_owned(),
            "stream-json".to_owned(),
            "--system-prompt".to_owned(),
            String::new(),
            "--include-partial-messages".to_owned(),
            "--permission-prompt-tool".to_owned(),
            "stdio".to_owned(),
            "--setting-sources=".to_owned(),
        ];
        if let Some(model) = self
            .config
            .model
            .as_deref()
            .filter(|model| !model.is_empty())
        {
            args.push("--model".to_owned());
            args.push(model.to_owned());
        }
        args.extend(self.config.tools.argv());
        Ok(SpawnPlan {
            path,
            args,
            dir: self.config.working_directory.clone().unwrap_or_default(),
            env: self.config.environment.clone(),
        })
    }

    pub fn admit(&self, request: &OpenRequest) -> Result<(), Error> {
        refuse_unadvertised_tools(request, &[])?;
        self.admit_flags(request).map(|_| ())
    }

    fn admit_flags(&self, request: &OpenRequest) -> Result<FlagSettings<'static>, Error> {
        refuse_unadvertised_tool_sources(request, &[])?;
        refuse_unadvertised_settings(request, &self.describe().capabilities)?;
        let flags = settings_flags(
            request.reasoning_level.clone(),
            request.compaction_policy.as_ref(),
            false,
        )?;
        if request.reopen && !valid_session_uuid(&request.native_session_id) {
            return Err(reopen_refused(
                "the binding must name a Claude Code session UUID",
            ));
        }
        Ok(flags)
    }

    pub(crate) fn features() -> BTreeMap<String, FeatureSupport> {
        let feature = |level, reason: &str| FeatureSupport {
            level,
            reason: reason.into(),
            ..FeatureSupport::default()
        };
        let opening = |level, reason: &str| FeatureSupport {
            modes: vec![MODE_SESSION_OPEN.into(), MODE_SESSION_LIVE.into()],
            ..feature(level, reason)
        };
        [
            ("protocol.initialize", feature(SupportLevel::EMULATED, "initialize control exchange at open; no capability negotiation")),
            ("capabilities", feature(SupportLevel::EMULATED, "conservative descriptor; per-turn system/init refresh recorded as evidence")),
            ("session.open", feature(SupportLevel::EMULATED, "process spawn + initialize; CLI session UUID observed on frames")),
            (FEATURE_OPEN_REOPEN, feature(SupportLevel::NATIVE, "--resume reloads the bound conversation; get_settings reports the loader model and effort, not persisted configuration")),
            ("session.state", feature(SupportLevel::DEGRADED, "reducer-owned live projection")),
            (FEATURE_SESSION_REASONING, opening(SupportLevel::NATIVE, "apply_flag_settings sets effortLevel after initialize and again between runs; low through max, and the CLI has no off or minimal level, so those are refused")),
            (FEATURE_COMPACTION_POLICY, opening(SupportLevel::EMULATED, "apply_flag_settings sets autoCompactEnabled and autoCompactWindow, at open and between runs; tokens is the window the CLI compacts within, and share is refused")),
            ("session.message.submit", feature(SupportLevel::DEGRADED, "host-minted turn uuid correlated by the user_message_uuid echo")),
            ("session.message.delivery.auto", feature(SupportLevel::DEGRADED, "accepted only when the CLI session is idle")),
            (FEATURE_DELIVERY_QUEUE, feature(SupportLevel::UNAVAILABLE, "queued continuation turns are not exposed in v1")),
            (FEATURE_DELIVERY_STEER, feature(SupportLevel::UNAVAILABLE, "shouldQuery/priority are unexercised")),
            (FEATURE_RUN_COMPACTION, feature(SupportLevel::NATIVE, "system/status compacting opens a compaction and the status carrying compact_result closes it, completed or failed with compact_error; the CLI does not say why it compacts, so the reason is threshold for an overflow too, and one still open at the terminal ends failed, or cancelled for a cancel")),
            ("run.streaming", feature(SupportLevel::NATIVE, "stream_event deltas with --include-partial-messages always on")),
            ("run.status", feature(SupportLevel::EMULATED, "")),
            ("run.cancel", feature(SupportLevel::DEGRADED, "interrupt intent; settlement only via terminal_reason aborted_*")),
            ("run.resume", feature(SupportLevel::DEGRADED, "OAP run resume replays the adapter journal; conversation reload uses session.open.reopen")),
            ("run.replay", feature(SupportLevel::DEGRADED, "bounded adapter journal; gaps are explicit and transcript persistence is not event replay")),
            ("run.reconciliation", feature(SupportLevel::DEGRADED, "system/init and session state frames corroborate")),
            ("action.tools", feature(SupportLevel::DEGRADED, "tool_use/tool_result projection; started synthesized; tool_progress observed-only")),
            ("action.tools.execute", feature(SupportLevel::UNAVAILABLE, "the CLI executes tools internally")),
            (FEATURE_TOOLS_LIST, feature(SupportLevel::DEGRADED, "system/init republishes the tool and MCP server lists per turn; there is none before the first")),
            ("action.permissions", feature(SupportLevel::NATIVE, "can_use_tool reverse control requests")),
            ("user_input", feature(SupportLevel::NATIVE, "permission gates over the control plane")),
            (FEATURE_TOOL_SELECTION, FeatureSupport {
                scope: SCOPE_RUN.into(),
                ..feature(SupportLevel::EMULATED, "tool_choice is enforced by the pre-tool-use hook and permission gate")
            }),
        ]
        .into_iter()
        .map(|(key, support)| (key.to_owned(), support))
        .collect()
    }

    pub fn describe(&self) -> Descriptor {
        Descriptor {
            capabilities: CapabilityDescriptor {
                endpoint: EndpointDescriptor {
                    id: ENDPOINT_ID.into(),
                    name: "Claude Code Adapter".into(),
                    version: pin().endpoint_version.clone(),
                    adapter: "claude-code-stream-json".into(),
                },
                protocol_versions: vec![VERSION.into()],
                profiles: vec![PROFILE.into()],
                features: Self::features(),
                sources: vec![native_source()],
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
            interactive_gates: true,
            cancellation_target: "session".into(),
            cancellation_implementation: "interrupt control request".into(),
        }
    }
}

impl AdapterContract for Adapter {
    fn probe(&self) -> BoxFuture<'_, Descriptor> {
        Box::pin(async { Ok(self.describe()) })
    }

    fn open(&self, request: OpenRequest) -> BoxFuture<'_, Box<dyn SessionContract>> {
        Box::pin(async move {
            refuse_unadvertised_tools(&request, &[])?;
            self.open_staged(request).await
        })
    }
}

impl Adapter {
    fn open_staged(&self, mut request: OpenRequest) -> BoxFuture<'_, Box<dyn SessionContract>> {
        Box::pin(async move {
            catalog::admit(&mut request.tools, &request.participant.id)?;
            let flags = self.admit_flags(&request)?;
            if request.reopen {
                return Err(reopen_refused(
                    "reopening requires native settings recovery",
                ));
            }
            let mut launch = self.spawn_plan()?;
            let server = (!request.tools.is_empty()).then(|| catalog::SDK_SERVER.to_owned());
            if server.is_some() {
                if matches!(self.config.tools, ToolPosture::Unrestricted) {
                    launch.args.push("--allowedTools".into());
                }
                for tool in &request.tools {
                    launch
                        .args
                        .push(format!("{}{}", catalog::SDK_PREFIX, tool.name));
                }
            }
            let initialize = initialize_request(server.as_slice()).map_err(Error::harness)?;
            let id = native_uuid()?;
            launch.args.extend(["--session-id".into(), id.clone()]);
            let process = Process::start(
                &ProcessConfig {
                    path: launch.path,
                    args: launch.args,
                    dir: launch.dir,
                    env: Some(launch.env),
                    ..ProcessConfig::default()
                },
                None,
            )
            .map_err(Error::harness)?;
            let mut state = SessionState {
                session_id: if request.session_id.as_str().is_empty() {
                    self.runtime.new_id("session").into()
                } else {
                    request.session_id
                },
                status: SessionStatus::IDLE,
                current_model_id: self.config.model.clone().unwrap_or_default(),
                updated_at_ms: self.runtime.now_ms(),
                reasoning_level: request.reasoning_level,
                compaction_policy: request.compaction_policy,
                ..SessionState::default()
            };
            bind(&mut state, &id);
            let session = OpenedSession::new(
                process,
                state,
                request.participant.id,
                self.config.expand_prompts,
                self.config.journal_capacity,
                self.runtime.clone(),
                request.tools,
            )?;
            session.exchange("initialize exchange", initialize)?;
            if flags != FlagSettings::default() {
                let settings = apply_flag_settings_request(&flags).map_err(Error::harness)?;
                session.exchange("apply_flag_settings", settings)?;
            }
            Ok(Box::new(session) as Box<dyn SessionContract>)
        })
    }
}

pub(crate) fn native_uuid() -> Result<String, Error> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(Error::harness)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] = bytes;
    Ok(format!("{a:02x}{b:02x}{c:02x}{d:02x}-{e:02x}{f:02x}-{g:02x}{h:02x}-{i:02x}{j:02x}-{k:02x}{l:02x}{m:02x}{n:02x}{o:02x}{p:02x}"))
}

fn reopen_refused(detail: &str) -> Error {
    Error::UnsupportedControl(Box::new(UnsupportedControl {
        feature: FEATURE_OPEN_REOPEN.to_owned(),
        reason: CONTROL_UNSATISFIABLE,
        detail: detail.into(),
        ..UnsupportedControl::default()
    }))
}

fn valid_session_uuid(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        let ok = match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        };
        if !ok {
            return false;
        }
    }
    true
}

#[cfg(all(test, unix))]
#[path = "../tests/support/provided_discovery.rs"]
mod provided_discovery;
