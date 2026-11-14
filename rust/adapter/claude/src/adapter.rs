use std::collections::BTreeMap;

use oxap_adapter::{
    Adapter as AdapterContract, BoxFuture, Descriptor, Error, JournalDescriptor, OpenRequest,
    Session,
};
use oxap_protocol::{
    CapabilityDescriptor, EndpointDescriptor, FeatureSupport, SupportLevel, ToolSourceDescriptor,
    FEATURE_COMPACTION_POLICY, FEATURE_OPEN_REOPEN, FEATURE_SESSION_REASONING, FEATURE_TOOLS_LIST,
    FEATURE_TOOL_SELECTION, MODE_SESSION_LIVE, MODE_SESSION_OPEN, PROFILE, SCOPE_RUN,
    TOOL_SOURCE_NATIVE, VERSION,
};

pub const ENDPOINT_ID: &str = "claude-code.cli";
pub const ADAPTER_NAME: &str = "claude-code-stream-json";
pub const DISPLAY_NAME: &str = "Claude Code Adapter";
pub const NATIVE_TOOL_SOURCE: &str = "claude-code-native";
pub const DEFAULT_JOURNAL_CAPACITY: usize = 256;

pub fn pin() -> &'static oxap_harness::Version {
    oxap_harness::current("claude-code")
        .expect("embedded Claude Code catalog has a current version")
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub journal_capacity: usize,
}

pub struct Adapter {
    descriptor: Descriptor,
}

impl Adapter {
    pub fn new(mut config: Config) -> Result<Self, Error> {
        if config.journal_capacity == 0 {
            config.journal_capacity = DEFAULT_JOURNAL_CAPACITY;
        }
        let descriptor = describe(&config);
        Ok(Self { descriptor })
    }

    pub fn describe(&self) -> Descriptor {
        self.descriptor.clone()
    }
}

fn support(level: SupportLevel, reason: &str) -> FeatureSupport {
    FeatureSupport {
        level,
        reason: reason.to_owned(),
        ..FeatureSupport::default()
    }
}

fn features() -> BTreeMap<String, FeatureSupport> {
    let native = |reason: &str| support(SupportLevel::NATIVE, reason);
    let emulated = |reason: &str| support(SupportLevel::EMULATED, reason);
    let degraded = |reason: &str| support(SupportLevel::DEGRADED, reason);
    let unavailable = |reason: &str| support(SupportLevel::UNAVAILABLE, reason);
    let live = |support: FeatureSupport| FeatureSupport {
        modes: vec![MODE_SESSION_OPEN.to_owned(), MODE_SESSION_LIVE.to_owned()],
        ..support
    };
    [
        ("protocol.initialize", emulated("initialize control exchange at open; no capability negotiation")),
        ("capabilities", emulated("conservative descriptor; per-turn system/init refresh recorded as evidence")),
        ("session.open", emulated("process spawn + initialize; CLI session UUID observed on frames")),
        (FEATURE_OPEN_REOPEN, native("--resume reloads the bound conversation; get_settings reports the loader model and effort, not persisted configuration")),
        ("session.state", degraded("reducer-owned live projection")),
        (FEATURE_SESSION_REASONING, live(native("apply_flag_settings sets effortLevel after initialize and again between runs; low through max, and the CLI has no off or minimal level, so those are refused"))),
        (FEATURE_COMPACTION_POLICY, live(emulated("apply_flag_settings sets autoCompactEnabled and autoCompactWindow, at open and between runs; tokens is the window the CLI compacts within, and share is refused"))),
        ("session.message.submit", degraded("host-minted turn uuid correlated by the user_message_uuid echo")),
        ("session.message.delivery.auto", degraded("accepted only when the CLI session is idle")),
        ("session.message.delivery.queue", unavailable("queued continuation turns are not exposed in v1")),
        ("session.message.delivery.steer", unavailable("shouldQuery/priority are unexercised")),
        ("run.compaction", native("system/status compacting opens a compaction and the status carrying compact_result closes it, completed or failed with compact_error; the CLI does not say why it compacts, so the reason is threshold for an overflow too, and one still open at the terminal ends failed, or cancelled for a cancel")),
        ("run.streaming", native("stream_event deltas with --include-partial-messages always on")),
        ("run.status", emulated("")),
        ("run.cancel", degraded("interrupt intent; settlement only via terminal_reason aborted_*")),
        ("run.resume", degraded("OAP run resume replays the adapter journal; conversation reload uses session.open.reopen")),
        ("run.replay", degraded("bounded adapter journal; gaps are explicit and transcript persistence is not event replay")),
        ("run.reconciliation", degraded("system/init and session state frames corroborate")),
        ("action.tools", degraded("tool_use/tool_result projection; started synthesized; tool_progress observed-only")),
        ("action.tools.execute", unavailable("the CLI executes tools internally")),
        (FEATURE_TOOLS_LIST, degraded("system/init republishes the tool and MCP server lists per turn; there is none before the first")),
        ("action.permissions", native("can_use_tool reverse control requests")),
        ("user_input", native("permission gates over the control plane")),
        (FEATURE_TOOL_SELECTION, FeatureSupport {
            scope: SCOPE_RUN.to_owned(),
            ..emulated("enforced per call: a PreToolUse hook, and the can_use_tool gate behind it, refuse an excluded tool before it runs and the call settles refused_by_policy; not retained past the run")
        }),
    ]
    .into_iter()
    .map(|(key, support)| (key.to_owned(), support))
    .collect()
}

fn describe(config: &Config) -> Descriptor {
    let version = pin();
    Descriptor {
        capabilities: CapabilityDescriptor {
            endpoint: EndpointDescriptor {
                id: ENDPOINT_ID.into(),
                name: DISPLAY_NAME.to_owned(),
                version: version.endpoint_version.clone(),
                adapter: ADAPTER_NAME.to_owned(),
            },
            protocol_versions: vec![VERSION.to_owned()],
            profiles: vec![PROFILE.to_owned()],
            features: features(),
            sources: vec![ToolSourceDescriptor {
                id: NATIVE_TOOL_SOURCE.to_owned(),
                kind: TOOL_SOURCE_NATIVE.to_owned(),
                display_name: "Claude Code built-in tools".to_owned(),
                ..ToolSourceDescriptor::default()
            }],
            ..CapabilityDescriptor::default()
        },
        capability_revision: version.capability_revision.clone(),
        journal: JournalDescriptor {
            scope: "session".to_owned(),
            persistence: "process_memory".to_owned(),
            replay: SupportLevel::DEGRADED,
            capacity: config.journal_capacity,
        },
        max_active_runs_per_session: 1,
        interactive_gates: true,
        cancellation_target: "session".to_owned(),
        cancellation_implementation: "interrupt control request".to_owned(),
    }
}

impl AdapterContract for Adapter {
    fn probe(&self) -> BoxFuture<'_, Descriptor> {
        Box::pin(async { Ok(self.describe()) })
    }

    fn open(&self, _request: OpenRequest) -> BoxFuture<'_, Box<dyn Session>> {
        Box::pin(async {
            Err(Error::Harness(
                "claude adapter: the session runtime is not ported yet".into(),
            ))
        })
    }
}
