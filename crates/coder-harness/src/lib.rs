use async_trait::async_trait;
use coder_core::{FinalReport, RunId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessRunRequest {
    pub run_id: RunId,
    pub workflow_id: String,
    pub node_id: String,
    pub agent_id: String,
    pub harness_id: String,
    #[serde(default)]
    pub repo_root: String,
    pub task: String,
    #[serde(default)]
    pub backend_context: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessRunResult {
    pub status: String,
    pub report: Option<FinalReport>,
    #[serde(default)]
    pub events: Vec<HarnessRunEvent>,
}

impl HarnessRunResult {
    pub fn completed() -> Self {
        Self {
            status: "completed".to_owned(),
            report: None,
            events: Vec::new(),
        }
    }

    pub fn blocked(blocker: impl Into<String>) -> Self {
        let blocker = blocker.into();
        Self {
            status: "blocked".to_owned(),
            report: Some(FinalReport::blocked("Harness backend blocked.", blocker)),
            events: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessRunEvent {
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub refs: Vec<HarnessRunEventRef>,
}

impl HarnessRunEvent {
    pub fn new(kind: impl Into<String>, payload: Value) -> Self {
        Self {
            kind: kind.into(),
            payload,
            refs: Vec::new(),
        }
    }

    pub fn with_ref(mut self, label: impl Into<String>, uri: impl Into<String>) -> Self {
        self.refs.push(HarnessRunEventRef {
            label: label.into(),
            uri: uri.into(),
        });
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessRunEventRef {
    pub label: String,
    pub uri: String,
}

#[async_trait]
pub trait HarnessBackend: Send + Sync {
    async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError>;
}

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("backend unavailable: {0}")]
    Unavailable(String),
    #[error("backend rejected request: {0}")]
    Rejected(String),
    #[error("backend failed: {0}")]
    Failed(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    #[default]
    Medium,
    High,
}

impl RiskLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Medium => 1,
            Self::High => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectLevel {
    None,
    Read,
    Write,
    #[default]
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpManifestOperation {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub risk: RiskLevel,
    #[serde(default)]
    pub side_effect: SideEffectLevel,
    #[serde(default)]
    pub enabled_by_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerManifest {
    pub server_id: String,
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env_vars: Vec<String>,
    pub startup_timeout_sec: Option<u64>,
    pub tool_timeout_sec: Option<u64>,
    #[serde(default)]
    pub operations: Vec<McpManifestOperation>,
    #[serde(default)]
    pub enabled_by_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpManifestValidation {
    pub ok: bool,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub manifest: Option<McpServerManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerSummary {
    pub server_id: String,
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub requires_approval: bool,
    #[serde(default)]
    pub operations: Vec<McpManifestOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolSummary {
    pub server_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub risk: RiskLevel,
    pub side_effect: SideEffectLevel,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub requires_approval: bool,
    #[serde(default)]
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpToolCallRequest {
    pub server_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub args: Value,
    pub run_id: Option<RunId>,
    #[serde(default)]
    pub approved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpToolCallResult {
    pub status: String,
    #[serde(default)]
    pub requires_approval: bool,
    pub approval_key: String,
    #[serde(default)]
    pub output: Value,
    pub evidence_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionCapabilityPolicy {
    #[serde(default)]
    pub risk_level: RiskLevel,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub requires_approval: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionActionPolicy {
    pub operation_id: String,
    pub risk_level: RiskLevel,
    #[serde(default)]
    pub permissions: Vec<String>,
    pub requires_approval: bool,
    pub known_operation: bool,
    #[serde(default)]
    pub reason: String,
}

impl ExtensionActionPolicy {
    pub fn approval_key(&self) -> String {
        format!("plugin:{}:{}", self.operation_id, self.risk_level.as_str())
    }
}

impl McpManifestOperation {
    pub fn requires_approval(&self) -> bool {
        true
    }
}

pub fn mcp_approval_key(server_id: &str, tool_name: &str) -> String {
    format!("mcp:{server_id}:{tool_name}")
}

pub fn merge_extension_policy(
    operation_id: impl Into<String>,
    capability: Option<&ExtensionCapabilityPolicy>,
    spec_risk_level: RiskLevel,
    spec_requires_permission: bool,
    input_requires_permission: bool,
    input_requires_approval: bool,
) -> ExtensionActionPolicy {
    let operation_id = operation_id.into();
    let Some(capability) = capability else {
        return ExtensionActionPolicy {
            operation_id,
            risk_level: spec_risk_level,
            permissions: Vec::new(),
            requires_approval: true,
            known_operation: false,
            reason: "Unknown plugin operation requires explicit approval.".to_owned(),
        };
    };
    let effective_risk = max_risk(spec_risk_level, capability.risk_level);
    let requires_approval = capability.requires_approval
        || spec_requires_permission
        || input_requires_permission
        || input_requires_approval
        || matches!(effective_risk, RiskLevel::Medium | RiskLevel::High);
    ExtensionActionPolicy {
        operation_id,
        risk_level: effective_risk,
        permissions: capability.permissions.clone(),
        requires_approval,
        known_operation: true,
        reason: "Capability policy merged.".to_owned(),
    }
}

pub fn validate_mcp_manifest(raw: &Value) -> McpManifestValidation {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut manifest = match parse_mcp_manifest(raw) {
        Ok(manifest) => manifest,
        Err(error) => {
            return McpManifestValidation {
                ok: false,
                errors: vec![error],
                warnings,
                manifest: None,
            };
        }
    };

    if manifest.server_id.is_empty() {
        errors.push("server_id is required".to_owned());
    }
    if manifest.command.is_empty() {
        errors.push("command is required for stdio MCP servers".to_owned());
    }
    for name in &manifest.env_vars {
        if name.is_empty()
            || !name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            errors.push(format!("invalid MCP env var name '{name}'"));
        }
    }
    if manifest.enabled_by_default {
        warnings.push(
            "MCP servers are not enabled by default; explicit user approval is required".to_owned(),
        );
        manifest.enabled_by_default = false;
    }
    for operation in &mut manifest.operations {
        if operation.enabled_by_default {
            warnings.push(format!(
                "operation {} default enablement was disabled",
                operation.name
            ));
            operation.enabled_by_default = false;
        }
    }

    McpManifestValidation {
        ok: errors.is_empty(),
        errors,
        warnings,
        manifest: Some(manifest),
    }
}

pub fn parse_mcp_manifest(raw: &Value) -> Result<McpServerManifest, String> {
    let object = raw
        .as_object()
        .ok_or_else(|| "MCP manifest must be a JSON object".to_owned())?;
    let server_id = string_field(object.get("server_id").or_else(|| object.get("id")));
    let mut name = string_field(object.get("name"));
    if name.is_empty() {
        name = server_id.clone();
    }
    let raw_operations = object
        .get("operations")
        .or_else(|| object.get("tools"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut operations = Vec::new();
    for item in raw_operations {
        let Some(operation) = parse_mcp_operation(&item)? else {
            continue;
        };
        operations.push(operation);
    }
    Ok(McpServerManifest {
        server_id,
        name,
        command: string_field(object.get("command")),
        args: string_list_field(object.get("args")),
        cwd: optional_string_field(object.get("cwd")),
        env_vars: string_list_field(object.get("env_vars")),
        startup_timeout_sec: positive_u64_field(object.get("startup_timeout_sec"))?,
        tool_timeout_sec: positive_u64_field(object.get("tool_timeout_sec"))?,
        operations,
        enabled_by_default: bool_field(object.get("enabled_by_default")),
    })
}

fn parse_mcp_operation(raw: &Value) -> Result<Option<McpManifestOperation>, String> {
    let Some(object) = raw.as_object() else {
        return Ok(None);
    };
    let name = string_field(
        object
            .get("name")
            .or_else(|| object.get("operation"))
            .or_else(|| object.get("id")),
    );
    if name.is_empty() {
        return Ok(None);
    }
    Ok(Some(McpManifestOperation {
        name,
        description: string_field(object.get("description")),
        risk: parse_risk_level(object.get("risk").or_else(|| object.get("risk_level")))?,
        side_effect: parse_side_effect_level(object.get("side_effect"))?,
        enabled_by_default: bool_field(object.get("enabled_by_default")),
    }))
}

fn parse_risk_level(value: Option<&Value>) -> Result<RiskLevel, String> {
    match string_field(value).as_str() {
        "" | "medium" => Ok(RiskLevel::Medium),
        "low" => Ok(RiskLevel::Low),
        "high" => Ok(RiskLevel::High),
        other => Err(format!("unsupported MCP risk level '{other}'")),
    }
}

fn parse_side_effect_level(value: Option<&Value>) -> Result<SideEffectLevel, String> {
    match string_field(value).as_str() {
        "" | "external" => Ok(SideEffectLevel::External),
        "none" => Ok(SideEffectLevel::None),
        "read" => Ok(SideEffectLevel::Read),
        "write" => Ok(SideEffectLevel::Write),
        other => Err(format!("unsupported MCP side effect '{other}'")),
    }
}

fn string_field(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_owned()
}

fn string_list_field(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn bool_field(value: Option<&Value>) -> bool {
    value.and_then(Value::as_bool).unwrap_or(false)
}

fn optional_string_field(value: Option<&Value>) -> Option<String> {
    let value = string_field(value);
    (!value.is_empty()).then_some(value)
}

fn positive_u64_field(value: Option<&Value>) -> Result<Option<u64>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .as_u64()
        .ok_or_else(|| "MCP timeout values must be positive integer seconds".to_owned())?;
    if value == 0 {
        return Err("MCP timeout values must be positive integer seconds".to_owned());
    }
    Ok(Some(value))
}

fn default_true() -> bool {
    true
}

fn max_risk(left: RiskLevel, right: RiskLevel) -> RiskLevel {
    if left.rank() >= right.rank() {
        left
    } else {
        right
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn mcp_manifest_validation_never_enables_by_default() {
        let validation = validate_mcp_manifest(&json!({
            "server_id": "github",
            "name": "GitHub",
            "command": "github-mcp",
            "enabled_by_default": true,
            "operations": [
                {
                    "name": "search_issues",
                    "risk": "low",
                    "side_effect": "read",
                    "enabled_by_default": true
                }
            ]
        }));

        assert!(validation.ok);
        let manifest = validation.manifest.unwrap();
        assert!(!manifest.enabled_by_default);
        assert!(!manifest.operations[0].enabled_by_default);
        assert!(manifest.operations[0].requires_approval());
        assert!(validation.warnings.len() >= 2);
    }

    #[test]
    fn mcp_manifest_supports_tool_aliases_and_defaults() {
        let manifest = parse_mcp_manifest(&json!({
            "id": "fs",
            "command": "fs-mcp",
            "tools": [
                {"id": "read_file"}
            ]
        }))
        .unwrap();

        assert_eq!(manifest.server_id, "fs");
        assert_eq!(manifest.name, "fs");
        assert_eq!(manifest.operations[0].name, "read_file");
        assert_eq!(manifest.operations[0].risk, RiskLevel::Medium);
        assert_eq!(
            manifest.operations[0].side_effect,
            SideEffectLevel::External
        );
    }

    #[test]
    fn mcp_manifest_reports_missing_required_fields() {
        let validation = validate_mcp_manifest(&json!({"name": "Empty"}));

        assert!(!validation.ok);
        assert!(validation
            .errors
            .iter()
            .any(|error| error == "server_id is required"));
        assert!(validation
            .errors
            .iter()
            .any(|error| error == "command is required for stdio MCP servers"));
    }

    #[test]
    fn mcp_manifest_rejects_unknown_risk_and_side_effect() {
        let risk = validate_mcp_manifest(&json!({
            "server_id": "x",
            "command": "x-mcp",
            "operations": [{"name": "op", "risk": "critical"}]
        }));
        let side_effect = validate_mcp_manifest(&json!({
            "server_id": "x",
            "command": "x-mcp",
            "operations": [{"name": "op", "side_effect": "network"}]
        }));

        assert!(!risk.ok);
        assert!(risk.errors[0].contains("unsupported MCP risk level"));
        assert!(!side_effect.ok);
        assert!(side_effect.errors[0].contains("unsupported MCP side effect"));
    }

    #[test]
    fn extension_policy_uses_highest_risk_and_capability_permissions() {
        let capability = ExtensionCapabilityPolicy {
            risk_level: RiskLevel::High,
            permissions: vec!["edit_files".to_owned()],
            requires_approval: true,
        };

        let policy = merge_extension_policy(
            "apply_patch",
            Some(&capability),
            RiskLevel::Low,
            false,
            false,
            false,
        );

        assert!(policy.known_operation);
        assert_eq!(policy.risk_level, RiskLevel::High);
        assert_eq!(policy.permissions, ["edit_files"]);
        assert!(policy.requires_approval);
        assert_eq!(policy.approval_key(), "plugin:apply_patch:high");
    }

    #[test]
    fn extension_policy_requires_approval_for_unknown_operation() {
        let policy =
            merge_extension_policy("unknown.op", None, RiskLevel::Low, false, false, false);

        assert!(!policy.known_operation);
        assert!(policy.requires_approval);
        assert_eq!(policy.risk_level, RiskLevel::Low);
    }

    #[test]
    fn extension_policy_allows_known_low_risk_without_permission_flags() {
        let capability = ExtensionCapabilityPolicy {
            risk_level: RiskLevel::Low,
            permissions: Vec::new(),
            requires_approval: false,
        };

        let policy = merge_extension_policy(
            "project_index",
            Some(&capability),
            RiskLevel::Low,
            false,
            false,
            false,
        );

        assert!(policy.known_operation);
        assert!(!policy.requires_approval);
    }

    #[test]
    fn extension_policy_requires_approval_for_medium_or_requested_permission() {
        let capability = ExtensionCapabilityPolicy {
            risk_level: RiskLevel::Low,
            permissions: Vec::new(),
            requires_approval: false,
        };

        let medium_policy = merge_extension_policy(
            "project_index",
            Some(&capability),
            RiskLevel::Medium,
            false,
            false,
            false,
        );
        let permission_policy = merge_extension_policy(
            "project_index",
            Some(&capability),
            RiskLevel::Low,
            true,
            false,
            false,
        );

        assert!(medium_policy.requires_approval);
        assert!(permission_policy.requires_approval);
    }
}
