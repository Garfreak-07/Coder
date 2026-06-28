use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub version: u16,
    #[serde(default)]
    pub models: BTreeMap<String, ModelSpec>,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentSpec>,
    #[serde(default)]
    pub harnesses: BTreeMap<String, HarnessSpec>,
    #[serde(default)]
    pub workflows: BTreeMap<String, WorkflowSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub provider: String,
    pub model: String,
    pub base_url_env: Option<String>,
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpec {
    pub role: String,
    pub model: String,
    pub system: String,
    #[serde(default)]
    pub memory: MemoryAccess,
    pub output_contract: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryAccess {
    #[serde(default)]
    pub read: Vec<MemoryScope>,
    #[serde(default)]
    pub write: Vec<MemoryScope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    User,
    Project,
    Agent,
    Workflow,
    Run,
    RepoFacts,
    KnowledgeHints,
    ExternalDocs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessSpec {
    pub backend: String,
    pub openhands: Option<OpenHandsHarnessConfig>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub permissions: PermissionPolicy,
    #[serde(default)]
    pub memory: MemoryAccess,
    #[serde(default)]
    pub verification: VerificationPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenHandsHarnessConfig {
    pub server_url: String,
    pub session_api_key_env: Option<String>,
    pub workspace_mode: Option<String>,
    #[serde(default = "default_prefer_websocket")]
    pub prefer_websocket: bool,
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_max_event_poll_seconds")]
    pub max_event_poll_seconds: u64,
    #[serde(default = "default_max_events")]
    pub max_events: usize,
    #[serde(default = "default_terminal_event_kinds")]
    pub terminal_event_kinds: Vec<String>,
    #[serde(default)]
    pub api_paths: OpenHandsApiPaths,
    #[serde(default)]
    pub run_start_strategy: OpenHandsRunStartStrategy,
}

fn default_prefer_websocket() -> bool {
    true
}

fn default_poll_interval_ms() -> u64 {
    1000
}

fn default_max_event_poll_seconds() -> u64 {
    300
}

fn default_max_events() -> usize {
    1000
}

fn default_terminal_event_kinds() -> Vec<String> {
    [
        "completed",
        "done",
        "finished",
        "failed",
        "error",
        "cancelled",
        "canceled",
        "run.completed",
        "run.failed",
        "run.cancelled",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenHandsApiPaths {
    #[serde(default)]
    pub api_prefix: String,
    #[serde(default = "default_conversations_path")]
    pub conversations_path: String,
    #[serde(default)]
    pub events_search_path: Option<String>,
    #[serde(default)]
    pub run_endpoint_path: Option<String>,
    #[serde(default)]
    pub websocket_path_template: Option<String>,
    #[serde(default)]
    pub auth_header: OpenHandsAuthHeaderMode,
}

impl Default for OpenHandsApiPaths {
    fn default() -> Self {
        Self {
            api_prefix: String::new(),
            conversations_path: default_conversations_path(),
            events_search_path: None,
            run_endpoint_path: None,
            websocket_path_template: None,
            auth_header: OpenHandsAuthHeaderMode::default(),
        }
    }
}

fn default_conversations_path() -> String {
    "/conversations".to_owned()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenHandsAuthHeaderMode {
    #[default]
    AuthorizationBearer,
    XSessionApiKey,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenHandsRunStartStrategy {
    PostRunEndpoint,
    #[default]
    PostUserEventWithRunTrue,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionPolicy {
    #[serde(default = "allow")]
    pub read_files: PermissionDecision,
    #[serde(default = "ask")]
    pub write_files: PermissionDecision,
    #[serde(default = "ask")]
    pub run_commands: PermissionDecision,
    #[serde(default = "deny")]
    pub network: PermissionDecision,
    #[serde(default = "deny")]
    pub secrets: PermissionDecision,
    #[serde(default = "deny")]
    pub publish_external: PermissionDecision,
    #[serde(default = "deny")]
    pub git_commit: PermissionDecision,
    #[serde(default = "deny")]
    pub git_push: PermissionDecision,
    #[serde(default = "deny")]
    pub deploy: PermissionDecision,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        Self {
            read_files: PermissionDecision::Allow,
            write_files: PermissionDecision::Ask,
            run_commands: PermissionDecision::Ask,
            network: PermissionDecision::Deny,
            secrets: PermissionDecision::Deny,
            publish_external: PermissionDecision::Deny,
            git_commit: PermissionDecision::Deny,
            git_push: PermissionDecision::Deny,
            deploy: PermissionDecision::Deny,
        }
    }
}

fn allow() -> PermissionDecision {
    PermissionDecision::Allow
}

fn ask() -> PermissionDecision {
    PermissionDecision::Ask
}

fn deny() -> PermissionDecision {
    PermissionDecision::Deny
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerificationPolicy {
    #[serde(default)]
    pub require_evidence: bool,
    #[serde(default)]
    pub allowed_checks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSpec {
    pub name: String,
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
    #[serde(default)]
    pub nodes: Vec<WorkflowNodeSpec>,
    #[serde(default)]
    pub edges: Vec<WorkflowEdgeSpec>,
    #[serde(default)]
    pub stop: StopPolicy,
}

fn default_max_rounds() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNodeSpec {
    pub id: String,
    pub agent: String,
    pub harness: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEdgeSpec {
    pub from: String,
    pub to: String,
    pub on: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StopPolicy {
    #[serde(default)]
    pub on_status: Vec<String>,
    pub final_report_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationLevel {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub level: ValidationLevel,
    pub code: String,
    pub message: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    pub status: String,
    #[serde(default)]
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    pub fn new(issues: Vec<ValidationIssue>) -> Self {
        let status = if issues
            .iter()
            .any(|issue| issue.level == ValidationLevel::Error)
        {
            "error"
        } else if issues
            .iter()
            .any(|issue| issue.level == ValidationLevel::Warning)
        {
            "warning"
        } else {
            "pass"
        };
        Self {
            status: status.to_owned(),
            issues,
        }
    }

    pub fn is_pass(&self) -> bool {
        self.status == "pass"
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to parse YAML {path}: {source}")]
    Parse {
        path: String,
        source: serde_yaml::Error,
    },
}

pub fn load_project_config(path: impl AsRef<Path>) -> Result<ProjectConfig, ConfigError> {
    let path = path.as_ref();
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })?;
    serde_yaml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.display().to_string(),
        source,
    })
}

pub fn validate_project_config(config: &ProjectConfig) -> ValidationReport {
    let mut issues = Vec::new();

    if config.version != 1 {
        issues.push(error(
            "unsupported_version",
            "config version must be 1",
            "version",
        ));
    }
    if config.workflows.is_empty() {
        issues.push(error(
            "missing_workflows",
            "config must define at least one workflow",
            "workflows",
        ));
    }

    for (agent_id, agent) in &config.agents {
        if !config.models.contains_key(&agent.model) {
            issues.push(error(
                "agent_model_not_found",
                format!(
                    "agent '{agent_id}' references unknown model '{}'",
                    agent.model
                ),
                format!("agents.{agent_id}.model"),
            ));
        }
        if agent.system.trim().is_empty() {
            issues.push(warning(
                "agent_system_empty",
                format!("agent '{agent_id}' has empty system instructions"),
                format!("agents.{agent_id}.system"),
            ));
        }
    }

    for (harness_id, harness) in &config.harnesses {
        if harness.backend == "openhands" && harness.openhands.is_none() {
            issues.push(error(
                "openhands_config_missing",
                format!("harness '{harness_id}' uses openhands backend without openhands config"),
                format!("harnesses.{harness_id}.openhands"),
            ));
        }
    }

    for (workflow_id, workflow) in &config.workflows {
        issues.extend(validate_workflow(
            workflow_id,
            workflow,
            &config.agents,
            &config.harnesses,
        ));
    }

    ValidationReport::new(issues)
}

pub fn validate_workflow(
    workflow_id: &str,
    workflow: &WorkflowSpec,
    agents: &BTreeMap<String, AgentSpec>,
    harnesses: &BTreeMap<String, HarnessSpec>,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    if workflow.name.trim().is_empty() {
        issues.push(error(
            "workflow_name_empty",
            format!("workflow '{workflow_id}' must have a name"),
            format!("workflows.{workflow_id}.name"),
        ));
    }
    if workflow.max_rounds == 0 || workflow.max_rounds > 20 {
        issues.push(error(
            "workflow_max_rounds_out_of_range",
            format!("workflow '{workflow_id}' max_rounds must be between 1 and 20"),
            format!("workflows.{workflow_id}.max_rounds"),
        ));
    }
    if let Some(final_report_agent) = &workflow.stop.final_report_agent {
        if !agents.contains_key(final_report_agent) {
            issues.push(error(
                "workflow_final_report_agent_not_found",
                format!(
                    "workflow '{workflow_id}' final_report_agent '{final_report_agent}' does not exist"
                ),
                format!("workflows.{workflow_id}.stop.final_report_agent"),
            ));
        }
    }
    for status in &workflow.stop.on_status {
        if !is_known_stop_status(status) {
            issues.push(error(
                "workflow_stop_status_unknown",
                format!("workflow '{workflow_id}' stop status '{status}' is not supported"),
                format!("workflows.{workflow_id}.stop.on_status"),
            ));
        }
    }

    let node_ids: std::collections::BTreeSet<&str> =
        workflow.nodes.iter().map(|node| node.id.as_str()).collect();
    if node_ids.len() != workflow.nodes.len() {
        issues.push(error(
            "duplicate_workflow_node",
            format!("workflow '{workflow_id}' contains duplicate node ids"),
            format!("workflows.{workflow_id}.nodes"),
        ));
    }
    if workflow.nodes.is_empty() {
        issues.push(error(
            "workflow_nodes_empty",
            format!("workflow '{workflow_id}' must define at least one node"),
            format!("workflows.{workflow_id}.nodes"),
        ));
    }
    for node in &workflow.nodes {
        if node.id.trim().is_empty() {
            issues.push(error(
                "workflow_node_id_empty",
                format!("workflow '{workflow_id}' contains a node with an empty id"),
                format!("workflows.{workflow_id}.nodes"),
            ));
        }
        if !agents.contains_key(&node.agent) {
            issues.push(error(
                "workflow_node_agent_not_found",
                format!(
                    "workflow '{workflow_id}' node '{}' references unknown agent '{}'",
                    node.id, node.agent
                ),
                format!("workflows.{workflow_id}.nodes.{}", node.id),
            ));
        }
        if !harnesses.contains_key(&node.harness) {
            issues.push(error(
                "workflow_node_harness_not_found",
                format!(
                    "workflow '{workflow_id}' node '{}' references unknown harness '{}'",
                    node.id, node.harness
                ),
                format!("workflows.{workflow_id}.nodes.{}", node.id),
            ));
        }
    }
    for edge in &workflow.edges {
        if edge.on.trim().is_empty() {
            issues.push(error(
                "workflow_edge_condition_empty",
                format!(
                    "workflow '{workflow_id}' edge from '{}' to '{}' must define a transition condition",
                    edge.from, edge.to
                ),
                format!("workflows.{workflow_id}.edges"),
            ));
        } else if !is_known_transition_condition(&edge.on) {
            issues.push(error(
                "workflow_edge_condition_unknown",
                format!(
                    "workflow '{workflow_id}' edge from '{}' to '{}' uses unsupported transition condition '{}'",
                    edge.from, edge.to, edge.on
                ),
                format!("workflows.{workflow_id}.edges"),
            ));
        }
        if !node_ids.contains(edge.from.as_str()) {
            issues.push(error(
                "workflow_edge_source_not_found",
                format!(
                    "workflow '{workflow_id}' edge source '{}' does not exist",
                    edge.from
                ),
                format!("workflows.{workflow_id}.edges"),
            ));
        }
        if !node_ids.contains(edge.to.as_str()) {
            issues.push(error(
                "workflow_edge_target_not_found",
                format!(
                    "workflow '{workflow_id}' edge target '{}' does not exist",
                    edge.to
                ),
                format!("workflows.{workflow_id}.edges"),
            ));
        }
    }
    issues
}

fn is_known_transition_condition(condition: &str) -> bool {
    matches!(
        condition,
        "ready" | "completed" | "blocked" | "failed" | "cancelled" | "continue" | "finish"
    )
}

fn is_known_stop_status(status: &str) -> bool {
    matches!(
        status,
        "completed" | "blocked" | "failed" | "cancelled" | "max_rounds"
    )
}

fn error(
    code: impl Into<String>,
    message: impl Into<String>,
    target: impl Into<String>,
) -> ValidationIssue {
    ValidationIssue {
        level: ValidationLevel::Error,
        code: code.into(),
        message: message.into(),
        target: target.into(),
    }
}

fn warning(
    code: impl Into<String>,
    message: impl Into<String>,
    target: impl Into<String>,
) -> ValidationIssue {
    ValidationIssue {
        level: ValidationLevel::Warning,
        code: code.into(),
        message: message.into(),
        target: target.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_config_passes() {
        let config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        let report = validate_project_config(&config);
        assert_eq!(report.status, "pass");
    }

    #[test]
    fn invalid_edge_reference_is_reported() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config
            .workflows
            .get_mut("planner-led")
            .unwrap()
            .edges
            .push(WorkflowEdgeSpec {
                from: "planner".to_owned(),
                to: "missing".to_owned(),
                on: "completed".to_owned(),
            });

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "workflow_edge_target_not_found"));
    }

    #[test]
    fn invalid_stop_policy_is_reported() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        let workflow = config.workflows.get_mut("planner-led").unwrap();
        workflow.stop.final_report_agent = Some("missing".to_owned());
        workflow.stop.on_status.push("mystery".to_owned());

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "workflow_final_report_agent_not_found"));
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "workflow_stop_status_unknown"));
    }

    #[test]
    fn invalid_transition_condition_is_reported() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config
            .workflows
            .get_mut("planner-led")
            .unwrap()
            .edges
            .push(WorkflowEdgeSpec {
                from: "planner".to_owned(),
                to: "executor".to_owned(),
                on: "maybe".to_owned(),
            });

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "workflow_edge_condition_unknown"));
    }
}
