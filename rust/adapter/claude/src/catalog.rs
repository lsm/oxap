use std::collections::{BTreeMap, HashSet};

use oxap_adapter::{Error, UnsupportedControl, CONTROL_UNSATISFIABLE};
use oxap_protocol::{
    FeatureSupport, ParticipantId, SessionId, SupportLevel, ToolDefinition, ToolSourceDescriptor,
    ToolsListResponse, FEATURE_TOOLS_PROVIDE, TOOL_SOURCE_MCP, TOOL_SOURCE_NATIVE,
    TOOL_SOURCE_PROCESS, VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::value::{to_raw_value, RawValue};

use crate::native::{source, InitFrame, Raw};

const NATIVE_SOURCE: &str = "claude-code-native";

pub(crate) fn native_source() -> ToolSourceDescriptor {
    ToolSourceDescriptor {
        id: NATIVE_SOURCE.into(),
        kind: TOOL_SOURCE_NATIVE.into(),
        display_name: "Claude Code built-in tools".into(),
        ..ToolSourceDescriptor::default()
    }
}

#[derive(Default)]
pub(crate) struct Catalog {
    observed: Option<ToolsListResponse>,
    pub(crate) provided: Vec<ToolDefinition>,
    attribution: BTreeMap<String, String>,
    served: bool,
}

impl Catalog {
    pub(crate) fn attribution_for(&self, name: &str) -> &str {
        self.attribution.get(name).map_or("", String::as_str)
    }

    pub(crate) fn observe(&mut self, init: &InitFrame) {
        let mut seen = HashSet::new();
        let servers: Vec<_> = init
            .mcp_servers
            .iter()
            .map(|server| server.name.as_str())
            .filter(|name| *name != SDK_SERVER || self.provided.is_empty())
            .filter(|name| !name.is_empty() && seen.insert(*name))
            .collect();
        let mut sources = vec![native_source()];
        sources.extend(servers.iter().map(|name| ToolSourceDescriptor {
            id: format!("mcp:{name}"),
            kind: TOOL_SOURCE_PROCESS.into(),
            protocol: TOOL_SOURCE_MCP.into(),
            display_name: (*name).into(),
            ..ToolSourceDescriptor::default()
        }));
        seen.clear();
        let mut tools: Vec<_> = init
            .tools
            .iter()
            .flatten()
            .filter(|name| !name.is_empty() && seen.insert(name.as_str()))
            .map(|name| ToolDefinition {
                name: name.clone(),
                input_schema: Some(
                    RawValue::from_string(r#"{"type":"object"}"#.into()).expect("literal JSON"),
                ),
                execution_owner: "claude-code".into(),
                source: source_for(name, &servers),
                features: BTreeMap::from([(
                    "action.tools.execute".into(),
                    FeatureSupport {
                        level: SupportLevel::UNAVAILABLE,
                        reason: "the CLI executes its own tools".into(),
                        ..FeatureSupport::default()
                    },
                )]),
                ..ToolDefinition::default()
            })
            .collect();
        for tool in &self.provided {
            let qualified = format!("{SDK_PREFIX}{}", tool.name);
            let Some(echo) = tools.iter().position(|entry| entry.name == qualified) else {
                continue;
            };
            let bare = tools.iter().position(|entry| entry.name == tool.name);
            let first = bare.map_or(echo, |bare| bare.min(echo));
            tools[first] = tool.clone();
            if let Some(bare) = bare {
                tools.remove(bare.max(echo));
            }
        }
        if !self.served {
            let native_tools = tools.iter().filter(|tool| tool.source == NATIVE_SOURCE);
            self.attribution.clear();
            self.attribution
                .extend(native_tools.map(|tool| (tool.name.clone(), tool.source.clone())));
        }
        self.observed = Some(ToolsListResponse {
            sources,
            tools,
            ..ToolsListResponse::default()
        });
    }

    pub(crate) fn list(&mut self, session_id: Option<SessionId>) -> ToolsListResponse {
        if let (Some(_), Some(current)) = (&session_id, &self.observed) {
            self.attribution = current
                .tools
                .iter()
                .map(|tool| (tool.name.clone(), tool.source.clone()))
                .collect();
            self.served = true;
            return ToolsListResponse {
                session_id,
                ..current.clone()
            };
        }
        ToolsListResponse {
            session_id,
            sources: vec![native_source()],
            ..ToolsListResponse::default()
        }
    }
}

fn source_for(name: &str, servers: &[&str]) -> String {
    name.strip_prefix("mcp__")
        .and_then(|rest| {
            servers
                .iter()
                .filter(|server| server_matches(rest, server))
                .max_by_key(|server| server.len())
        })
        .map_or_else(|| NATIVE_SOURCE.into(), |server| format!("mcp:{server}"))
}

fn server_matches(rest: &str, server: &str) -> bool {
    rest.strip_prefix(server)
        .is_some_and(|suffix| suffix.starts_with("__"))
}

pub(crate) const SDK_SERVER: &str = "oap";
pub(crate) const SDK_PREFIX: &str = "mcp__oap__";
const MAX_TOOLS: usize = 64;
const NAME_LIMIT: usize = 64 - SDK_PREFIX.len();
const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";
const BUILTINS: &str = concat!(
    "Agent Artifact AskUserQuestion Bash ClaudeDesign CronCreate CronDelete CronList Edit ",
    "EndConversation EnterPlanMode EnterWorktree ExitPlanMode ExitWorktree Glob Grep ListAgents ListMcpResourcesTool ",
    "LSP Monitor NotebookEdit OfferChromeSetup PowerShell Projects propose_skills ProposeGoal PushNotification ",
    "Read ReadMcpResourceDirTool ReadMcpResourceTool ReadNotifications RemoteTrigger ReportFindings ScheduleWakeup SendFeedback SendMessage ",
    "SendUserFile SendUserMessage ShareOnboardingGuide ShowOnboardingRolePicker Skill SubagentHandback TaskCreate TaskGet TaskList ",
    "TaskStop TaskUpdate TodoWrite ToolSearch WaitForMcpServers WebFetch WebSearch Workflow Write Task",
);

pub(crate) fn admit(tools: &mut [ToolDefinition], opener: &ParticipantId) -> Result<(), Error> {
    let refuse = |tool: &ToolDefinition, source: &str, detail: &str| {
        Error::UnsupportedControl(Box::new(UnsupportedControl {
            feature: FEATURE_TOOLS_PROVIDE.into(),
            reason: CONTROL_UNSATISFIABLE,
            tool: tool.name.clone(),
            source: source.into(),
            detail: detail.into(),
            ..UnsupportedControl::default()
        }))
    };
    if let Some(extra) = tools.get(MAX_TOOLS) {
        return Err(refuse(extra, "", "at most 64 tools may be provided"));
    }
    let owner = match opener.as_str() {
        "" => "user",
        id => id,
    };
    let mut seen = HashSet::new();
    for tool in tools {
        let name = tool.name.as_str();
        let allowed_chars = |byte: u8| byte.is_ascii_alphanumeric() || b"_-".contains(&byte);
        let built_in = BUILTINS.split_ascii_whitespace().any(|word| word == name);
        if name.is_empty()
            || name.len() > NAME_LIMIT
            || !name.bytes().all(allowed_chars)
            || name.starts_with("mcp__")
            || built_in
            || !seen.insert(name)
        {
            return Err(refuse(tool, "", "invalid, reserved or duplicate tool name"));
        }
        if tool.execution_owner.as_str() != owner {
            return Err(refuse(tool, "", "execution_owner must be the opener"));
        }
        if tool.source != NATIVE_SOURCE {
            return Err(refuse(tool, &tool.source, "undeclared tool source"));
        }
        let schema = tool.input_schema.as_deref().map_or("null", RawValue::get);
        let fields: Option<BTreeMap<String, Box<RawValue>>> = source::decode(schema)
            .map_err(|_| refuse(tool, "", "input_schema must be an object"))?;
        if let Some(dialect) = fields.as_ref().and_then(|fields| fields.get("$schema")) {
            let allowed = source::decode::<Option<String>>(dialect.get())
                .is_ok_and(|name| name.is_none_or(|name| name.is_empty() || name == DIALECT));
            if !allowed {
                return Err(refuse(tool, "", "input_schema needs the 2020-12 dialect"));
            }
        }
        if fields.is_none() {
            tool.input_schema =
                Some(to_raw_value(&json!({"type":"object"})).expect("literal JSON"));
        }
    }
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Message {
    #[serde(deserialize_with = "source::raw")]
    id: Raw,
    method: String,
    params: serde_json::Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListedTool<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    description: &'a str,
    input_schema: &'a Raw,
}

pub(crate) fn discover(tools: &[ToolDefinition], request: &Raw) -> Result<Raw, Error> {
    let message = request.as_deref().map_or("null", RawValue::get);
    let message = source::decode_object::<Message>(message).unwrap_or_default();
    let Some(id) = message.id else {
        return Ok(None);
    };
    let result = match message.method.as_str() {
        "initialize" => to_raw_value(&json!({
            "protocolVersion": message.params["protocolVersion"].as_str()
                .filter(|version| !version.is_empty()).unwrap_or("2025-11-25"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SDK_SERVER, "version": VERSION}
        })),
        "ping" => to_raw_value(&json!({})),
        "tools/list" => {
            let listed: Vec<_> = tools
                .iter()
                .map(|tool| ListedTool {
                    name: &tool.name,
                    description: &tool.description,
                    input_schema: &tool.input_schema,
                })
                .collect();
            to_raw_value(&BTreeMap::from([("tools", listed)]))
        }
        _ => return reply(&id, "error", METHOD_NOT_FOUND),
    }
    .map_err(Error::harness)?;
    reply(&id, "result", result.get())
}

const METHOD_NOT_FOUND: &str = r#"{"code":-32601,"message":"method not found"}"#;

fn reply(id: &RawValue, key: &str, payload: &str) -> Result<Raw, Error> {
    let body = format!(r#"{{"mcp_response":{{"jsonrpc":"2.0","id":{id},"{key}":{payload}}}}}"#);
    RawValue::from_string(body)
        .map(Some)
        .map_err(Error::harness)
}

#[cfg(test)]
#[path = "../tests/support/catalog_attribution.rs"]
mod attribution_tests;
