use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use futures_channel::oneshot;
use oxap_adapter::{
    refuse_unadvertised_settings, refuse_unadvertised_tool_sources, refuse_unadvertised_tools,
    Adapter as AdapterContract, BoxFuture, Descriptor, Error, JournalDescriptor, OpenRequest,
    Session as SessionContract, UnsupportedControl, CONTROL_UNSATISFIABLE,
};
use oxap_protocol::{
    CapabilityDescriptor, CompactionKind, CompactionPolicy, EndpointDescriptor, FeatureSupport,
    ReasoningLevel, RecoveryMetadata, SessionState, SessionStatus, SupportLevel,
    FEATURE_COMPACTION_POLICY, FEATURE_OPEN_REOPEN, FEATURE_SESSION_MODEL_SWITCH,
    FEATURE_SESSION_REASONING, MODE_SESSION_OPEN, PROFILE, VERSION,
};

use crate::native::{
    validate_initialize_params, InitializeParams, SessionResumeParams, METHOD_SESSION_RESUME,
};
use crate::process::{Process, ProcessConfig};
use crate::rpc::Inbound;
use crate::runtime::Runtime;
use crate::session::Session;

pub(crate) const ENDPOINT_ID: &str = "deepseek.harness";
const DEFAULT_JOURNAL_CAPACITY: usize = 256;
const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(30);
const REOPEN_SUPPORT_REASON: &str = "session/resume reloads the bound stored session after initialize, under the initialized model route";
const REOPEN_RECOVERY_REASON: &str =
    "the harness restored the bound conversation; OAP runs and cursors remain process-local";

static PATCHES: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub process: ProcessConfig,
    pub provider: String,
    pub model: String,
    pub max_tokens: Option<i64>,
    pub journal_capacity: usize,
    pub runtime: Runtime,
    pub start_timeout: Duration,
    pub native_session_id: Option<fn(&str) -> String>,
}

#[derive(Clone, Debug)]
pub struct Launch {
    pub process: ProcessConfig,
    pub patch: Option<String>,
}

#[derive(Clone)]
pub struct Adapter {
    config: Config,
}

pub fn pin() -> &'static oxap_harness::Version {
    oxap_harness::current("deepseek-harness").expect("embedded catalog pins deepseek-harness")
}

fn harness(text: &str) -> Error {
    Error::Harness(text.into())
}

pub(crate) fn unsatisfiable(feature: &str, field: &str, detail: &str) -> Error {
    Error::UnsupportedControl(Box::new(UnsupportedControl {
        feature: feature.into(),
        reason: CONTROL_UNSATISFIABLE,
        field: field.into(),
        detail: detail.into(),
        ..UnsupportedControl::default()
    }))
}

impl Adapter {
    pub fn new(mut config: Config) -> Result<Self, Error> {
        if config.process.path.is_empty() {
            return Err(harness(
                "deepseek adapter: factory or executable is required",
            ));
        }
        if !Path::new(&config.process.dir).is_absolute() {
            return Err(harness(
                "deepseek adapter: absolute working directory is required",
            ));
        }
        config.process.initialize = InitializeParams {
            cwd: config.process.dir.clone(),
            provider: config.provider.clone(),
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            reasoning_effort: String::new(),
        };
        validate_initialize_params(&config.process.initialize).map_err(|text| harness(&text))?;
        if config.journal_capacity == 0 {
            config.journal_capacity = DEFAULT_JOURNAL_CAPACITY;
        }
        if config.start_timeout.is_zero() {
            config.start_timeout = DEFAULT_START_TIMEOUT;
        }
        Ok(Self { config })
    }

    pub fn describe(&self) -> Descriptor {
        let feature = |level, reason: &str| FeatureSupport {
            level,
            reason: reason.into(),
            ..FeatureSupport::default()
        };
        let opening = |level, reason: &str| FeatureSupport {
            modes: vec![MODE_SESSION_OPEN.into()],
            ..feature(level, reason)
        };
        let features: BTreeMap<String, FeatureSupport> = [
            ("protocol.initialize", feature(SupportLevel::EMULATED, "adapter-owned one-shot initialization freeze")),
            ("capabilities", feature(SupportLevel::EMULATED, "conservative descriptor for pinned SDK wire")),
            ("session.open", feature(SupportLevel::EMULATED, "one process and native session per OAP session")),
            (FEATURE_OPEN_REOPEN, feature(SupportLevel::NATIVE, REOPEN_SUPPORT_REASON)),
            ("session.state", feature(SupportLevel::DEGRADED, "reducer-owned live projection; no native query")),
            (FEATURE_SESSION_MODEL_SWITCH, feature(SupportLevel::EMULATED, "the runtime freezes its model at initialize, so a session that has not run restarts its own runtime on the new model; once it has run, a switch is refused")),
            (FEATURE_SESSION_REASONING, opening(SupportLevel::NATIVE, "initialize carries reasoningEffort for the runtime this session starts; the DeepSeek route takes off, low, high and max")),
            (FEATURE_COMPACTION_POLICY, opening(SupportLevel::EMULATED, "a --patch layer on this session's runtime sets compaction-basic's thresholdRatio for share, or disables it for off; its headroom still caps the threshold, and tokens is refused")),
            ("session.message.submit", feature(SupportLevel::DEGRADED, "receipt plus entered direct-user message proves start")),
            ("session.message.delivery.auto", feature(SupportLevel::DEGRADED, "accepted only for known idle sessions and normalized to start")),
            ("session.message.delivery.queue", feature(SupportLevel::UNAVAILABLE, "overlapping native followups are outside the adapter contract")),
            ("session.message.delivery.steer", feature(SupportLevel::UNAVAILABLE, "selected SDK wire has no steer request")),
            ("run.streaming", feature(SupportLevel::NATIVE, "")),
            ("run.status", feature(SupportLevel::EMULATED, "")),
            ("run.cancel", feature(SupportLevel::UNAVAILABLE, "selected SDK wire has no cancel request")),
            ("run.resume", feature(SupportLevel::DEGRADED, "selected SDK wire has no resume request; OAP resume replays the adapter journal")),
            ("run.reconciliation", feature(SupportLevel::DEGRADED, "live status corroboration only")),
            ("run.replay", feature(SupportLevel::DEGRADED, "bounded adapter journal; gaps are explicit and there is no native replay request")),
            ("action.tools", feature(SupportLevel::DEGRADED, "call/result only; started is synthesized")),
            ("action.tools.execute", feature(SupportLevel::UNAVAILABLE, "Harness executes tools internally")),
            ("action.permissions", feature(SupportLevel::UNAVAILABLE, "selected SDK wire has no interaction channel")),
        ]
        .into_iter()
        .map(|(key, support)| (key.to_owned(), support))
        .collect();
        Descriptor {
            capabilities: CapabilityDescriptor {
                endpoint: EndpointDescriptor {
                    id: ENDPOINT_ID.into(),
                    name: "DeepSeek Harness SDK Adapter".into(),
                    version: pin().endpoint_version.clone(),
                    adapter: "deepseek-harness-jsonrpc".into(),
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
            cancellation_target: "none".into(),
            cancellation_implementation: "unavailable".into(),
        }
    }

    pub fn launch(&self, request: &OpenRequest) -> Result<Launch, Error> {
        let mut process = self.config.process.clone();
        let level = &request.reasoning_level;
        if !level.is_empty()
            && ![
                ReasoningLevel::OFF,
                ReasoningLevel::LOW,
                ReasoningLevel::HIGH,
                ReasoningLevel::MAX,
            ]
            .contains(level)
        {
            return Err(unsatisfiable(
                FEATURE_SESSION_REASONING,
                "reasoning_level",
                "the DeepSeek route takes off, low, high and max",
            ));
        }
        process.initialize.reasoning_effort = level.as_str().into();
        let patch = compaction_patch(request.compaction_policy.as_ref())?;
        Ok(Launch { process, patch })
    }

    pub fn open(&self, request: OpenRequest) -> Result<Session, Error> {
        self.open_with(request, &crossbeam_channel::never())
    }

    fn open_with(&self, request: OpenRequest, cancel: &Receiver<()>) -> Result<Session, Error> {
        if request.reopen && request.native_session_id.trim().is_empty() {
            return Err(unsatisfiable(
                FEATURE_OPEN_REOPEN,
                "",
                "a reopen names no stored native session",
            ));
        }
        refuse_unadvertised_tool_sources(&request, &[])?;
        refuse_unadvertised_tools(&request, &[])?;
        refuse_unadvertised_settings(&request, &self.describe().capabilities)?;
        let launch = self.launch(&request)?;
        let process = launch.start(self.config.start_timeout, None, cancel)?;
        let session_id: oxap_protocol::SessionId = if request.session_id.as_str().is_empty() {
            self.config.runtime.new_id("session").into()
        } else {
            request.session_id
        };
        let (native_id, recovery) = if request.reopen {
            resume(
                &process,
                &request.native_session_id,
                self.config.start_timeout,
            )?;
            (
                request.native_session_id,
                Some(RecoveryMetadata {
                    recovered: true,
                    reason: REOPEN_RECOVERY_REASON.into(),
                    ..RecoveryMetadata::default()
                }),
            )
        } else {
            let mint = self.config.native_session_id.unwrap_or(fresh_native_id);
            (mint(session_id.as_str()), None)
        };
        let state = SessionState {
            session_id,
            status: SessionStatus::IDLE,
            current_model_id: self.config.model.clone(),
            updated_at_ms: self.config.runtime.now_ms(),
            reasoning_level: request.reasoning_level,
            compaction_policy: request.compaction_policy,
            recovery,
            ..SessionState::default()
        };
        Ok(Session::new(
            process,
            native_id,
            state,
            self.config.journal_capacity,
            self.config.runtime.clone(),
            launch,
            self.config.start_timeout,
        ))
    }
}

impl AdapterContract for Adapter {
    fn probe(&self) -> BoxFuture<'_, Descriptor> {
        Box::pin(async move { Ok(self.describe()) })
    }

    fn open(&self, request: OpenRequest) -> BoxFuture<'_, Box<dyn SessionContract>> {
        let adapter = self.clone();
        Box::pin(async move {
            let (cancel, cancelled) = crossbeam_channel::bounded::<()>(0);
            let (reply, replied) = oneshot::channel();
            std::thread::spawn(move || {
                let _ = reply.send(adapter.open_with(request, &cancelled));
            });
            let session = replied.await.unwrap_or(Err(Error::SessionClosed))?;
            drop(cancel);
            Ok(Box::new(session) as Box<dyn SessionContract>)
        })
    }
}

impl Launch {
    pub(crate) fn start(
        &self,
        timeout: Duration,
        model: Option<&str>,
        cancel: &Receiver<()>,
    ) -> Result<Process, Error> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| harness("deepseek adapter: start timeout is out of range"))?;
        let mut process = self.process.clone();
        if let Some(model) = model {
            process.initialize.model = model.into();
        }
        let patch = self
            .patch
            .as_ref()
            .map(|patch| {
                write_patch(patch, |_, file, bytes| file.write_all(bytes)).map_err(Error::harness)
            })
            .transpose()?;
        if let Some(path) = &patch {
            process.args.push("--patch".into());
            process.args.push(path.to_string_lossy().into_owned());
        }
        let started = Process::start_classified(process, Some(deadline), cancel);
        if let Some(path) = patch {
            let _ = std::fs::remove_file(path);
        }
        started.map_err(|failure| match model {
            Some(model) if failure.remote => Error::ModelNotFound(model.into()),
            _ => Error::harness(failure.rpc),
        })
    }
}

pub fn fresh_native_id(_: &str) -> String {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).expect("the operating system provides secure entropy");
    random.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn resume(process: &Process, native_id: &str, timeout: Duration) -> Result<(), Error> {
    let params = serde_json::value::to_raw_value(&SessionResumeParams {
        session_id: native_id.into(),
    })
    .map_err(Error::harness)?;
    let deadline = Instant::now().checked_add(timeout);
    let caller = process.client.clone();
    let (reply, answered) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = reply.send(caller.call(METHOD_SESSION_RESUME, Some(params), deadline));
    });
    let mut inbound = process.client.inbound();
    let mut done = process.client.done();
    let outcome: Result<(), String> = loop {
        crossbeam_channel::select! {
            recv(answered) -> outcome => break match outcome {
                Ok(answer) => answer.map(drop).map_err(|failure| failure.to_string()),
                Err(_) => Err("the resume call ended without an answer".into()),
            },
            recv(inbound) -> message => match message {
                Ok(Inbound::Barrier(ack)) => {
                    drop(ack);
                    inbound = crossbeam_channel::never();
                    done = crossbeam_channel::never();
                }
                _ => break Err("a native observation preceded the resume response".into()),
            },
            recv(done) -> _ => break Err(process.client.err().unwrap_or_else(|| "EOF".into())),
        }
    };
    outcome.map_err(|detail| {
        unsatisfiable(
            FEATURE_OPEN_REOPEN,
            "",
            &format!("the harness could not resume the stored session: {detail}"),
        )
    })
}

fn write_patch(
    patch: &str,
    write: impl FnOnce(&Path, &mut File, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<PathBuf> {
    loop {
        let path = std::env::temp_dir().join(format!(
            "oap-deepseek-{}-{}.patch.yml",
            std::process::id(),
            PATCHES.fetch_add(1, Ordering::SeqCst)
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = match options.open(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            opened => opened?,
        };
        if let Err(error) = write(&path, &mut file, patch.as_bytes()) {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        return Ok(path);
    }
}

pub fn compaction_patch(policy: Option<&CompactionPolicy>) -> Result<Option<String>, Error> {
    let Some(policy) = policy else {
        return Ok(None);
    };
    match policy.kind.as_str() {
        kind if kind == CompactionKind::AUTO.as_str() => Ok(None),
        kind if kind == CompactionKind::OFF.as_str() => {
            Ok(Some("- id: compaction-basic\n  disabled: true\n".into()))
        }
        kind if kind == CompactionKind::SHARE.as_str() => Ok(Some(format!(
            "- id: compaction-basic\n  config:\n    thresholdRatio: {}\n",
            policy.share_percent as f64 / 100.0
        ))),
        _ => Err(unsatisfiable(
            FEATURE_COMPACTION_POLICY,
            "compaction_policy",
            "compaction-basic sets its threshold from the window, so a token count is refused",
        )),
    }
}

#[cfg(test)]
#[path = "../tests/support/patch.rs"]
mod tests;
