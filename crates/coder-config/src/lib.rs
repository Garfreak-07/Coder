use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use coder_tools::canonical_builtin_tool_name;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod validation;

mod permissions;

pub use permissions::{
    apply_permission_updates_to_policy, apply_permission_updates_to_settings, evaluate_permission,
    permission_decision, permission_policy_explanation, permission_policy_rules,
    permission_settings_update_applied, permission_update_application_applied,
    permission_update_destination_supports_persistence, PermissionDecisionReason,
    PermissionEvaluation, PermissionMode, PermissionRule, PermissionRuleSource,
    PermissionRuleValue, PermissionSettingsRecord, PermissionSettingsRules,
    PermissionSettingsUpdateApplication, PermissionUpdate, PermissionUpdateApplication,
    PermissionUpdateDestination, PERMISSION_FIELDS,
};
pub use validation::{validate_project_config, validate_workflow};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub version: u16,
    #[serde(default, alias = "disableAllHooks")]
    pub disable_all_hooks: bool,
    #[serde(default, alias = "allowedWebhookUrls")]
    pub allowed_webhook_urls: Option<Vec<String>>,
    #[serde(default, alias = "webhookAllowedEnvVars")]
    pub webhook_allowed_env_vars: Option<Vec<String>>,
    #[serde(default)]
    pub hooks: HookSettings,
    #[serde(default)]
    pub models: BTreeMap<String, ModelSpec>,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentSpec>,
    #[serde(default)]
    pub harnesses: BTreeMap<String, HarnessSpec>,
    #[serde(default)]
    pub surface_bindings: SurfaceBindings,
    #[serde(default)]
    pub workflows: BTreeMap<String, WorkflowSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SurfaceBindings {
    #[serde(default)]
    pub planner_chat: Option<AgentHarnessBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHarnessBinding {
    pub agent: String,
    pub harness: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookSettings {
    #[serde(default, rename = "PreToolUse")]
    pub pre_tool_use: Vec<HookMatcherSpec>,
    #[serde(default, rename = "PostToolUse")]
    pub post_tool_use: Vec<HookMatcherSpec>,
    #[serde(default, rename = "PostToolUseFailure")]
    pub post_tool_use_failure: Vec<HookMatcherSpec>,
}

impl HookSettings {
    pub fn matchers_for_event(&self, event: HookEvent) -> &[HookMatcherSpec] {
        match event {
            HookEvent::PreToolUse => &self.pre_tool_use,
            HookEvent::PostToolUse => &self.post_tool_use,
            HookEvent::PostToolUseFailure => &self.post_tool_use_failure,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pre_tool_use.is_empty()
            && self.post_tool_use.is_empty()
            && self.post_tool_use_failure.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookMatcherSpec {
    #[serde(default)]
    pub matcher: Option<String>,
    #[serde(default)]
    pub hooks: Vec<HookCommandSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookCommandSpec {
    Command {
        command: String,
        #[serde(default, rename = "if")]
        if_condition: Option<String>,
        #[serde(default)]
        shell: Option<String>,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default, alias = "statusMessage")]
        status_message: Option<String>,
        #[serde(default)]
        once: bool,
        #[serde(default, rename = "async")]
        run_async: bool,
        #[serde(default, alias = "asyncRewake")]
        async_rewake: bool,
    },
    Prompt {
        prompt: String,
        #[serde(default, rename = "if")]
        if_condition: Option<String>,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default, alias = "statusMessage")]
        status_message: Option<String>,
        #[serde(default)]
        once: bool,
    },
    Agent {
        prompt: String,
        #[serde(default, rename = "if")]
        if_condition: Option<String>,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default, alias = "statusMessage")]
        status_message: Option<String>,
        #[serde(default)]
        once: bool,
    },
    Webhook {
        url: String,
        #[serde(default, rename = "if")]
        if_condition: Option<String>,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default, alias = "allowedEnvVars")]
        allowed_env_vars: Vec<String>,
        #[serde(default, alias = "statusMessage")]
        status_message: Option<String>,
        #[serde(default)]
        once: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub provider: String,
    pub model: String,
    pub base_url_env: Option<String>,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub context_window_tokens: Option<u32>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub auto_compact_token_limit: Option<u32>,
    #[serde(default)]
    pub effective_context_window_percent: Option<u8>,
    #[serde(default)]
    pub supports_streaming: Option<bool>,
    #[serde(default)]
    pub supports_tool_calls: Option<bool>,
    #[serde(default)]
    pub supports_parallel_tool_calls: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedModelCapabilities {
    pub context_window_tokens: u32,
    pub max_output_tokens: u32,
    pub auto_compact_token_limit: u32,
    pub effective_context_window_percent: u8,
    pub supports_streaming: bool,
    pub supports_tool_calls: bool,
    pub supports_parallel_tool_calls: bool,
}

impl ModelSpec {
    pub fn resolved_capabilities(&self) -> ResolvedModelCapabilities {
        let defaults = default_model_capabilities(&self.provider, &self.model);
        let context_window_tokens = self
            .capabilities
            .context_window_tokens
            .unwrap_or(defaults.context_window_tokens);
        let codex_auto_compact_limit = context_window_tokens.saturating_mul(9) / 10;
        ResolvedModelCapabilities {
            context_window_tokens,
            max_output_tokens: self
                .capabilities
                .max_output_tokens
                .unwrap_or(defaults.max_output_tokens),
            auto_compact_token_limit: self
                .capabilities
                .auto_compact_token_limit
                .unwrap_or(codex_auto_compact_limit)
                .min(codex_auto_compact_limit),
            effective_context_window_percent: self
                .capabilities
                .effective_context_window_percent
                .unwrap_or(MODEL_EFFECTIVE_CONTEXT_WINDOW_PERCENT_DEFAULT),
            supports_streaming: self
                .capabilities
                .supports_streaming
                .unwrap_or(defaults.supports_streaming),
            supports_tool_calls: self
                .capabilities
                .supports_tool_calls
                .unwrap_or(defaults.supports_tool_calls),
            supports_parallel_tool_calls: self
                .capabilities
                .supports_parallel_tool_calls
                .unwrap_or(defaults.supports_parallel_tool_calls),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpec {
    pub role: String,
    pub model: String,
    pub system: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default, alias = "disallowedTools", alias = "disallowed-tools")]
    pub disallowed_tools: Vec<String>,
    #[serde(default)]
    pub memory: MemoryAccess,
    pub output_contract: String,
    #[serde(default)]
    pub runtime: AgentRuntimePolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRuntimePolicy {
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub compact_output_reserve_tokens: Option<u32>,
    #[serde(default = "default_max_output_recovery_attempts")]
    pub max_output_recovery_attempts: u8,
    #[serde(default = "default_max_consecutive_compaction_failures")]
    pub max_consecutive_compaction_failures: u8,
}

impl Default for AgentRuntimePolicy {
    fn default() -> Self {
        Self {
            max_output_tokens: None,
            max_turns: None,
            effort: None,
            compact_output_reserve_tokens: None,
            max_output_recovery_attempts: AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_DEFAULT,
            max_consecutive_compaction_failures: AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_DEFAULT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentRuntimePolicy {
    pub max_output_tokens: u32,
    pub max_turns: u32,
    pub effort: Option<String>,
    pub context_window_tokens: u32,
    pub effective_context_window_tokens: u32,
    pub compact_output_reserve_tokens: u32,
    pub auto_compact_token_limit: u32,
    pub max_output_recovery_attempts: u8,
    pub max_consecutive_compaction_failures: u8,
    pub supports_streaming: bool,
    pub supports_tool_calls: bool,
    pub supports_parallel_tool_calls: bool,
}

pub fn resolve_agent_runtime_policy(
    model: &ModelSpec,
    runtime: &AgentRuntimePolicy,
) -> ResolvedAgentRuntimePolicy {
    let capabilities = model.resolved_capabilities();
    let max_output_tokens = runtime
        .max_output_tokens
        .unwrap_or(capabilities.max_output_tokens)
        .min(capabilities.max_output_tokens);
    let compact_output_reserve_tokens = runtime
        .compact_output_reserve_tokens
        .unwrap_or(AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_DEFAULT.min(max_output_tokens))
        .min(max_output_tokens);
    ResolvedAgentRuntimePolicy {
        max_output_tokens,
        max_turns: runtime.max_turns.unwrap_or({
            if capabilities.max_output_tokens >= 32_000 {
                16
            } else {
                24
            }
        }),
        effort: runtime.effort.clone(),
        context_window_tokens: capabilities.context_window_tokens,
        effective_context_window_tokens: capabilities
            .context_window_tokens
            .saturating_mul(u32::from(capabilities.effective_context_window_percent))
            / 100,
        compact_output_reserve_tokens,
        auto_compact_token_limit: capabilities.auto_compact_token_limit,
        max_output_recovery_attempts: runtime.max_output_recovery_attempts,
        max_consecutive_compaction_failures: runtime.max_consecutive_compaction_failures,
        supports_streaming: capabilities.supports_streaming,
        supports_tool_calls: capabilities.supports_tool_calls,
        supports_parallel_tool_calls: capabilities.supports_parallel_tool_calls,
    }
}

pub const AGENT_MAX_OUTPUT_TOKENS_MIN: u32 = 256;
pub const AGENT_MAX_OUTPUT_TOKENS_MAX: u32 = 64_000;
pub const AGENT_EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
pub const MODEL_CONTEXT_WINDOW_TOKENS_DEFAULT: u32 = 200_000;
pub const MODEL_CONTEXT_WINDOW_TOKENS_MIN: u32 = 32_000;
pub const MODEL_CONTEXT_WINDOW_TOKENS_MAX: u32 = 1_000_000;
pub const MODEL_EFFECTIVE_CONTEXT_WINDOW_PERCENT_DEFAULT: u8 = 95;
pub const MODEL_EFFECTIVE_CONTEXT_WINDOW_PERCENT_MIN: u8 = 1;
pub const MODEL_EFFECTIVE_CONTEXT_WINDOW_PERCENT_MAX: u8 = 100;
pub const AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_DEFAULT: u32 = 20_000;
pub const AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MIN: u32 = 1_000;
pub const AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MAX: u32 = AGENT_MAX_OUTPUT_TOKENS_MAX;
pub const AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_DEFAULT: u8 = 3;
pub const AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MIN: u8 = 0;
pub const AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MAX: u8 = 10;
pub const AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_DEFAULT: u8 = 3;
pub const AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MIN: u8 = 1;
pub const AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MAX: u8 = 10;

fn default_model_capabilities(provider: &str, model: &str) -> ResolvedModelCapabilities {
    let provider = provider.trim().to_ascii_lowercase();
    let model = model.trim().to_ascii_lowercase();
    let (context_window_tokens, max_output_tokens) = if provider == "deepseek" {
        (
            128_000,
            if model.contains("reasoner") {
                64_000
            } else {
                8_000
            },
        )
    } else {
        (
            MODEL_CONTEXT_WINDOW_TOKENS_DEFAULT,
            AGENT_MAX_OUTPUT_TOKENS_MAX,
        )
    };
    ResolvedModelCapabilities {
        context_window_tokens,
        max_output_tokens,
        auto_compact_token_limit: context_window_tokens.saturating_mul(9) / 10,
        effective_context_window_percent: MODEL_EFFECTIVE_CONTEXT_WINDOW_PERCENT_DEFAULT,
        supports_streaming: true,
        supports_tool_calls: true,
        supports_parallel_tool_calls: true,
    }
}

fn default_max_output_recovery_attempts() -> u8 {
    AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_DEFAULT
}

fn default_max_consecutive_compaction_failures() -> u8 {
    AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_DEFAULT
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
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub permissions: PermissionPolicy,
    #[serde(default)]
    pub memory: MemoryAccess,
    #[serde(default)]
    pub verification: VerificationPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentToolResolution {
    pub selected_tools: Vec<String>,
    pub requested_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub invalid_requested_tools: Vec<String>,
    pub ignored_disallowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_agent_types: Option<Vec<String>>,
    pub wildcard: bool,
}

pub fn resolve_agent_tools(agent: &AgentSpec, harness: &HarnessSpec) -> AgentToolResolution {
    let requested_specs = parsed_tool_specs(&agent.tools);
    let requested_tools = requested_specs
        .iter()
        .map(|spec| spec.tool_name.clone())
        .collect::<Vec<_>>();
    let disallowed_tools = normalized_tool_specs(&agent.disallowed_tools);
    let disallowed_set = disallowed_tools.iter().cloned().collect::<BTreeSet<_>>();
    let harness_specs = parsed_tool_specs(&harness.tools);
    let harness_tool_names = harness_specs
        .iter()
        .map(|tool| tool.tool_name.clone())
        .collect::<BTreeSet<_>>();
    let wildcard = requested_tools.is_empty()
        || (requested_tools.len() == 1 && requested_tools.first().is_some_and(|tool| tool == "*"));

    let mut selected_tools = Vec::new();
    let mut seen_selected = BTreeSet::new();
    let mut selected_source_specs = Vec::new();
    let mut invalid_requested_tools = Vec::new();
    if wildcard {
        for tool in &harness_specs {
            let tool_name = &tool.tool_name;
            if disallowed_set.contains(tool_name) {
                continue;
            }
            if seen_selected.insert(tool_name.clone()) {
                selected_tools.push(tool_name.clone());
                selected_source_specs.push(tool.clone());
            }
        }
    } else {
        for requested_tool in &requested_specs {
            if disallowed_set.contains(&requested_tool.tool_name)
                || !harness_tool_names.contains(&requested_tool.tool_name)
            {
                invalid_requested_tools.push(requested_tool.tool_name.clone());
                continue;
            }
            if seen_selected.insert(requested_tool.tool_name.clone()) {
                selected_tools.push(requested_tool.tool_name.clone());
                selected_source_specs.push(requested_tool.clone());
            }
        }
    }

    let ignored_disallowed_tools = disallowed_tools
        .iter()
        .filter(|tool| !harness_tool_names.contains(*tool))
        .cloned()
        .collect();
    let allowed_agent_types =
        resolve_allowed_agent_types(&selected_tools, &selected_source_specs, &harness_specs);

    AgentToolResolution {
        selected_tools,
        requested_tools,
        disallowed_tools,
        invalid_requested_tools,
        ignored_disallowed_tools,
        allowed_agent_types,
        wildcard,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    pub tool_name: String,
    pub rule_content: Option<String>,
}

pub fn parse_tool_spec(tool_spec: &str) -> Option<ToolSpec> {
    let trimmed = tool_spec.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (tool_name, rule_content) = if let Some((name, rest)) = trimmed.split_once('(') {
        let content = rest.strip_suffix(')').unwrap_or(rest).trim();
        (
            name.trim(),
            (!content.is_empty()).then(|| content.to_owned()),
        )
    } else {
        (trimmed, None)
    };
    let tool_name = canonical_config_tool_name(tool_name);
    (!tool_name.is_empty()).then_some(ToolSpec {
        tool_name,
        rule_content,
    })
}

pub fn normalized_tool_name(tool_spec: &str) -> Option<String> {
    parse_tool_spec(tool_spec).map(|spec| spec.tool_name)
}

fn normalized_tool_specs(tool_specs: &[String]) -> Vec<String> {
    parsed_tool_specs(tool_specs)
        .into_iter()
        .map(|tool| tool.tool_name)
        .collect()
}

fn parsed_tool_specs(tool_specs: &[String]) -> Vec<ToolSpec> {
    tool_specs
        .iter()
        .filter_map(|tool| parse_tool_spec(tool))
        .collect()
}

fn canonical_config_tool_name(tool_name: &str) -> String {
    canonical_builtin_tool_name(tool_name)
        .unwrap_or(tool_name)
        .to_owned()
}

fn resolve_allowed_agent_types(
    selected_tools: &[String],
    selected_source_specs: &[ToolSpec],
    harness_specs: &[ToolSpec],
) -> Option<Vec<String>> {
    if !selected_tools.iter().any(|tool| tool == "agent_subagent") {
        return None;
    }
    let agent_restrictions = collect_agent_type_restrictions(selected_source_specs);
    let harness_restrictions = collect_agent_type_restrictions(harness_specs);
    match (agent_restrictions, harness_restrictions) {
        (Some(agent), Some(harness)) => Some(intersect_preserving_order(agent, &harness)),
        (Some(agent), None) => Some(agent),
        (None, Some(harness)) => Some(harness),
        (None, None) => None,
    }
}

fn collect_agent_type_restrictions(specs: &[ToolSpec]) -> Option<Vec<String>> {
    let mut allowed = Vec::new();
    let mut seen = BTreeSet::new();
    for spec in specs
        .iter()
        .filter(|spec| spec.tool_name == "agent_subagent")
        .filter_map(|spec| spec.rule_content.as_deref())
    {
        for agent_type in spec
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            if seen.insert(agent_type.to_owned()) {
                allowed.push(agent_type.to_owned());
            }
        }
    }
    (!allowed.is_empty()).then_some(allowed)
}

fn intersect_preserving_order(left: Vec<String>, right: &[String]) -> Vec<String> {
    let right = right.iter().cloned().collect::<BTreeSet<_>>();
    left.into_iter()
        .filter(|item| right.contains(item))
        .collect()
}

pub const WORKFLOW_MAX_ROUNDS_DEFAULT: u32 = 3;
pub const WORKFLOW_MAX_ROUNDS_MIN: u32 = 1;
pub const WORKFLOW_MAX_ROUNDS_MAX: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionPolicy {
    #[serde(default)]
    pub mode: PermissionMode,
    #[serde(default = "allow")]
    pub read_files: PermissionDecision,
    #[serde(default = "ask")]
    pub write_files: PermissionDecision,
    #[serde(default = "ask")]
    pub run_commands: PermissionDecision,
    #[serde(default = "ask")]
    pub child_harness_permissions: PermissionDecision,
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
            mode: PermissionMode::Default,
            read_files: PermissionDecision::Allow,
            write_files: PermissionDecision::Ask,
            run_commands: PermissionDecision::Ask,
            child_harness_permissions: PermissionDecision::Ask,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub nodes: Vec<WorkflowNodeSpec>,
    #[serde(default)]
    pub edges: Vec<WorkflowEdgeSpec>,
    #[serde(default)]
    pub stop: StopPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedWorkflowCostPolicy {
    pub token_budget: u64,
    pub token_budget_source: String,
    pub model_id: String,
    pub provider: String,
    pub model: String,
    pub default_max_turns: u32,
    pub max_rounds: u32,
}

pub fn resolve_workflow_cost_policy(
    config: &ProjectConfig,
    workflow_id: &str,
) -> Option<ResolvedWorkflowCostPolicy> {
    let workflow = config.workflows.get(workflow_id)?;
    let node = workflow
        .nodes
        .iter()
        .find(|node| {
            config
                .agents
                .get(&node.agent)
                .is_some_and(|agent| agent.role == "executor")
        })
        .or_else(|| workflow.nodes.first())?;
    let agent = config.agents.get(&node.agent)?;
    let model = config.models.get(&agent.model)?;
    let runtime = resolve_agent_runtime_policy(model, &agent.runtime);
    let derived_budget = (u64::from(runtime.context_window_tokens)
        .saturating_add(u64::from(runtime.max_output_tokens).saturating_mul(2)))
    .saturating_mul(u64::from(workflow.max_rounds.max(1)))
    .clamp(64_000, 2_000_000);
    Some(ResolvedWorkflowCostPolicy {
        token_budget: workflow.token_budget.unwrap_or(derived_budget),
        token_budget_source: if workflow.token_budget.is_some() {
            "configured"
        } else {
            "model_capability_default"
        }
        .to_owned(),
        model_id: agent.model.clone(),
        provider: model.provider.clone(),
        model: model.model.clone(),
        default_max_turns: runtime.max_turns,
        max_rounds: workflow.max_rounds,
    })
}

fn default_max_rounds() -> u32 {
    WORKFLOW_MAX_ROUNDS_DEFAULT
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
    fn workflow_token_budget_is_optional_but_must_be_positive() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config
            .workflows
            .get_mut("planner-led")
            .unwrap()
            .token_budget = Some(100_000);
        assert_eq!(validate_project_config(&config).status, "pass");

        config
            .workflows
            .get_mut("planner-led")
            .unwrap()
            .token_budget = Some(0);
        let report = validate_project_config(&config);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "workflow_token_budget_zero"));
    }

    #[test]
    fn claude_style_hooks_configuration_is_accepted() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config.hooks = serde_yaml::from_str::<HookSettings>(
            r#"
PreToolUse:
  - matcher: command_run|repo_read_file
    hooks:
      - type: command
        command: echo checking
        timeout: 5
PostToolUse:
  - matcher: "*"
    hooks:
      - type: webhook
        url: http://127.0.0.1:8765/hooks
"#,
        )
        .unwrap();

        let report = validate_project_config(&config);

        assert_eq!(report.status, "pass");
    }

    #[test]
    fn webhook_hook_configuration_is_accepted() {
        let config_text = format!(
            "{}\nallowedWebhookUrls:\n  - https://hooks.example.com/*\nwebhookAllowedEnvVars:\n  - CODER_WEBHOOK_TOKEN\n",
            include_str!("../../../examples/coder.yaml")
        );
        let mut config: ProjectConfig = serde_yaml::from_str(&config_text).unwrap();
        config.hooks = serde_yaml::from_str::<HookSettings>(
            r#"
PreToolUse:
  - matcher: command_run
    hooks:
      - type: webhook
        url: https://hooks.example.com/coder
        headers:
          Authorization: Bearer $CODER_WEBHOOK_TOKEN
        allowedEnvVars:
          - CODER_WEBHOOK_TOKEN
"#,
        )
        .unwrap();

        let report = validate_project_config(&config);

        assert_eq!(report.status, "pass");
        assert_eq!(
            config.allowed_webhook_urls.as_deref(),
            Some(&["https://hooks.example.com/*".to_owned()][..])
        );
        assert_eq!(
            config.webhook_allowed_env_vars.as_deref(),
            Some(&["CODER_WEBHOOK_TOKEN".to_owned()][..])
        );
    }

    #[test]
    fn external_http_webhook_urls_are_rejected() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config.hooks = serde_yaml::from_str::<HookSettings>(
            r#"
PreToolUse:
  - matcher: command_run
    hooks:
      - type: webhook
        url: http://hooks.example.com/coder
"#,
        )
        .unwrap();

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report.issues.iter().any(|issue| {
            issue.code == "hook_webhook_url_invalid"
                && issue.message.contains("must use https://")
                && issue.target.ends_with(".url")
        }));
    }

    #[test]
    fn loopback_http_webhook_urls_are_accepted_for_local_development() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config.hooks = serde_yaml::from_str::<HookSettings>(
            r#"
PreToolUse:
  - matcher: command_run
    hooks:
      - type: webhook
        url: http://localhost:8765/hooks
PostToolUse:
  - matcher: repo_read_file
    hooks:
      - type: webhook
        url: http://[::1]:8765/hooks
"#,
        )
        .unwrap();

        let report = validate_project_config(&config);

        assert_eq!(report.status, "pass");
    }

    #[test]
    fn invalid_hooks_configuration_is_reported() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config.hooks = serde_yaml::from_str::<HookSettings>(
            r#"
PreToolUse:
  - matcher: command_run
    hooks:
      - type: command
        command: ""
        timeout: 0
PostToolUse:
  - matcher: repo_read_file
    hooks: []
"#,
        )
        .unwrap();

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        for code in [
            "hook_command_empty",
            "hook_timeout_out_of_range",
            "hook_matcher_hooks_empty",
        ] {
            assert!(
                report.issues.iter().any(|issue| issue.code == code),
                "missing {code}"
            );
        }
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

    #[test]
    fn non_planner_agents_cannot_request_long_term_memory_scopes() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config
            .agents
            .get_mut("executor")
            .unwrap()
            .memory
            .read
            .push(MemoryScope::Project);

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "agent_long_term_memory_for_non_planner"));
    }

    #[test]
    fn execution_harnesses_cannot_request_long_term_memory_scopes() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config
            .harnesses
            .get_mut("native-code-edit")
            .unwrap()
            .memory
            .read
            .push(MemoryScope::Project);

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "harness_long_term_memory_for_execution_backend"));
    }

    #[test]
    fn agent_runtime_policy_bounds_are_reported() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        let runtime = &mut config.agents.get_mut("executor").unwrap().runtime;
        runtime.max_output_tokens = Some(AGENT_MAX_OUTPUT_TOKENS_MAX + 1);
        runtime.max_turns = Some(0);
        runtime.effort = Some("ultracode".to_owned());
        runtime.compact_output_reserve_tokens = Some(AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MAX + 1);
        runtime.max_output_recovery_attempts = AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MAX + 1;
        runtime.max_consecutive_compaction_failures =
            AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MAX + 1;
        let capabilities = &mut config.models.get_mut("default").unwrap().capabilities;
        capabilities.context_window_tokens = Some(MODEL_CONTEXT_WINDOW_TOKENS_MIN - 1);
        capabilities.auto_compact_token_limit = Some(0);
        capabilities.effective_context_window_percent = Some(0);

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        for code in [
            "agent_max_output_tokens_out_of_range",
            "agent_max_turns_out_of_range",
            "agent_effort_level_unknown",
            "agent_compact_output_reserve_tokens_out_of_range",
            "agent_max_output_recovery_attempts_out_of_range",
            "agent_max_consecutive_compaction_failures_out_of_range",
            "model_context_window_tokens_out_of_range",
            "model_auto_compact_token_limit_out_of_range",
            "model_effective_context_window_percent_out_of_range",
        ] {
            assert!(
                report.issues.iter().any(|issue| issue.code == code),
                "missing {code}"
            );
        }
    }

    #[test]
    fn model_capabilities_use_provider_defaults_and_codex_context_bounds() {
        let model = |name: &str| ModelSpec {
            provider: "deepseek".to_owned(),
            model: name.to_owned(),
            base_url_env: None,
            api_key_env: None,
            capabilities: ModelCapabilities::default(),
        };

        let chat = model("deepseek-chat").resolved_capabilities();
        assert_eq!(chat.context_window_tokens, 128_000);
        assert_eq!(chat.max_output_tokens, 8_000);
        assert_eq!(chat.auto_compact_token_limit, 115_200);
        assert_eq!(chat.effective_context_window_percent, 95);

        let reasoner = model("deepseek-reasoner").resolved_capabilities();
        assert_eq!(reasoner.context_window_tokens, 128_000);
        assert_eq!(reasoner.max_output_tokens, 64_000);
    }

    #[test]
    fn agent_runtime_overrides_are_clamped_to_model_capabilities() {
        let model = ModelSpec {
            provider: "deepseek".to_owned(),
            model: "deepseek-chat".to_owned(),
            base_url_env: None,
            api_key_env: None,
            capabilities: ModelCapabilities::default(),
        };
        let runtime = resolve_agent_runtime_policy(
            &model,
            &AgentRuntimePolicy {
                max_output_tokens: Some(64_000),
                compact_output_reserve_tokens: Some(20_000),
                ..AgentRuntimePolicy::default()
            },
        );

        assert_eq!(runtime.max_output_tokens, 8_000);
        assert_eq!(runtime.compact_output_reserve_tokens, 8_000);
        assert_eq!(runtime.effective_context_window_tokens, 121_600);
        assert_eq!(runtime.auto_compact_token_limit, 115_200);
    }

    #[test]
    fn workflow_cost_policy_is_model_aware_bounded_and_explicitly_overridable() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        let derived = resolve_workflow_cost_policy(&config, "planner-led").unwrap();
        assert_eq!(derived.token_budget, 432_000);
        assert_eq!(derived.token_budget_source, "model_capability_default");
        assert_eq!(derived.default_max_turns, 24);

        config
            .workflows
            .get_mut("planner-led")
            .unwrap()
            .token_budget = Some(90_000);
        let explicit = resolve_workflow_cost_policy(&config, "planner-led").unwrap();
        assert_eq!(explicit.token_budget, 90_000);
        assert_eq!(explicit.token_budget_source, "configured");
    }

    #[test]
    fn agent_tool_resolution_applies_allow_and_disallow_lists() {
        let config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        let mut harness = config.harnesses.get("native-code-edit").unwrap().clone();
        harness.tools.push("agent_subagent".to_owned());
        let mut agent = config.agents.get("executor").unwrap().clone();
        agent.tools = vec![
            "command_run".to_owned(),
            "patch_apply".to_owned(),
            "agent_subagent".to_owned(),
        ];
        agent.disallowed_tools = vec!["patch_apply".to_owned()];

        let resolution = resolve_agent_tools(&agent, &harness);

        assert_eq!(
            resolution.selected_tools,
            vec!["command_run".to_owned(), "agent_subagent".to_owned()]
        );
        assert_eq!(resolution.invalid_requested_tools, vec!["apply_patch"]);
        assert!(!resolution.wildcard);
    }

    #[test]
    fn agent_tool_resolution_parses_agent_allowed_types() {
        let config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        let mut harness = config.harnesses.get("native-code-edit").unwrap().clone();
        harness.tools.push("agent_subagent".to_owned());
        let mut agent = config.agents.get("executor").unwrap().clone();
        agent.tools = vec![
            "Agent(reviewer, planner)".to_owned(),
            "patch_apply".to_owned(),
        ];

        let resolution = resolve_agent_tools(&agent, &harness);

        assert_eq!(
            resolution.selected_tools,
            vec!["agent_subagent".to_owned(), "apply_patch".to_owned()]
        );
        assert_eq!(
            resolution.requested_tools,
            vec!["agent_subagent".to_owned(), "apply_patch".to_owned()]
        );
        assert_eq!(
            resolution.allowed_agent_types,
            Some(vec!["reviewer".to_owned(), "planner".to_owned()])
        );
        assert_eq!(
            normalized_tool_name("Task(reviewer)").as_deref(),
            Some("agent_subagent")
        );
    }

    #[test]
    fn agent_tool_resolution_treats_missing_tools_as_harness_wildcard() {
        let config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        let harness = config.harnesses.get("native-code-edit").unwrap();
        let mut agent = config.agents.get("executor").unwrap().clone();
        agent.disallowed_tools = vec!["command_preview".to_owned()];

        let resolution = resolve_agent_tools(&agent, harness);

        assert!(resolution.wildcard);
        assert!(resolution
            .selected_tools
            .iter()
            .any(|tool| tool == "command_run"));
        assert!(!resolution
            .selected_tools
            .iter()
            .any(|tool| tool == "command_preview"));
    }

    #[test]
    fn agent_tool_config_errors_when_node_harness_cannot_supply_requested_tool() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config
            .agents
            .get_mut("executor")
            .unwrap()
            .tools
            .push("unsupported_tool".to_owned());

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "workflow_node_agent_tool_not_in_harness"));
    }

    #[test]
    fn unknown_harness_backend_and_tools_are_reported() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        let harness = config.harnesses.get_mut("native-code-edit").unwrap();
        harness.backend = "mystery-backend".to_owned();
        harness.tools.push("definitely_not_a_tool".to_owned());

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "harness_backend_unknown"));
    }

    #[test]
    fn domain_specific_verifier_backend_is_not_part_of_core() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config
            .harnesses
            .get_mut("native-code-edit")
            .unwrap()
            .backend = "browser-verifier".to_owned();

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "harness_backend_unknown"));
    }

    #[test]
    fn planner_chat_surface_binding_must_reference_explicit_contracts() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config.surface_bindings.planner_chat.as_mut().unwrap().agent = "executor".to_owned();

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "planner_chat_agent_contract_invalid"));
    }

    #[test]
    fn backend_specific_unknown_tool_is_reported() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config
            .harnesses
            .get_mut("native-code-edit")
            .unwrap()
            .tools
            .push("unsupported_tool".to_owned());

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "harness_tool_unknown_for_backend"));
    }

    #[test]
    fn native_accepts_coder_api_tool_surface_tools() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        let harness = config.harnesses.get_mut("native-code-edit").unwrap();
        harness.tools = vec![
            "agent_subagent".to_owned(),
            "read_subagent_status".to_owned(),
            "cancel_subagent_background".to_owned(),
            "command_background".to_owned(),
            "read_command_output".to_owned(),
            "repo_read_file".to_owned(),
            "patch_preview".to_owned(),
        ];

        let report = validate_project_config(&config);

        assert_eq!(report.status, "pass");
    }

    #[test]
    fn native_harness_tool_denied_permission_is_reported() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        let harness = config.harnesses.get_mut("native-code-edit").unwrap();
        harness.tools.push("run_command_sandbox".to_owned());
        harness.permissions.run_commands = PermissionDecision::Deny;

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report.issues.iter().any(|issue| {
            issue.code == "harness_tool_permission_denied"
                && issue.target == "harnesses.native-code-edit.permissions.run_commands"
        }));
    }

    #[test]
    fn subagent_tool_denied_child_harness_permission_is_reported() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        let harness = config.harnesses.get_mut("native-code-edit").unwrap();
        harness.tools.push("agent_subagent".to_owned());
        harness.permissions.child_harness_permissions = PermissionDecision::Deny;

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report.issues.iter().any(|issue| {
            issue.code == "harness_tool_permission_denied"
                && issue.target
                    == "harnesses.native-code-edit.permissions.child_harness_permissions"
        }));
    }

    #[test]
    fn native_write_tool_denied_permission_is_reported() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config
            .harnesses
            .get_mut("native-code-edit")
            .unwrap()
            .permissions
            .write_files = PermissionDecision::Deny;

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report.issues.iter().any(|issue| {
            issue.code == "harness_tool_permission_denied"
                && issue.target == "harnesses.native-code-edit.permissions.write_files"
        }));
    }

    #[test]
    fn read_tool_denied_permission_is_reported() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config
            .harnesses
            .get_mut("native-code-edit")
            .unwrap()
            .permissions
            .read_files = PermissionDecision::Deny;

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report.issues.iter().any(|issue| {
            issue.code == "harness_tool_permission_denied"
                && issue.target == "harnesses.native-code-edit.permissions.read_files"
        }));
    }

    #[test]
    fn planner_model_harness_must_deny_side_effect_permissions() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config
            .harnesses
            .get_mut("workflow-planner")
            .unwrap()
            .permissions
            .run_commands = PermissionDecision::Ask;

        let report = validate_project_config(&config);

        assert_eq!(report.status, "error");
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "planner_model_side_effect_permission_not_denied"));
    }
}
