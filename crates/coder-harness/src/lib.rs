use async_trait::async_trait;
use coder_core::{FinalReport, RunId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCapability {
    pub name: String,
    pub toolset: String,
    pub side_effect: SideEffectLevel,
    pub risk: RiskLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRegistryEntry {
    pub capability: ToolCapability,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub harness_ids: Vec<String>,
    #[serde(default)]
    pub required_permission: String,
    #[serde(default)]
    pub approval_behavior: String,
    #[serde(default)]
    pub evidence_emitted: String,
    #[serde(default)]
    pub timeline_item: String,
    #[serde(default = "default_true")]
    pub enabled_by_default: bool,
    #[serde(default)]
    pub requires_approval: bool,
}

#[derive(Debug, Clone)]
pub struct ToolRegistry {
    entries: Vec<ToolRegistryEntry>,
}

impl ToolRegistry {
    pub fn new(entries: Vec<ToolRegistryEntry>) -> Self {
        Self { entries }
    }

    pub fn list_tools(&self, harness_id: Option<&str>) -> Vec<ToolRegistryEntry> {
        self.entries
            .iter()
            .filter(|entry| {
                harness_id
                    .map(|harness_id| entry.harness_ids.iter().any(|id| id == harness_id))
                    .unwrap_or(true)
            })
            .cloned()
            .collect()
    }

    pub fn get_tool(&self, name: &str) -> Option<ToolRegistryEntry> {
        self.entries
            .iter()
            .find(|entry| entry.capability.name == name)
            .cloned()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new(tool_registry_entries())
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

pub fn mock_mcp_server_manifest() -> McpServerManifest {
    McpServerManifest {
        server_id: "local-mock".to_owned(),
        name: "Local Mock MCP".to_owned(),
        enabled_by_default: false,
        operations: vec![
            mock_mcp_operation(
                "mock.echo",
                "Returns the supplied arguments after secret-key redaction.",
                RiskLevel::Low,
                SideEffectLevel::None,
            ),
            mock_mcp_operation(
                "mock.sum",
                "Sums numeric arguments deterministically.",
                RiskLevel::Low,
                SideEffectLevel::None,
            ),
            mock_mcp_operation(
                "mock.fail",
                "Returns a deterministic failure response.",
                RiskLevel::Medium,
                SideEffectLevel::None,
            ),
            mock_mcp_operation(
                "mock.large_output",
                "Returns output large enough for blob-backed evidence.",
                RiskLevel::Medium,
                SideEffectLevel::Read,
            ),
            mock_mcp_operation(
                "mock.external_effect",
                "Simulates an external side effect without performing it.",
                RiskLevel::High,
                SideEffectLevel::External,
            ),
        ],
    }
}

pub fn mock_mcp_servers() -> Vec<McpServerSummary> {
    let manifest = mock_mcp_server_manifest();
    vec![McpServerSummary {
        server_id: manifest.server_id,
        name: manifest.name,
        enabled: false,
        requires_approval: true,
        operations: manifest.operations,
    }]
}

pub fn mock_mcp_tools() -> Vec<McpToolSummary> {
    mock_mcp_server_manifest()
        .operations
        .into_iter()
        .map(|operation| McpToolSummary {
            server_id: "local-mock".to_owned(),
            name: operation.name,
            description: operation.description,
            risk: operation.risk,
            side_effect: operation.side_effect,
            enabled: false,
            requires_approval: true,
        })
        .collect()
}

pub fn find_mock_mcp_tool(server_id: &str, tool_name: &str) -> Option<McpToolSummary> {
    mock_mcp_tools()
        .into_iter()
        .find(|tool| tool.server_id == server_id && tool.name == tool_name)
}

pub fn invoke_mock_mcp_tool(request: &McpToolCallRequest) -> McpToolCallResult {
    let approval_key = mcp_approval_key(&request.server_id, &request.tool_name);
    let Some(_tool) = find_mock_mcp_tool(&request.server_id, &request.tool_name) else {
        if !request.approved {
            return blocked_mcp_result(
                approval_key,
                "Unknown MCP tools require explicit approval before rejection.",
            );
        }
        return failed_mcp_result(
            approval_key,
            json!({"error": "unknown MCP tool", "tool_name": request.tool_name}),
        );
    };

    if !request.approved {
        return blocked_mcp_result(approval_key, "MCP tool calls require explicit approval.");
    }

    match request.tool_name.as_str() {
        "mock.echo" => completed_mcp_result(
            approval_key,
            json!({"echo": redact_mcp_value(request.args.clone())}),
        ),
        "mock.sum" => completed_mcp_result(
            approval_key,
            json!({"sum": sum_numeric_args(&request.args)}),
        ),
        "mock.fail" => failed_mcp_result(
            approval_key,
            json!({"error": "mock MCP failure", "tool_name": "mock.fail"}),
        ),
        "mock.large_output" => {
            let payload = format!("mcp-large-output:{}", "x".repeat(8192));
            completed_mcp_result(
                approval_key,
                json!({
                    "text": payload,
                    "byte_count": payload.len()
                }),
            )
        }
        "mock.external_effect" => completed_mcp_result(
            approval_key,
            json!({
                "effect": "simulated_external_effect",
                "committed": false
            }),
        ),
        _ => failed_mcp_result(
            approval_key,
            json!({"error": "unsupported MCP tool", "tool_name": request.tool_name}),
        ),
    }
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
    if manifest.operations.is_empty() {
        errors.push("at least one operation is required".to_owned());
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

fn bool_field(value: Option<&Value>) -> bool {
    value.and_then(Value::as_bool).unwrap_or(false)
}

fn default_true() -> bool {
    true
}

fn mock_mcp_operation(
    name: &str,
    description: &str,
    risk: RiskLevel,
    side_effect: SideEffectLevel,
) -> McpManifestOperation {
    McpManifestOperation {
        name: name.to_owned(),
        description: description.to_owned(),
        risk,
        side_effect,
        enabled_by_default: false,
    }
}

fn completed_mcp_result(approval_key: String, output: Value) -> McpToolCallResult {
    McpToolCallResult {
        status: "completed".to_owned(),
        requires_approval: false,
        approval_key,
        output,
        evidence_ref: None,
    }
}

fn blocked_mcp_result(approval_key: String, reason: &str) -> McpToolCallResult {
    McpToolCallResult {
        status: "blocked".to_owned(),
        requires_approval: true,
        approval_key,
        output: json!({"reason": reason}),
        evidence_ref: None,
    }
}

fn failed_mcp_result(approval_key: String, output: Value) -> McpToolCallResult {
    McpToolCallResult {
        status: "failed".to_owned(),
        requires_approval: false,
        approval_key,
        output,
        evidence_ref: None,
    }
}

fn sum_numeric_args(value: &Value) -> f64 {
    match value {
        Value::Array(items) => items.iter().filter_map(Value::as_f64).sum(),
        Value::Object(object) => object.values().filter_map(Value::as_f64).sum(),
        other => other.as_f64().unwrap_or(0.0),
    }
}

fn redact_mcp_value(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| {
                    let value = if is_secret_like_key(&key) {
                        Value::String("[REDACTED]".to_owned())
                    } else {
                        redact_mcp_value(value)
                    };
                    (key, value)
                })
                .collect::<Map<String, Value>>(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(redact_mcp_value).collect()),
        other => other,
    }
}

fn is_secret_like_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    normalized.contains("api_key")
        || normalized.contains("apikey")
        || normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("authorization")
        || normalized.contains("cookie")
        || normalized.contains("private_key")
}

fn max_risk(left: RiskLevel, right: RiskLevel) -> RiskLevel {
    if left.rank() >= right.rank() {
        left
    } else {
        right
    }
}

pub fn tool_registry_entries() -> Vec<ToolRegistryEntry> {
    let planner_harnesses = [
        "conversation-harness",
        "planner-order-harness",
        "planner-decision-harness",
        "final-report-harness",
    ];
    let code_worker_harnesses = ["task-execution-harness", "code-worker-harness"];
    planner_tool_capabilities()
        .into_iter()
        .map(|capability| {
            tool_registry_entry(capability, "Planner harness tool", &planner_harnesses)
        })
        .chain(
            code_worker_tool_capabilities()
                .into_iter()
                .map(|capability| {
                    tool_registry_entry(
                        capability,
                        "Code worker harness tool",
                        &code_worker_harnesses,
                    )
                }),
        )
        .collect()
}

pub fn planner_tool_capabilities() -> Vec<ToolCapability> {
    vec![
        tool_capability(
            "inspect_workflow",
            "workflow",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "inspect_project_summary",
            "context",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "inspect_artifact",
            "runtime_state",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "inspect_run_state",
            "runtime_state",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "inspect_round_summary",
            "runtime_state",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "inspect_evidence",
            "evidence",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "inspect_skill_index",
            "skills",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "inspect_memory",
            "memory",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "read_skill_index",
            "skills",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "search_workflow_memory",
            "memory",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "search_project_memory",
            "memory",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "validate_run_contract_draft",
            "artifacts",
            SideEffectLevel::None,
            RiskLevel::Low,
        ),
        tool_capability(
            "validate_planner_order",
            "artifacts",
            SideEffectLevel::None,
            RiskLevel::Low,
        ),
        tool_capability(
            "validate_planner_decision",
            "artifacts",
            SideEffectLevel::None,
            RiskLevel::Low,
        ),
        tool_capability(
            "build_final_report",
            "artifacts",
            SideEffectLevel::None,
            RiskLevel::Low,
        ),
        tool_capability(
            "estimate_risk",
            "runtime_policy",
            SideEffectLevel::None,
            RiskLevel::Low,
        ),
        tool_capability(
            "estimate_budget",
            "runtime_policy",
            SideEffectLevel::None,
            RiskLevel::Low,
        ),
    ]
}

pub fn code_worker_tool_capabilities() -> Vec<ToolCapability> {
    vec![
        tool_capability(
            "read_file",
            "filesystem",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "search_files",
            "filesystem",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "inspect_git_diff",
            "git",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "propose_patch",
            "filesystem",
            SideEffectLevel::Write,
            RiskLevel::Medium,
        ),
        tool_capability(
            "apply_patch_sandbox",
            "filesystem",
            SideEffectLevel::Write,
            RiskLevel::Medium,
        ),
        tool_capability(
            "run_command_sandbox",
            "commands",
            SideEffectLevel::External,
            RiskLevel::Medium,
        ),
        tool_capability(
            "command_background",
            "commands",
            SideEffectLevel::External,
            RiskLevel::Medium,
        ),
        tool_capability(
            "read_command_output",
            "runtime_state",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "cancel_command_background",
            "commands",
            SideEffectLevel::External,
            RiskLevel::Medium,
        ),
        tool_capability(
            "agent_subagent",
            "agents",
            SideEffectLevel::External,
            RiskLevel::Medium,
        ),
        tool_capability(
            "read_subagent_status",
            "runtime_state",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability("skill", "skills", SideEffectLevel::Read, RiskLevel::Low),
        tool_capability(
            "cancel_subagent_background",
            "agents",
            SideEffectLevel::External,
            RiskLevel::Medium,
        ),
        tool_capability(
            "read_tool_output",
            "runtime_state",
            SideEffectLevel::Read,
            RiskLevel::Low,
        ),
        tool_capability(
            "return_execution_result",
            "artifacts",
            SideEffectLevel::None,
            RiskLevel::Low,
        ),
    ]
}

fn tool_registry_entry(
    capability: ToolCapability,
    description_prefix: &str,
    harness_ids: &[&str],
) -> ToolRegistryEntry {
    let requires_approval = capability.risk != RiskLevel::Low
        || matches!(
            capability.side_effect,
            SideEffectLevel::Write | SideEffectLevel::External
        );
    ToolRegistryEntry {
        description: format!("{description_prefix}: {}.", capability.name),
        required_permission: required_permission_for_tool(&capability).to_owned(),
        approval_behavior: approval_behavior_for_tool(&capability, requires_approval).to_owned(),
        evidence_emitted: evidence_for_tool(&capability).to_owned(),
        timeline_item: timeline_item_for_tool(&capability).to_owned(),
        capability,
        harness_ids: harness_ids.iter().map(|id| (*id).to_owned()).collect(),
        enabled_by_default: true,
        requires_approval,
    }
}

fn required_permission_for_tool(capability: &ToolCapability) -> &'static str {
    match capability.name.as_str() {
        "propose_patch" | "apply_patch_sandbox" => "write_files",
        "run_command_sandbox" | "command_background" | "cancel_command_background" => {
            "run_commands"
        }
        "agent_subagent" | "cancel_subagent_background" => "child_harness_permissions",
        "inspect_skill_index" | "read_skill_index" | "skill" => "read_files",
        name if name.contains("memory") => "memory_policy",
        name if name.contains("artifact") || name.contains("report") => "run_artifacts",
        _ if capability.side_effect == SideEffectLevel::Read => "read_files",
        _ => "none",
    }
}

fn approval_behavior_for_tool(
    capability: &ToolCapability,
    requires_approval: bool,
) -> &'static str {
    if requires_approval {
        match capability.name.as_str() {
            "apply_patch_sandbox" => "approval.requested when patch apply is not pre-approved",
            "run_command_sandbox" => "approval.requested for model or risky commands",
            "command_background" => {
                "approval.requested before starting model or risky background commands"
            }
            "cancel_command_background" => {
                "approval.requested before cancelling a running command task"
            }
            "agent_subagent" => {
                "approval.requested before spawning a child agent; child tools are rechecked against the parent harness"
            }
            "cancel_subagent_background" => {
                "approval.requested before cancelling a running child agent task"
            }
            _ => "requires explicit approval before side effects",
        }
    } else {
        "allowed without approval inside the assigned harness"
    }
}

fn evidence_for_tool(capability: &ToolCapability) -> &'static str {
    match capability.name.as_str() {
        "read_file" | "search_files" | "inspect_git_diff" => "repo_evidence",
        "propose_patch" | "apply_patch_sandbox" => "repo_evidence + patch_evidence",
        "run_command_sandbox" | "command_background" => "command_evidence",
        "read_command_output" => "command_output",
        "agent_subagent" | "read_subagent_status" | "cancel_subagent_background" => {
            "subagent_sidechain"
        }
        "inspect_artifact" | "build_final_report" | "return_execution_result" => "artifact/report",
        "inspect_memory" | "search_workflow_memory" | "search_project_memory" => "memory_event",
        "inspect_skill_index" | "read_skill_index" => "skill_summary",
        "skill" => "skill.invoked",
        _ => "structured_runtime_event",
    }
}

fn timeline_item_for_tool(capability: &ToolCapability) -> &'static str {
    match capability.name.as_str() {
        "run_command_sandbox" => "command_execution",
        "command_background" => "background_command_start",
        "read_command_output" => "background_command_output",
        "cancel_command_background" => "background_command_cancel",
        "agent_subagent" => "subagent_start",
        "read_subagent_status" => "subagent_status",
        "cancel_subagent_background" => "subagent_cancel",
        "skill" => "skill_invoked",
        "propose_patch" | "apply_patch_sandbox" => "file_change / approval",
        "read_file" | "search_files" | "inspect_git_diff" => "tool_call",
        "build_final_report" | "return_execution_result" => "final_summary",
        _ => "executor_step / tool_call",
    }
}

fn tool_capability(
    name: &str,
    toolset: &str,
    side_effect: SideEffectLevel,
    risk: RiskLevel,
) -> ToolCapability {
    ToolCapability {
        name: name.to_owned(),
        toolset: toolset.to_owned(),
        side_effect,
        risk,
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
            .any(|error| error == "at least one operation is required"));
    }

    #[test]
    fn mcp_manifest_rejects_unknown_risk_and_side_effect() {
        let risk = validate_mcp_manifest(&json!({
            "server_id": "x",
            "operations": [{"name": "op", "risk": "critical"}]
        }));
        let side_effect = validate_mcp_manifest(&json!({
            "server_id": "x",
            "operations": [{"name": "op", "side_effect": "network"}]
        }));

        assert!(!risk.ok);
        assert!(risk.errors[0].contains("unsupported MCP risk level"));
        assert!(!side_effect.ok);
        assert!(side_effect.errors[0].contains("unsupported MCP side effect"));
    }

    #[test]
    fn mock_mcp_server_is_disabled_and_discovers_required_tools() {
        let servers = mock_mcp_servers();
        let tools = mock_mcp_tools();
        let tool_names = tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].server_id, "local-mock");
        assert!(!servers[0].enabled);
        assert!(servers[0].requires_approval);
        assert!(tool_names.contains("mock.echo"));
        assert!(tool_names.contains("mock.sum"));
        assert!(tool_names.contains("mock.fail"));
        assert!(tool_names.contains("mock.large_output"));
        assert!(tool_names.contains("mock.external_effect"));
        assert!(tools.iter().all(|tool| tool.requires_approval));
    }

    #[test]
    fn mock_mcp_unapproved_call_blocks_with_approval_key() {
        let result = invoke_mock_mcp_tool(&McpToolCallRequest {
            server_id: "local-mock".to_owned(),
            tool_name: "mock.echo".to_owned(),
            args: json!({"message": "hello"}),
            run_id: None,
            approved: false,
        });

        assert_eq!(result.status, "blocked");
        assert!(result.requires_approval);
        assert_eq!(result.approval_key, "mcp:local-mock:mock.echo");
    }

    #[test]
    fn mock_mcp_approved_echo_redacts_secret_keys() {
        let result = invoke_mock_mcp_tool(&McpToolCallRequest {
            server_id: "local-mock".to_owned(),
            tool_name: "mock.echo".to_owned(),
            args: json!({
                "message": "hello",
                "api_key": "sk-test",
                "nested": {"session_token": "token-value"}
            }),
            run_id: None,
            approved: true,
        });

        assert_eq!(result.status, "completed");
        assert_eq!(result.output["echo"]["message"], "hello");
        assert_eq!(result.output["echo"]["api_key"], "[REDACTED]");
        assert_eq!(
            result.output["echo"]["nested"]["session_token"],
            "[REDACTED]"
        );
        assert!(!result.output.to_string().contains("sk-test"));
    }

    #[test]
    fn mock_mcp_sum_and_failure_are_deterministic() {
        let sum = invoke_mock_mcp_tool(&McpToolCallRequest {
            server_id: "local-mock".to_owned(),
            tool_name: "mock.sum".to_owned(),
            args: json!({"a": 2, "b": 3.5, "ignored": "x"}),
            run_id: None,
            approved: true,
        });
        let failure = invoke_mock_mcp_tool(&McpToolCallRequest {
            server_id: "local-mock".to_owned(),
            tool_name: "mock.fail".to_owned(),
            args: json!({}),
            run_id: None,
            approved: true,
        });

        assert_eq!(sum.status, "completed");
        assert_eq!(sum.output["sum"], 5.5);
        assert_eq!(failure.status, "failed");
        assert_eq!(failure.output["tool_name"], "mock.fail");
    }

    #[test]
    fn mock_mcp_unknown_tool_is_safe() {
        let blocked = invoke_mock_mcp_tool(&McpToolCallRequest {
            server_id: "local-mock".to_owned(),
            tool_name: "mock.unknown".to_owned(),
            args: json!({}),
            run_id: None,
            approved: false,
        });
        let failed = invoke_mock_mcp_tool(&McpToolCallRequest {
            server_id: "local-mock".to_owned(),
            tool_name: "mock.unknown".to_owned(),
            args: json!({}),
            run_id: None,
            approved: true,
        });

        assert_eq!(blocked.status, "blocked");
        assert!(blocked.requires_approval);
        assert_eq!(failed.status, "failed");
    }

    #[test]
    fn tool_registry_filters_by_harness_and_marks_risky_tools() {
        let registry = ToolRegistry::default();
        let code_worker_tools = registry.list_tools(Some("code-worker-harness"));
        let names = code_worker_tools
            .iter()
            .map(|entry| entry.capability.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let patch_tool = registry.get_tool("apply_patch_sandbox").unwrap();

        assert!(names.contains("run_command_sandbox"));
        assert!(names.contains("command_background"));
        assert!(names.contains("read_command_output"));
        assert!(names.contains("cancel_command_background"));
        assert!(names.contains("agent_subagent"));
        assert!(names.contains("read_subagent_status"));
        assert!(names.contains("cancel_subagent_background"));
        assert!(names.contains("skill"));
        assert!(!names.contains("inspect_run_state"));
        assert!(patch_tool.requires_approval);
        assert_eq!(patch_tool.required_permission, "write_files");
        assert_eq!(
            patch_tool.evidence_emitted,
            "repo_evidence + patch_evidence"
        );
        assert_eq!(patch_tool.timeline_item, "file_change / approval");
    }

    #[test]
    fn tool_registry_low_risk_read_tools_do_not_require_approval() {
        let registry = ToolRegistry::default();
        let read_file = registry.get_tool("read_file").unwrap();
        let read_command_output = registry.get_tool("read_command_output").unwrap();
        let read_subagent_status = registry.get_tool("read_subagent_status").unwrap();
        let skill = registry.get_tool("skill").unwrap();
        let inspect_workflow = registry.get_tool("inspect_workflow").unwrap();

        assert!(!read_file.requires_approval);
        assert_eq!(read_file.required_permission, "read_files");
        assert_eq!(read_file.timeline_item, "tool_call");
        assert!(!read_command_output.requires_approval);
        assert_eq!(read_command_output.required_permission, "read_files");
        assert_eq!(
            read_command_output.timeline_item,
            "background_command_output"
        );
        assert!(!read_subagent_status.requires_approval);
        assert_eq!(read_subagent_status.required_permission, "read_files");
        assert_eq!(read_subagent_status.timeline_item, "subagent_status");
        assert!(!skill.requires_approval);
        assert_eq!(skill.required_permission, "read_files");
        assert_eq!(skill.evidence_emitted, "skill.invoked");
        assert_eq!(skill.timeline_item, "skill_invoked");
        assert!(!inspect_workflow.requires_approval);
    }

    #[test]
    fn tool_registry_entries_document_boundary_matrix() {
        let registry = ToolRegistry::default();

        for entry in registry.list_tools(None) {
            assert!(
                !entry.required_permission.trim().is_empty(),
                "{} missing required permission",
                entry.capability.name
            );
            assert!(
                !entry.approval_behavior.trim().is_empty(),
                "{} missing approval behavior",
                entry.capability.name
            );
            assert!(
                !entry.evidence_emitted.trim().is_empty(),
                "{} missing evidence mapping",
                entry.capability.name
            );
            assert!(
                !entry.timeline_item.trim().is_empty(),
                "{} missing timeline mapping",
                entry.capability.name
            );
        }
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
