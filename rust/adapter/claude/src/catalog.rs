use std::collections::{BTreeMap, HashSet};

use oxap_protocol::{
    FeatureSupport, SessionId, SupportLevel, ToolDefinition, ToolSourceDescriptor,
    ToolsListResponse, TOOL_SOURCE_MCP, TOOL_SOURCE_NATIVE, TOOL_SOURCE_PROCESS,
};
use serde_json::value::RawValue;

use crate::native::InitFrame;

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
    attribution: BTreeMap<String, String>,
    served: bool,
}

impl Catalog {
    pub(crate) fn observe(&mut self, init: &InitFrame) {
        let mut seen = HashSet::new();
        let servers: Vec<_> = init
            .mcp_servers
            .iter()
            .map(|server| server.name.as_str())
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
        let tools: Vec<_> = init
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

#[cfg(test)]
#[path = "../tests/support/catalog_attribution.rs"]
mod attribution_tests;
