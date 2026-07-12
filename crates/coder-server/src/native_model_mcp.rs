use std::collections::{BTreeMap, BTreeSet};

use coder_harness::{McpToolSummary, SideEffectLevel};
use coder_workflow::ToolConcurrency;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::model_tool_dispatch::ModelMcpToolRoute;

const MCP_TOOL_PREFIX: &str = "mcp__";
const MCP_TOOL_DELIMITER: &str = "__";
const MAX_MODEL_TOOL_NAME_BYTES: usize = 64;
const MODEL_TOOL_HASH_HEX_LEN: usize = 12;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeModelMcpTool {
    pub(crate) provider_name: String,
    pub(crate) server_id: String,
    pub(crate) tool_name: String,
    pub(crate) description: String,
    pub(crate) input_schema: Value,
    pub(crate) side_effect: SideEffectLevel,
}

impl NativeModelMcpTool {
    pub(crate) fn model_spec(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.provider_name,
                "description": self.description,
                "parameters": self.input_schema
            }
        })
    }

    pub(crate) fn route(&self) -> ModelMcpToolRoute {
        ModelMcpToolRoute {
            server_id: self.server_id.clone(),
            tool_name: self.tool_name.clone(),
        }
    }

    pub(crate) fn concurrency(&self) -> ToolConcurrency {
        if self.side_effect == SideEffectLevel::Read {
            ToolConcurrency::ConcurrentSafe
        } else {
            ToolConcurrency::Exclusive
        }
    }
}

pub(crate) fn snapshot_native_model_mcp_tools(
    tools: Vec<McpToolSummary>,
) -> Vec<NativeModelMcpTool> {
    let mut tools = tools
        .into_iter()
        .filter(|tool| {
            tool.enabled && !tool.server_id.trim().is_empty() && !tool.name.trim().is_empty()
        })
        .collect::<Vec<_>>();
    tools
        .sort_by(|left, right| (&left.server_id, &left.name).cmp(&(&right.server_id, &right.name)));

    let mut seen_raw = BTreeSet::new();
    let candidates = tools
        .into_iter()
        .filter_map(|tool| {
            let raw_identity = format!("{}\0{}", tool.server_id, tool.name);
            if !seen_raw.insert(raw_identity.clone()) {
                return None;
            }
            let input_schema = normalized_input_schema(tool.input_schema)?;
            let base_name = base_provider_name(&tool.server_id, &tool.name);
            Some(NativeModelMcpCandidate {
                raw_identity,
                base_name,
                tool: NativeModelMcpTool {
                    provider_name: String::new(),
                    server_id: tool.server_id,
                    tool_name: tool.name,
                    description: tool.description,
                    input_schema,
                    side_effect: tool.side_effect,
                },
            })
        })
        .collect::<Vec<_>>();
    let base_counts = candidates
        .iter()
        .fold(BTreeMap::new(), |mut counts, candidate| {
            *counts.entry(candidate.base_name.clone()).or_insert(0_usize) += 1;
            counts
        });
    let mut used_provider_names = BTreeSet::new();
    candidates
        .into_iter()
        .map(|mut candidate| {
            let force_hash = base_counts.get(&candidate.base_name).copied().unwrap_or(0) > 1;
            candidate.tool.provider_name = unique_provider_name(
                &candidate.base_name,
                &candidate.raw_identity,
                force_hash,
                &mut used_provider_names,
            );
            candidate.tool
        })
        .collect()
}

struct NativeModelMcpCandidate {
    raw_identity: String,
    base_name: String,
    tool: NativeModelMcpTool,
}

pub(crate) fn native_model_mcp_routes(
    tools: &[NativeModelMcpTool],
) -> BTreeMap<String, ModelMcpToolRoute> {
    tools
        .iter()
        .map(|tool| (tool.provider_name.clone(), tool.route()))
        .collect()
}

fn normalized_input_schema(schema: Value) -> Option<Value> {
    let mut schema = schema.as_object()?.clone();
    match schema.get("type") {
        Some(Value::String(kind)) if kind == "object" => {}
        None => {
            schema.insert("type".to_owned(), Value::String("object".to_owned()));
        }
        _ => return None,
    }
    schema
        .entry("properties".to_owned())
        .or_insert_with(|| json!({}));
    Some(Value::Object(schema))
}

fn base_provider_name(server_id: &str, tool_name: &str) -> String {
    format!(
        "{MCP_TOOL_PREFIX}{}{MCP_TOOL_DELIMITER}{}",
        sanitize_model_tool_name(server_id),
        sanitize_model_tool_name(tool_name)
    )
}

fn unique_provider_name(
    base: &str,
    raw_identity: &str,
    force_hash: bool,
    used_names: &mut BTreeSet<String>,
) -> String {
    if !force_hash && base.len() <= MAX_MODEL_TOOL_NAME_BYTES && used_names.insert(base.to_owned())
    {
        return base.to_owned();
    }

    let mut attempt = 0_u32;
    loop {
        let hash_input = if attempt == 0 {
            raw_identity.to_owned()
        } else {
            format!("{raw_identity}\0{attempt}")
        };
        let suffix = model_tool_hash_suffix(&hash_input);
        let prefix_len = MAX_MODEL_TOOL_NAME_BYTES.saturating_sub(suffix.len());
        let candidate = format!("{}{}", &base[..base.len().min(prefix_len)], suffix);
        if used_names.insert(candidate.clone()) {
            return candidate;
        }
        attempt = attempt.saturating_add(1);
    }
}

fn sanitize_model_tool_name(name: &str) -> String {
    let sanitized = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "_".to_owned()
    } else {
        sanitized
    }
}

fn model_tool_hash_suffix(raw_identity: &str) -> String {
    let digest = Sha256::digest(raw_identity.as_bytes());
    let hex = format!("{digest:x}");
    format!("_{}", &hex[..MODEL_TOOL_HASH_HEX_LEN])
}

#[cfg(test)]
mod tests {
    use coder_harness::{RiskLevel, SideEffectLevel};

    use super::*;

    fn summary(server_id: &str, name: &str) -> McpToolSummary {
        McpToolSummary {
            server_id: server_id.to_owned(),
            name: name.to_owned(),
            description: "Test tool".to_owned(),
            risk: RiskLevel::Low,
            side_effect: SideEffectLevel::Read,
            enabled: true,
            requires_approval: true,
            input_schema: json!({
                "type": "object",
                "properties": {"query": {"type": "string"}}
            }),
        }
    }

    #[test]
    fn snapshots_codex_style_names_without_losing_raw_routes() {
        let tools = snapshot_native_model_mcp_tools(vec![summary("local files", "find/all")]);

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].provider_name, "mcp__local_files__find_all");
        assert_eq!(tools[0].server_id, "local files");
        assert_eq!(tools[0].tool_name, "find/all");
        assert_eq!(
            tools[0].model_spec()["function"]["parameters"]["type"],
            "object"
        );
    }

    #[test]
    fn hashes_sanitization_collisions_and_enforces_64_byte_limit() {
        let tools = snapshot_native_model_mcp_tools(vec![
            summary("same server", "same/name"),
            summary("same-server", "same-name"),
            summary(&"server".repeat(20), &"tool".repeat(20)),
        ]);
        let names = tools
            .iter()
            .map(|tool| tool.provider_name.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(names.len(), 3);
        assert!(names
            .iter()
            .all(|name| name.len() <= MAX_MODEL_TOOL_NAME_BYTES));
        assert!(names.iter().all(|name| name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')));
        assert!(names.iter().any(|name| name.contains("_")));
    }

    #[test]
    fn skips_disabled_duplicate_and_non_object_tools() {
        let duplicate = summary("server", "tool");
        let mut disabled = summary("server", "disabled");
        disabled.enabled = false;
        let mut invalid = summary("server", "invalid");
        invalid.input_schema = json!({"type": "array"});

        let tools =
            snapshot_native_model_mcp_tools(vec![duplicate.clone(), duplicate, disabled, invalid]);

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].provider_name, "mcp__server__tool");
    }

    #[test]
    fn only_read_only_mcp_tools_are_parallel_safe() {
        let read = snapshot_native_model_mcp_tools(vec![summary("server", "read")]);
        let mut write = summary("server", "write");
        write.side_effect = SideEffectLevel::Write;
        let write = snapshot_native_model_mcp_tools(vec![write]);

        assert_eq!(read[0].concurrency(), ToolConcurrency::ConcurrentSafe);
        assert_eq!(write[0].concurrency(), ToolConcurrency::Exclusive);
    }
}
