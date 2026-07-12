use coder_tools::{builtin_tool, ToolPermission};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
};

use crate::{
    normalized_tool_name, permission_decision, resolve_agent_tools, AgentRuntimePolicy, AgentSpec,
    HarnessSpec, HookCommandSpec, HookEvent, HookMatcherSpec, HookSettings, MemoryScope, ModelSpec,
    PermissionDecision, PermissionPolicy, ProjectConfig, ValidationIssue, ValidationLevel,
    ValidationReport, WorkflowSpec, AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MAX,
    AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MIN, AGENT_EFFORT_LEVELS,
    AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MAX, AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MIN,
    AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MAX, AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MIN,
    AGENT_MAX_OUTPUT_TOKENS_MAX, AGENT_MAX_OUTPUT_TOKENS_MIN, MODEL_CONTEXT_WINDOW_TOKENS_MAX,
    MODEL_CONTEXT_WINDOW_TOKENS_MIN, MODEL_EFFECTIVE_CONTEXT_WINDOW_PERCENT_MAX,
    MODEL_EFFECTIVE_CONTEXT_WINDOW_PERCENT_MIN, WORKFLOW_MAX_ROUNDS_MAX, WORKFLOW_MAX_ROUNDS_MIN,
};

const KNOWN_BACKENDS: &[&str] = &["planner-model", "native-rust", "native_mock", "mock"];

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
    issues.extend(validate_hook_settings(&config.hooks));

    for (model_id, model) in &config.models {
        issues.extend(validate_model_capabilities(model_id, model));
    }

    for (agent_id, agent) in &config.agents {
        issues.extend(validate_agent_runtime_policy(
            agent_id,
            &agent.runtime,
            config.models.get(&agent.model),
        ));
        issues.extend(validate_agent_tool_specs(agent_id, agent));
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
        if agent.role != "planner"
            && (contains_long_term_memory_scope(&agent.memory.read)
                || contains_long_term_memory_scope(&agent.memory.write))
        {
            issues.push(error(
                "agent_long_term_memory_for_non_planner",
                format!(
                    "agent '{agent_id}' has role '{}' and may only use workflow/run memory scopes",
                    agent.role
                ),
                format!("agents.{agent_id}.memory"),
            ));
        }
    }

    for (harness_id, harness) in &config.harnesses {
        issues.extend(validate_harness_config(harness_id, harness));
        if harness.backend != "planner-model"
            && (contains_long_term_memory_scope(&harness.memory.read)
                || contains_long_term_memory_scope(&harness.memory.write))
        {
            issues.push(error(
                "harness_long_term_memory_for_execution_backend",
                format!(
                    "harness '{harness_id}' uses backend '{}' and may only use workflow/run memory scopes",
                    harness.backend
                ),
                format!("harnesses.{harness_id}.memory"),
            ));
        }
    }

    if let Some(binding) = &config.surface_bindings.planner_chat {
        match config.agents.get(&binding.agent) {
            Some(agent)
                if agent.role == "planner"
                    && agent.output_contract == "planner_conversation" => {}
            Some(agent) => issues.push(error(
                "planner_chat_agent_contract_invalid",
                format!(
                    "Planner Chat agent '{}' must use role 'planner' and output_contract 'planner_conversation', found role '{}' and output_contract '{}'",
                    binding.agent, agent.role, agent.output_contract
                ),
                "surface_bindings.planner_chat.agent",
            )),
            None => issues.push(error(
                "planner_chat_agent_not_found",
                format!(
                    "surface_bindings.planner_chat references unknown agent '{}'",
                    binding.agent
                ),
                "surface_bindings.planner_chat.agent",
            )),
        }
        match config.harnesses.get(&binding.harness) {
            Some(harness) if harness.backend == "planner-model" => {}
            Some(harness) => issues.push(error(
                "planner_chat_harness_backend_invalid",
                format!(
                    "Planner Chat harness '{}' must use backend 'planner-model', found '{}'",
                    binding.harness, harness.backend
                ),
                "surface_bindings.planner_chat.harness",
            )),
            None => issues.push(error(
                "planner_chat_harness_not_found",
                format!(
                    "surface_bindings.planner_chat references unknown harness '{}'",
                    binding.harness
                ),
                "surface_bindings.planner_chat.harness",
            )),
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

fn validate_hook_settings(hooks: &HookSettings) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    for (event, event_path) in [
        (HookEvent::PreToolUse, "hooks.PreToolUse"),
        (HookEvent::PostToolUse, "hooks.PostToolUse"),
        (HookEvent::PostToolUseFailure, "hooks.PostToolUseFailure"),
    ] {
        for (matcher_index, matcher) in hooks.matchers_for_event(event).iter().enumerate() {
            issues.extend(validate_hook_matcher(
                matcher,
                &format!("{event_path}.{matcher_index}"),
            ));
        }
    }
    issues
}

fn validate_hook_matcher(matcher: &HookMatcherSpec, path: &str) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    if matcher.hooks.is_empty() {
        issues.push(error(
            "hook_matcher_hooks_empty",
            "hook matcher must define at least one hook",
            format!("{path}.hooks"),
        ));
    }
    for (hook_index, hook) in matcher.hooks.iter().enumerate() {
        issues.extend(validate_hook_command(
            hook,
            &format!("{path}.hooks.{hook_index}"),
        ));
    }
    issues
}

fn validate_hook_command(hook: &HookCommandSpec, path: &str) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    match hook {
        HookCommandSpec::Command {
            command, timeout, ..
        } => {
            if command.trim().is_empty() {
                issues.push(error(
                    "hook_command_empty",
                    "command hook must define a non-empty command",
                    format!("{path}.command"),
                ));
            }
            validate_hook_timeout(*timeout, path, &mut issues);
        }
        HookCommandSpec::Prompt {
            prompt, timeout, ..
        }
        | HookCommandSpec::Agent {
            prompt, timeout, ..
        } => {
            if prompt.trim().is_empty() {
                issues.push(error(
                    "hook_prompt_empty",
                    "prompt/agent hook must define a non-empty prompt",
                    format!("{path}.prompt"),
                ));
            }
            validate_hook_timeout(*timeout, path, &mut issues);
        }
        HookCommandSpec::Webhook { url, timeout, .. } => {
            if let Err(message) = validate_webhook_url_transport(url) {
                issues.push(error(
                    "hook_webhook_url_invalid",
                    message,
                    format!("{path}.url"),
                ));
            }
            validate_hook_timeout(*timeout, path, &mut issues);
        }
    }
    issues
}

fn validate_webhook_url_transport(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    let Some((scheme, _)) = trimmed.split_once("://") else {
        return Err("webhook hook url must start with http:// or https://".to_owned());
    };
    if scheme.eq_ignore_ascii_case("https") {
        return Ok(());
    }
    if scheme.eq_ignore_ascii_case("http") {
        if webhook_url_targets_loopback(trimmed) {
            return Ok(());
        }
        return Err(
            "webhook hook url must use https:// unless it targets loopback local development (localhost, 127.0.0.1/8, or ::1)"
                .to_owned(),
        );
    }
    Err("webhook hook url must start with http:// or https://".to_owned())
}

fn webhook_url_targets_loopback(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let Some(host) = webhook_authority_host(authority) else {
        return false;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host == "localhost" {
        return true;
    }
    host.parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

fn webhook_authority_host(authority: &str) -> Option<&str> {
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, host_port)| host_port)
        .unwrap_or(authority);
    if let Some(rest) = host_port.strip_prefix('[') {
        return rest.split_once(']').map(|(host, _)| host);
    }
    host_port.split(':').next().filter(|host| !host.is_empty())
}

fn validate_hook_timeout(timeout: Option<u64>, path: &str, issues: &mut Vec<ValidationIssue>) {
    if matches!(timeout, Some(0)) {
        issues.push(error(
            "hook_timeout_out_of_range",
            "hook timeout must be greater than 0 seconds",
            format!("{path}.timeout"),
        ));
    }
}

fn validate_agent_runtime_policy(
    agent_id: &str,
    runtime: &AgentRuntimePolicy,
    model: Option<&ModelSpec>,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    if runtime.max_turns == Some(0) {
        issues.push(error(
            "agent_max_turns_out_of_range",
            format!("agent '{agent_id}' runtime.max_turns must be a positive integer when set"),
            format!("agents.{agent_id}.runtime.max_turns"),
        ));
    }
    if let Some(max_output_tokens) = runtime.max_output_tokens {
        if !(AGENT_MAX_OUTPUT_TOKENS_MIN..=AGENT_MAX_OUTPUT_TOKENS_MAX).contains(&max_output_tokens)
        {
            issues.push(error(
                "agent_max_output_tokens_out_of_range",
                format!(
                    "agent '{agent_id}' runtime.max_output_tokens must be between {AGENT_MAX_OUTPUT_TOKENS_MIN} and {AGENT_MAX_OUTPUT_TOKENS_MAX}"
                ),
                format!("agents.{agent_id}.runtime.max_output_tokens"),
            ));
        }
        if let Some(model) = model {
            let model_limit = model.resolved_capabilities().max_output_tokens;
            if max_output_tokens > model_limit {
                issues.push(error(
                    "agent_max_output_tokens_exceeds_model_capability",
                    format!(
                        "agent '{agent_id}' runtime.max_output_tokens must not exceed model capability {model_limit}"
                    ),
                    format!("agents.{agent_id}.runtime.max_output_tokens"),
                ));
            }
        }
    }
    if let Some(effort) = runtime.effort.as_deref() {
        let normalized = effort.trim().to_ascii_lowercase();
        if normalized.is_empty() || !AGENT_EFFORT_LEVELS.iter().any(|level| *level == normalized) {
            issues.push(error(
                "agent_effort_level_unknown",
                format!(
                    "agent '{agent_id}' runtime.effort must be one of {}",
                    AGENT_EFFORT_LEVELS.join(", ")
                ),
                format!("agents.{agent_id}.runtime.effort"),
            ));
        }
    }
    if let Some(reserve) = runtime.compact_output_reserve_tokens {
        if !(AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MIN..=AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MAX)
            .contains(&reserve)
        {
            issues.push(error(
                "agent_compact_output_reserve_tokens_out_of_range",
                format!(
                    "agent '{agent_id}' runtime.compact_output_reserve_tokens must be between {AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MIN} and {AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MAX}"
                ),
                format!("agents.{agent_id}.runtime.compact_output_reserve_tokens"),
            ));
        }
        if let Some(model) = model {
            let model_limit = model.resolved_capabilities().max_output_tokens;
            if reserve > model_limit {
                issues.push(error(
                    "agent_compact_output_reserve_exceeds_model_capability",
                    format!(
                        "agent '{agent_id}' runtime.compact_output_reserve_tokens must not exceed model output capability {model_limit}"
                    ),
                    format!("agents.{agent_id}.runtime.compact_output_reserve_tokens"),
                ));
            }
        }
    }
    if !(AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MIN..=AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MAX)
        .contains(&runtime.max_output_recovery_attempts)
    {
        issues.push(error(
            "agent_max_output_recovery_attempts_out_of_range",
            format!(
                "agent '{agent_id}' runtime.max_output_recovery_attempts must be between {AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MIN} and {AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MAX}"
            ),
            format!("agents.{agent_id}.runtime.max_output_recovery_attempts"),
        ));
    }
    if !(AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MIN
        ..=AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MAX)
        .contains(&runtime.max_consecutive_compaction_failures)
    {
        issues.push(error(
            "agent_max_consecutive_compaction_failures_out_of_range",
            format!(
                "agent '{agent_id}' runtime.max_consecutive_compaction_failures must be between {AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MIN} and {AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MAX}"
            ),
            format!("agents.{agent_id}.runtime.max_consecutive_compaction_failures"),
        ));
    }
    issues
}

fn validate_model_capabilities(model_id: &str, model: &ModelSpec) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    if let Some(context_window) = model.capabilities.context_window_tokens {
        if !(MODEL_CONTEXT_WINDOW_TOKENS_MIN..=MODEL_CONTEXT_WINDOW_TOKENS_MAX)
            .contains(&context_window)
        {
            issues.push(error(
                "model_context_window_tokens_out_of_range",
                format!(
                    "model '{model_id}' capabilities.context_window_tokens must be between {MODEL_CONTEXT_WINDOW_TOKENS_MIN} and {MODEL_CONTEXT_WINDOW_TOKENS_MAX}"
                ),
                format!("models.{model_id}.capabilities.context_window_tokens"),
            ));
        }
    }
    if let Some(max_output_tokens) = model.capabilities.max_output_tokens {
        if !(AGENT_MAX_OUTPUT_TOKENS_MIN..=AGENT_MAX_OUTPUT_TOKENS_MAX).contains(&max_output_tokens)
        {
            issues.push(error(
                "model_max_output_tokens_out_of_range",
                format!(
                    "model '{model_id}' capabilities.max_output_tokens must be between {AGENT_MAX_OUTPUT_TOKENS_MIN} and {AGENT_MAX_OUTPUT_TOKENS_MAX}"
                ),
                format!("models.{model_id}.capabilities.max_output_tokens"),
            ));
        }
    }
    if model.capabilities.auto_compact_token_limit == Some(0) {
        issues.push(error(
            "model_auto_compact_token_limit_out_of_range",
            format!(
                "model '{model_id}' capabilities.auto_compact_token_limit must be positive when set"
            ),
            format!("models.{model_id}.capabilities.auto_compact_token_limit"),
        ));
    }
    if let Some(percent) = model.capabilities.effective_context_window_percent {
        if !(MODEL_EFFECTIVE_CONTEXT_WINDOW_PERCENT_MIN
            ..=MODEL_EFFECTIVE_CONTEXT_WINDOW_PERCENT_MAX)
            .contains(&percent)
        {
            issues.push(error(
                "model_effective_context_window_percent_out_of_range",
                format!(
                    "model '{model_id}' capabilities.effective_context_window_percent must be between {MODEL_EFFECTIVE_CONTEXT_WINDOW_PERCENT_MIN} and {MODEL_EFFECTIVE_CONTEXT_WINDOW_PERCENT_MAX}"
                ),
                format!("models.{model_id}.capabilities.effective_context_window_percent"),
            ));
        }
    }
    issues
}

fn validate_agent_tool_specs(agent_id: &str, agent: &AgentSpec) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    issues.extend(validate_agent_tool_spec_list(
        agent_id,
        "tools",
        &agent.tools,
        true,
    ));
    issues.extend(validate_agent_tool_spec_list(
        agent_id,
        "disallowed_tools",
        &agent.disallowed_tools,
        false,
    ));
    issues
}

fn validate_agent_tool_spec_list(
    agent_id: &str,
    field: &str,
    specs: &[String],
    wildcard_allowed: bool,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    let mut seen_tools = BTreeSet::new();
    let has_wildcard = specs
        .iter()
        .filter_map(|spec| normalized_tool_name(spec))
        .any(|tool| tool == "*");
    if wildcard_allowed && has_wildcard && specs.len() > 1 {
        issues.push(error(
            "agent_tool_wildcard_mixed",
            format!("agent '{agent_id}' {field} may use '*' only by itself"),
            format!("agents.{agent_id}.{field}"),
        ));
    }
    if !wildcard_allowed && has_wildcard {
        issues.push(error(
            "agent_disallowed_tool_wildcard",
            format!("agent '{agent_id}' {field} may not use '*'"),
            format!("agents.{agent_id}.{field}"),
        ));
    }
    for spec in specs {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            issues.push(error(
                "agent_tool_empty",
                format!("agent '{agent_id}' {field} contains an empty tool name"),
                format!("agents.{agent_id}.{field}"),
            ));
            continue;
        }
        let Some(tool_name) = normalized_tool_name(spec) else {
            continue;
        };
        if !seen_tools.insert(tool_name.clone()) {
            issues.push(warning(
                "agent_tool_duplicate",
                format!("agent '{agent_id}' {field} lists tool '{tool_name}' more than once"),
                format!("agents.{agent_id}.{field}"),
            ));
        }
    }
    issues
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
    if workflow.max_rounds < WORKFLOW_MAX_ROUNDS_MIN
        || workflow.max_rounds > WORKFLOW_MAX_ROUNDS_MAX
    {
        issues.push(error(
            "workflow_max_rounds_out_of_range",
            format!(
                "workflow '{workflow_id}' max_rounds must be between {WORKFLOW_MAX_ROUNDS_MIN} and {WORKFLOW_MAX_ROUNDS_MAX}"
            ),
            format!("workflows.{workflow_id}.max_rounds"),
        ));
    }
    if workflow.token_budget == Some(0) {
        issues.push(error(
            "workflow_token_budget_zero",
            format!("workflow '{workflow_id}' token_budget must be positive when configured"),
            format!("workflows.{workflow_id}.token_budget"),
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

    let node_ids: BTreeSet<&str> = workflow.nodes.iter().map(|node| node.id.as_str()).collect();
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
        if let (Some(agent), Some(harness)) =
            (agents.get(&node.agent), harnesses.get(&node.harness))
        {
            issues.extend(validate_workflow_node_agent_tools(
                workflow_id,
                &node.id,
                &node.agent,
                agent,
                &node.harness,
                harness,
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

fn validate_workflow_node_agent_tools(
    workflow_id: &str,
    node_id: &str,
    agent_id: &str,
    agent: &AgentSpec,
    harness_id: &str,
    harness: &HarnessSpec,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    let resolution = resolve_agent_tools(agent, harness);
    for tool in resolution.invalid_requested_tools {
        issues.push(error(
            "workflow_node_agent_tool_not_in_harness",
            format!(
                "workflow '{workflow_id}' node '{node_id}' agent '{agent_id}' requests tool '{tool}' but harness '{harness_id}' does not provide it or disallows it for that agent"
            ),
            format!("workflows.{workflow_id}.nodes.{node_id}"),
        ));
    }
    for tool in resolution.ignored_disallowed_tools {
        issues.push(warning(
            "workflow_node_agent_disallowed_tool_not_in_harness",
            format!(
                "workflow '{workflow_id}' node '{node_id}' agent '{agent_id}' disallows tool '{tool}' but harness '{harness_id}' does not provide it"
            ),
            format!("workflows.{workflow_id}.nodes.{node_id}"),
        ));
    }
    issues
}

fn validate_harness_config(harness_id: &str, harness: &HarnessSpec) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    if !KNOWN_BACKENDS.contains(&harness.backend.as_str()) {
        issues.push(error(
            "harness_backend_unknown",
            format!(
                "harness '{harness_id}' uses unsupported backend '{}'",
                harness.backend
            ),
            format!("harnesses.{harness_id}.backend"),
        ));
    }

    let mut seen_tools = BTreeSet::new();
    for tool in &harness.tools {
        let tool = tool.trim();
        if tool.is_empty() {
            issues.push(error(
                "harness_tool_empty",
                format!("harness '{harness_id}' contains an empty tool name"),
                format!("harnesses.{harness_id}.tools"),
            ));
            continue;
        }
        let Some(tool_name) = normalized_tool_name(tool) else {
            continue;
        };
        if !seen_tools.insert(tool_name.clone()) {
            issues.push(warning(
                "harness_tool_duplicate",
                format!("harness '{harness_id}' lists tool '{tool_name}' more than once"),
                format!("harnesses.{harness_id}.tools"),
            ));
        }
        if !known_tool_for_backend(&harness.backend, &tool_name) {
            issues.push(error(
                "harness_tool_unknown_for_backend",
                format!(
                    "harness '{harness_id}' tool '{tool}' is not supported by backend '{}'",
                    harness.backend
                ),
                format!("harnesses.{harness_id}.tools"),
            ));
            continue;
        }
        issues.extend(validate_tool_permission(harness_id, &tool_name, harness));
    }

    if harness.backend == "planner-model" {
        issues.extend(validate_planner_model_permissions(
            harness_id,
            &harness.permissions,
        ));
    }
    issues
}

fn known_tool_for_backend(backend: &str, tool: &str) -> bool {
    match backend {
        "planner-model" => builtin_tool(tool)
            .is_some_and(|definition| definition.permission == ToolPermission::ReadFiles),
        "native-rust" | "native_mock" | "mock" => builtin_tool(tool).is_some(),
        _ => true,
    }
}

fn validate_tool_permission(
    harness_id: &str,
    tool: &str,
    harness: &HarnessSpec,
) -> Vec<ValidationIssue> {
    required_permissions_for_tool(&harness.backend, tool)
        .iter()
        .filter_map(|permission| {
            let decision = permission_decision(&harness.permissions, permission)?;
            if decision == PermissionDecision::Deny {
                Some(error(
                    "harness_tool_permission_denied",
                    format!(
                        "harness '{harness_id}' enables tool '{tool}' but permission '{permission}' is deny"
                    ),
                    format!("harnesses.{harness_id}.permissions.{permission}"),
                ))
            } else {
                None
            }
        })
        .collect()
}

fn required_permissions_for_tool(backend: &str, tool: &str) -> &'static [&'static str] {
    match backend {
        "planner-model" => builtin_tool(tool)
            .filter(|definition| definition.permission == ToolPermission::ReadFiles)
            .map(|_| &["read_files"][..])
            .unwrap_or(&[]),
        "native-rust" | "native_mock" | "mock" => match builtin_tool(tool)
            .map(|definition| definition.permission)
            .unwrap_or(ToolPermission::None)
        {
            ToolPermission::None => &[],
            ToolPermission::ReadFiles => &["read_files"],
            ToolPermission::WriteFiles => &["write_files"],
            ToolPermission::RunCommands => &["run_commands"],
            ToolPermission::ChildHarnessPermissions => &["child_harness_permissions"],
        },
        _ => &[],
    }
}

fn validate_planner_model_permissions(
    harness_id: &str,
    permissions: &PermissionPolicy,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    for (field, decision) in [
        ("write_files", permissions.write_files),
        ("run_commands", permissions.run_commands),
        (
            "child_harness_permissions",
            permissions.child_harness_permissions,
        ),
        ("network", permissions.network),
        ("secrets", permissions.secrets),
        ("publish_external", permissions.publish_external),
        ("git_commit", permissions.git_commit),
        ("git_push", permissions.git_push),
        ("deploy", permissions.deploy),
    ] {
        if decision != PermissionDecision::Deny {
            issues.push(error(
                "planner_model_side_effect_permission_not_denied",
                format!(
                    "planner-model harness '{harness_id}' must deny side-effect permission '{field}'"
                ),
                format!("harnesses.{harness_id}.permissions.{field}"),
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

fn contains_long_term_memory_scope(scopes: &[MemoryScope]) -> bool {
    scopes.iter().any(is_long_term_memory_scope)
}

fn is_long_term_memory_scope(scope: &MemoryScope) -> bool {
    matches!(
        scope,
        MemoryScope::User
            | MemoryScope::Project
            | MemoryScope::Agent
            | MemoryScope::RepoFacts
            | MemoryScope::KnowledgeHints
            | MemoryScope::ExternalDocs
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
