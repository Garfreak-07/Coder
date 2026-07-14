use coder_tools::{builtin_tool, ToolPermission};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
};

use crate::{
    normalized_tool_name, permission_decision, resolve_task_tools, AgentRuntimePolicy, HarnessSpec,
    HookCommandSpec, HookEvent, HookMatcherSpec, HookSettings, ModelSpec, PermissionDecision,
    ProjectConfig, TaskProfile, ValidationIssue, ValidationLevel, ValidationReport,
    AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MAX, AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MIN,
    AGENT_EFFORT_LEVELS, AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MAX,
    AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MIN, AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MAX,
    AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MIN, AGENT_MAX_OUTPUT_TOKENS_MAX,
    AGENT_MAX_OUTPUT_TOKENS_MIN, MODEL_CONTEXT_WINDOW_TOKENS_MAX, MODEL_CONTEXT_WINDOW_TOKENS_MIN,
    MODEL_EFFECTIVE_CONTEXT_WINDOW_PERCENT_MAX, MODEL_EFFECTIVE_CONTEXT_WINDOW_PERCENT_MIN,
};

const KNOWN_BACKENDS: &[&str] = &["native-rust", "native_mock", "mock"];

pub fn validate_project_config(config: &ProjectConfig) -> ValidationReport {
    let mut issues = Vec::new();

    if config.version != 1 {
        issues.push(error(
            "unsupported_version",
            "config version must be 1",
            "version",
        ));
    }
    if config.task_profiles.is_empty() {
        issues.push(error(
            "missing_task_profiles",
            "config must define at least one task profile",
            "task_profiles",
        ));
    }
    issues.extend(validate_hook_settings(&config.hooks));

    for (model_id, model) in &config.models {
        issues.extend(validate_model_capabilities(model_id, model));
    }

    for (profile_id, profile) in &config.task_profiles {
        issues.extend(validate_agent_runtime_policy(
            profile_id,
            &profile.runtime,
            config.models.get(&profile.model),
        ));
        issues.extend(validate_task_tool_specs(profile_id, profile));
        if profile.instructions.trim().is_empty() {
            issues.push(warning(
                "task_profile_instructions_empty",
                format!("task profile '{profile_id}' has empty instructions"),
                format!("task_profiles.{profile_id}.instructions"),
            ));
        }
    }

    for (harness_id, harness) in &config.harnesses {
        issues.extend(validate_harness_config(harness_id, harness));
    }

    for (profile_id, profile) in &config.task_profiles {
        issues.extend(validate_task_profile(
            profile_id,
            profile,
            &config.models,
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
    profile_id: &str,
    runtime: &AgentRuntimePolicy,
    model: Option<&ModelSpec>,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    if runtime.max_turns == Some(0) {
        issues.push(error(
            "task_profile_max_turns_out_of_range",
            format!(
                "task profile '{profile_id}' runtime.max_turns must be a positive integer when set"
            ),
            format!("task_profiles.{profile_id}.runtime.max_turns"),
        ));
    }
    if let Some(max_output_tokens) = runtime.max_output_tokens {
        if !(AGENT_MAX_OUTPUT_TOKENS_MIN..=AGENT_MAX_OUTPUT_TOKENS_MAX).contains(&max_output_tokens)
        {
            issues.push(error(
                "task_profile_max_output_tokens_out_of_range",
                format!(
                    "task profile '{profile_id}' runtime.max_output_tokens must be between {AGENT_MAX_OUTPUT_TOKENS_MIN} and {AGENT_MAX_OUTPUT_TOKENS_MAX}"
                ),
                format!("task_profiles.{profile_id}.runtime.max_output_tokens"),
            ));
        }
        if let Some(model) = model {
            let model_limit = model.resolved_capabilities().max_output_tokens;
            if max_output_tokens > model_limit {
                issues.push(error(
                    "task_profile_max_output_tokens_exceeds_model_capability",
                    format!(
                        "task profile '{profile_id}' runtime.max_output_tokens must not exceed model capability {model_limit}"
                    ),
                    format!("task_profiles.{profile_id}.runtime.max_output_tokens"),
                ));
            }
        }
    }
    if let Some(effort) = runtime.effort.as_deref() {
        let normalized = effort.trim().to_ascii_lowercase();
        if normalized.is_empty() || !AGENT_EFFORT_LEVELS.iter().any(|level| *level == normalized) {
            issues.push(error(
                "task_profile_effort_level_unknown",
                format!(
                    "task profile '{profile_id}' runtime.effort must be one of {}",
                    AGENT_EFFORT_LEVELS.join(", ")
                ),
                format!("task_profiles.{profile_id}.runtime.effort"),
            ));
        }
    }
    if let Some(reserve) = runtime.compact_output_reserve_tokens {
        if !(AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MIN..=AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MAX)
            .contains(&reserve)
        {
            issues.push(error(
                "task_profile_compact_output_reserve_tokens_out_of_range",
                format!(
                    "task profile '{profile_id}' runtime.compact_output_reserve_tokens must be between {AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MIN} and {AGENT_COMPACT_OUTPUT_RESERVE_TOKENS_MAX}"
                ),
                format!("task_profiles.{profile_id}.runtime.compact_output_reserve_tokens"),
            ));
        }
        if let Some(model) = model {
            let model_limit = model.resolved_capabilities().max_output_tokens;
            if reserve > model_limit {
                issues.push(error(
                    "task_profile_compact_output_reserve_exceeds_model_capability",
                    format!(
                        "task profile '{profile_id}' runtime.compact_output_reserve_tokens must not exceed model output capability {model_limit}"
                    ),
                    format!("task_profiles.{profile_id}.runtime.compact_output_reserve_tokens"),
                ));
            }
        }
    }
    if !(AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MIN..=AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MAX)
        .contains(&runtime.max_output_recovery_attempts)
    {
        issues.push(error(
            "task_profile_max_output_recovery_attempts_out_of_range",
            format!(
                "task profile '{profile_id}' runtime.max_output_recovery_attempts must be between {AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MIN} and {AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_MAX}"
            ),
            format!("task_profiles.{profile_id}.runtime.max_output_recovery_attempts"),
        ));
    }
    if !(AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MIN
        ..=AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MAX)
        .contains(&runtime.max_consecutive_compaction_failures)
    {
        issues.push(error(
            "task_profile_max_consecutive_compaction_failures_out_of_range",
            format!(
                "task profile '{profile_id}' runtime.max_consecutive_compaction_failures must be between {AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MIN} and {AGENT_MAX_CONSECUTIVE_COMPACTION_FAILURES_MAX}"
            ),
            format!("task_profiles.{profile_id}.runtime.max_consecutive_compaction_failures"),
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

fn validate_task_tool_specs(profile_id: &str, profile: &TaskProfile) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    issues.extend(validate_agent_tool_spec_list(
        profile_id,
        "tools",
        &profile.tools,
        true,
    ));
    issues.extend(validate_agent_tool_spec_list(
        profile_id,
        "disallowed_tools",
        &profile.disallowed_tools,
        false,
    ));
    issues
}

fn validate_agent_tool_spec_list(
    profile_id: &str,
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
            "task_profile_tool_wildcard_mixed",
            format!("task profile '{profile_id}' {field} may use '*' only by itself"),
            format!("task_profiles.{profile_id}.{field}"),
        ));
    }
    if !wildcard_allowed && has_wildcard {
        issues.push(error(
            "task_profile_disallowed_tool_wildcard",
            format!("task profile '{profile_id}' {field} may not use '*'"),
            format!("task_profiles.{profile_id}.{field}"),
        ));
    }
    for spec in specs {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            issues.push(error(
                "task_profile_tool_empty",
                format!("task profile '{profile_id}' {field} contains an empty tool name"),
                format!("task_profiles.{profile_id}.{field}"),
            ));
            continue;
        }
        let Some(tool_name) = normalized_tool_name(spec) else {
            continue;
        };
        if !seen_tools.insert(tool_name.clone()) {
            issues.push(warning(
                "task_profile_tool_duplicate",
                format!(
                    "task profile '{profile_id}' {field} lists tool '{tool_name}' more than once"
                ),
                format!("task_profiles.{profile_id}.{field}"),
            ));
        }
    }
    issues
}

pub fn validate_task_profile(
    profile_id: &str,
    profile: &TaskProfile,
    models: &BTreeMap<String, ModelSpec>,
    harnesses: &BTreeMap<String, HarnessSpec>,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    if profile.token_budget == Some(0) {
        issues.push(error(
            "task_profile_token_budget_zero",
            format!("task profile '{profile_id}' token_budget must be positive when configured"),
            format!("task_profiles.{profile_id}.token_budget"),
        ));
    }
    if !models.contains_key(&profile.model) {
        issues.push(error(
            "task_profile_model_not_found",
            format!(
                "task profile '{profile_id}' references unknown model '{}'",
                profile.model
            ),
            format!("task_profiles.{profile_id}.model"),
        ));
    }
    if !harnesses.contains_key(&profile.harness) {
        issues.push(error(
            "task_profile_harness_not_found",
            format!(
                "task profile '{profile_id}' references unknown harness '{}'",
                profile.harness
            ),
            format!("task_profiles.{profile_id}.harness"),
        ));
    }
    if let Some(harness) = harnesses.get(&profile.harness) {
        issues.extend(validate_task_profile_tools(profile_id, profile, harness));
    }
    issues
}

fn validate_task_profile_tools(
    profile_id: &str,
    profile: &TaskProfile,
    harness: &HarnessSpec,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    let resolution = resolve_task_tools(profile, harness);
    for tool in resolution.invalid_requested_tools {
        issues.push(error(
            "task_profile_tool_not_in_harness",
            format!(
                "task profile '{profile_id}' requests tool '{tool}' but harness '{}' does not provide it or the profile disallows it",
                profile.harness
            ),
            format!("task_profiles.{profile_id}.tools"),
        ));
    }
    for tool in resolution.ignored_disallowed_tools {
        issues.push(warning(
            "task_profile_disallowed_tool_not_in_harness",
            format!(
                "task profile '{profile_id}' disallows tool '{tool}' but harness '{}' does not provide it",
                profile.harness
            ),
            format!("task_profiles.{profile_id}.disallowed_tools"),
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

    issues
}

fn known_tool_for_backend(backend: &str, tool: &str) -> bool {
    match backend {
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
