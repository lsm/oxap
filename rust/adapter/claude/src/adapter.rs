use std::collections::BTreeMap;

use oxap_adapter::{
    refuse_unadvertised_settings, refuse_unadvertised_tool_sources, refuse_unadvertised_tools,
    Adapter as AdapterContract, BoxFuture, Descriptor, Error, JournalDescriptor, OpenRequest,
    Session as SessionContract, UnsupportedControl, CONTROL_UNSATISFIABLE,
};
use oxap_protocol::{
    CapabilityDescriptor, EndpointDescriptor, FeatureSupport, SupportLevel, ToolSourceDescriptor,
    FEATURE_COMPACTION_POLICY, FEATURE_DELIVERY_QUEUE, FEATURE_DELIVERY_STEER, FEATURE_OPEN_REOPEN,
    FEATURE_RUN_COMPACTION, FEATURE_SESSION_REASONING, FEATURE_TOOLS_LIST, FEATURE_TOOL_SELECTION,
    MODE_SESSION_LIVE, MODE_SESSION_OPEN, PROFILE, SCOPE_RUN, TOOL_SOURCE_NATIVE, VERSION,
};

use crate::native::settings_flags;

pub(crate) const ENDPOINT_ID: &str = "claude-code.cli";
const NATIVE_TOOL_SOURCE: &str = "claude-code-native";
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
    pub environment: Vec<String>,
    pub working_directory: Option<String>,
    pub journal_capacity: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SpawnPlan {
    pub path: String,
    pub args: Vec<String>,
    pub dir: String,
    pub env: Vec<String>,
}

#[derive(Clone)]
pub struct Adapter {
    config: Config,
}

pub fn pin() -> &'static oxap_harness::Version {
    oxap_harness::current("claude-code").expect("embedded catalog pins claude-code")
}

impl Adapter {
    pub fn new(mut config: Config) -> Result<Self, Error> {
        config.tools.validate()?;
        if config.journal_capacity == 0 {
            config.journal_capacity = DEFAULT_JOURNAL_CAPACITY;
        }
        Ok(Self { config })
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
        refuse_unadvertised_tool_sources(request, &[])?;
        refuse_unadvertised_settings(request, &self.describe().capabilities)?;
        settings_flags(
            request.reasoning_level.clone(),
            request.compaction_policy.as_ref(),
            false,
        )?;
        if request.reopen && !valid_session_uuid(&request.native_session_id) {
            return Err(reopen_refused(
                "the binding must name a Claude Code session UUID",
            ));
        }
        Ok(())
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
                ..feature(SupportLevel::EMULATED, "enforced per call: a PreToolUse hook, and the can_use_tool gate behind it, refuse an excluded tool before it runs and the call settles refused_by_policy; not retained past the run")
            }),
        ]
        .into_iter()
        .map(|(key, support)| (key.to_owned(), support))
        .collect();
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
                features,
                sources: vec![ToolSourceDescriptor {
                    id: NATIVE_TOOL_SOURCE.into(),
                    kind: TOOL_SOURCE_NATIVE.into(),
                    display_name: "Claude Code built-in tools".into(),
                    ..ToolSourceDescriptor::default()
                }],
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
            self.admit(&request)?;
            Err(Error::Harness(
                "claude adapter: opening a session is not implemented yet".into(),
            ))
        })
    }
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
