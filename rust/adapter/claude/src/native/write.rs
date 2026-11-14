use oxap_protocol::{CompactionPolicy, ReasoningLevel};
use oxap_protocol::{FEATURE_COMPACTION_POLICY, FEATURE_SESSION_REASONING};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use oxap_adapter::gojson::{go_marshal, strings};
use oxap_adapter::{Error, UnsupportedControl, CONTROL_UNSATISFIABLE};

use super::{
    Invalid, CONTROL_APPLY_FLAGS, CONTROL_INITIALIZE, CONTROL_INTERRUPT, HOOK_PRE_TOOL_USE,
    TOOL_SELECTION_HOOK, TYPE_USER,
};

fn marshal(value: &impl Serialize) -> Result<String, serde_json::Error> {
    serde_json::to_string(value).map(|json| go_marshal(&json))
}

#[derive(Serialize)]
struct TurnMessage<'a> {
    content: &'a str,
    role: &'static str,
}

#[derive(Serialize)]
struct TurnOrigin {
    kind: &'static str,
}

#[derive(Serialize)]
struct UserTurn<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    client_composed: Option<bool>,
    message: TurnMessage<'a>,
    origin: TurnOrigin,
    parent_tool_use_id: Option<()>,
    session_id: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    uuid: &'a str,
}

pub fn new_user_turn(uuid: &str, text: &str, composed: bool) -> Result<String, serde_json::Error> {
    let turn = UserTurn {
        client_composed: composed.then_some(true),
        message: TurnMessage {
            content: text,
            role: "user",
        },
        origin: TurnOrigin { kind: "human" },
        parent_tool_use_id: None,
        session_id: "default",
        kind: TYPE_USER,
        uuid,
    };
    marshal(&turn)
}

pub fn validate_turn_uuid(uuid: &str) -> Result<(), Invalid> {
    if uuid.is_empty() || uuid.len() > 128 {
        return Err(Invalid("turn uuid must be 1-128 bytes".into()));
    }
    if !uuid
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err(Invalid(
            "turn uuid must be alphanumeric with hyphens".into(),
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct Empty {}

pub fn hook_continue() -> Result<String, serde_json::Error> {
    marshal(&Empty {})
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreToolUseDecision<'a> {
    hook_event_name: &'static str,
    permission_decision: &'static str,
    permission_decision_reason: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HookDeny<'a> {
    hook_specific_output: PreToolUseDecision<'a>,
}

pub fn deny_tool(reason: &str) -> Result<String, serde_json::Error> {
    marshal(&HookDeny {
        hook_specific_output: PreToolUseDecision {
            hook_event_name: HOOK_PRE_TOOL_USE,
            permission_decision: "deny",
            permission_decision_reason: reason,
        },
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HookMatcher {
    matcher: Option<()>,
    hook_callback_ids: [&'static str; 1],
}

#[derive(Serialize)]
struct ToolSelectionHooks {
    #[serde(rename = "PreToolUse")]
    pre_tool_use: [HookMatcher; 1],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InitializeRequest<'a> {
    subtype: &'static str,
    hooks: ToolSelectionHooks,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    sdk_mcp_servers: &'a [String],
}

pub fn initialize_request(sdk_mcp_servers: &[String]) -> Result<String, serde_json::Error> {
    marshal(&InitializeRequest {
        subtype: CONTROL_INITIALIZE,
        hooks: ToolSelectionHooks {
            pre_tool_use: [HookMatcher {
                matcher: None,
                hook_callback_ids: [TOOL_SELECTION_HOOK],
            }],
        },
        sdk_mcp_servers,
    })
}

#[derive(Serialize)]
struct SubtypeRequest {
    subtype: &'static str,
}

pub fn interrupt_request() -> Result<String, serde_json::Error> {
    marshal(&SubtypeRequest {
        subtype: CONTROL_INTERRUPT,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PermissionAllow<'a> {
    behavior: &'static str,
    updated_input: Option<&'a RawValue>,
}

pub fn permission_allow(updated_input: Option<&RawValue>) -> Result<String, serde_json::Error> {
    marshal(&PermissionAllow {
        behavior: "allow",
        updated_input,
    })
}

#[derive(Serialize)]
struct PermissionDeny<'a> {
    behavior: &'static str,
    message: &'a str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    interrupt: bool,
}

pub fn permission_deny(message: &str, interrupt: bool) -> Result<String, serde_json::Error> {
    marshal(&PermissionDeny {
        behavior: "deny",
        message,
        interrupt,
    })
}

#[derive(Serialize)]
struct McpResponse<'a> {
    mcp_response: Option<&'a RawValue>,
}

pub fn mcp_response(message: Option<&RawValue>) -> Result<String, serde_json::Error> {
    marshal(&McpResponse {
        mcp_response: message,
    })
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct InterruptResult {
    #[serde(default, deserialize_with = "strings")]
    pub still_queued: Vec<String>,
    #[serde(default, deserialize_with = "strings")]
    pub cancelled: Vec<String>,
}

pub fn decode_interrupt_result(raw: &[u8]) -> Result<InterruptResult, serde_json::Error> {
    super::source::decode_object(&String::from_utf8_lossy(raw))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagSettings<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_compact_enabled: Option<Option<bool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_compact_window: Option<Option<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort_level: Option<&'a str>,
}

#[derive(Serialize)]
struct ApplyFlagSettingsRequest<'a> {
    subtype: &'static str,
    settings: &'a FlagSettings<'a>,
}

pub fn apply_flag_settings_request(settings: &FlagSettings) -> Result<String, serde_json::Error> {
    marshal(&ApplyFlagSettingsRequest {
        subtype: CONTROL_APPLY_FLAGS,
        settings,
    })
}

pub fn get_settings_request() -> Result<String, serde_json::Error> {
    marshal(&SubtypeRequest {
        subtype: "get_settings",
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppliedSettings {
    pub model: String,
    pub effort: String,
}

impl AppliedSettings {
    fn merge(&mut self, raw: &str) -> Result<(), serde_json::Error> {
        for (key, value) in super::source::members(raw)? {
            let slot = match key.as_str() {
                "model" => &mut self.model,
                "effort" => &mut self.effort,
                _ => continue,
            };
            if value.get() != "null" {
                *slot = super::source::decode(value.get())?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectiveSettings {
    pub auto_compact_enabled: Option<bool>,
    pub auto_compact_window: Option<i64>,
}

impl EffectiveSettings {
    fn merge(&mut self, raw: &str) -> Result<(), serde_json::Error> {
        for (key, value) in super::source::members(raw)? {
            match key.as_str() {
                "autoCompactEnabled" if value.get() != "null" => {
                    self.auto_compact_enabled = super::source::decode(value.get())?;
                }
                "autoCompactWindow" if value.get() != "null" => {
                    self.auto_compact_window = super::source::decode(value.get())?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub applied: AppliedSettings,
    pub effective: EffectiveSettings,
}

pub fn decode_settings(raw: &[u8]) -> Result<Settings, serde_json::Error> {
    let mut settings = Settings::default();
    let text = String::from_utf8_lossy(raw);
    if text.trim() == "null" {
        return Ok(settings);
    }
    for (key, value) in super::source::members(&text)? {
        let member = value.get();
        if member == "null" {
            continue;
        }
        match key.as_str() {
            "applied" | "Applied" => settings.applied.merge(member)?,
            "effective" | "Effective" => settings.effective.merge(member)?,
            _ => {}
        }
    }
    Ok(settings)
}

pub fn settings_flags(
    level: ReasoningLevel,
    compaction: Option<&CompactionPolicy>,
    live: bool,
) -> Result<FlagSettings<'static>, Error> {
    let effort_level = match level.as_str() {
        "" => None,
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" => Some("max"),
        _ => {
            return Err(Error::UnsupportedControl(Box::new(UnsupportedControl {
                feature: FEATURE_SESSION_REASONING.to_owned(),
                reason: CONTROL_UNSATISFIABLE,
                field: "reasoning_level".into(),
                detail: "Claude Code's effort levels are low through max".into(),
                ..UnsupportedControl::default()
            })))
        }
    };
    let mut flags = FlagSettings {
        effort_level,
        ..FlagSettings::default()
    };
    if let Some(policy) = compaction {
        match policy.kind.as_str() {
            "auto" => {
                if live {
                    flags.auto_compact_enabled = Some(None);
                    flags.auto_compact_window = Some(None);
                }
            }
            "off" => {
                flags.auto_compact_enabled = Some(Some(false));
                if live {
                    flags.auto_compact_window = Some(None);
                }
            }
            "tokens" => {
                flags.auto_compact_enabled = Some(Some(true));
                flags.auto_compact_window = Some(Some(policy.tokens));
            }
            _ => {
                return Err(Error::UnsupportedControl(Box::new(UnsupportedControl {
                    feature: FEATURE_COMPACTION_POLICY.to_owned(),
                    reason: CONTROL_UNSATISFIABLE,
                    field: "compaction_policy".into(),
                    detail: "Claude Code takes a token window, not a share".into(),
                    ..UnsupportedControl::default()
                })))
            }
        }
    }
    Ok(flags)
}
