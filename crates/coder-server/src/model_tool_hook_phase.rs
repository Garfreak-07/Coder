use coder_config::{HookCommandSpec, HookEvent, HookSettings, ModelSpec, PermissionDecision};
use coder_store::RunStore;
use coder_workflow::ModelToolHostContext;
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::model_tool_agent_hooks::execute_agent_model_tool_hook;
use crate::model_tool_command_hooks::execute_command_model_tool_hook;
use crate::model_tool_hook_output::merge_hook_permission_behavior;
use crate::model_tool_permissions::{
    default_model_tool_config, model_tool_context_run_id, model_tool_permission_context,
    read_run_project_config_snapshot, ModelToolPermissionContext,
};
use crate::model_tool_prompt_hooks::execute_prompt_model_tool_hook;
use crate::model_tool_webhook_hooks::execute_webhook_model_tool_hook;
use crate::ApiState;

pub(crate) struct ModelToolHookContext {
    pub(crate) source: &'static str,
    pub(crate) disable_all_hooks: bool,
    pub(crate) hooks: HookSettings,
    pub(crate) models: BTreeMap<String, ModelSpec>,
    pub(crate) allowed_webhook_urls: Option<Vec<String>>,
    pub(crate) webhook_allowed_env_vars: Option<Vec<String>>,
}

pub(crate) struct ModelToolHookPhase {
    pub(crate) status: &'static str,
    pub(crate) payload: Value,
    pub(crate) blocking_error: Option<String>,
    pub(crate) updated_input: Option<Value>,
    pub(crate) updated_tool_output: Option<Value>,
}

pub(crate) struct ModelToolHookInvocation<'a> {
    pub(crate) event: HookEvent,
    pub(crate) canonical_tool_name: &'a str,
    pub(crate) requested_tool_name: &'a str,
    pub(crate) tool_use_id: &'a str,
    pub(crate) tool_input: &'a Value,
    pub(crate) tool_response: Option<&'a Value>,
    pub(crate) tool_error: Option<&'a str>,
    pub(crate) host_context: &'a ModelToolHostContext,
}

pub(crate) async fn execute_model_tool_hook_phase(
    state: &ApiState,
    invocation: ModelToolHookInvocation<'_>,
) -> ModelToolHookPhase {
    let store = &state.store;
    let context = model_tool_hook_context(store, invocation.tool_input, invocation.host_context);
    let event_name = hook_event_name(invocation.event);
    let matchers = context.hooks.matchers_for_event(invocation.event);
    let matcher_count = matchers.len();
    let hook_count = matchers
        .iter()
        .map(|matcher| matcher.hooks.len())
        .sum::<usize>();
    let matched_matchers = matchers
        .iter()
        .filter(|matcher| {
            hook_matcher_matches_tool(
                matcher.matcher.as_deref(),
                invocation.canonical_tool_name,
                invocation.requested_tool_name,
            )
        })
        .collect::<Vec<_>>();
    let matched_hook_count = matched_matchers
        .iter()
        .map(|matcher| matcher.hooks.len())
        .sum::<usize>();
    let matched_hook_types = matched_matchers
        .iter()
        .flat_map(|matcher| matcher.hooks.iter().map(hook_command_kind))
        .collect::<Vec<_>>();
    let command_hook_count = matched_matchers
        .iter()
        .flat_map(|matcher| matcher.hooks.iter())
        .filter(|hook| matches!(hook, HookCommandSpec::Command { .. }))
        .count();
    let webhook_hook_count = matched_matchers
        .iter()
        .flat_map(|matcher| matcher.hooks.iter())
        .filter(|hook| matches!(hook, HookCommandSpec::Webhook { .. }))
        .count();
    let prompt_hook_count = matched_matchers
        .iter()
        .flat_map(|matcher| matcher.hooks.iter())
        .filter(|hook| matches!(hook, HookCommandSpec::Prompt { .. }))
        .count();
    let agent_hook_count = matched_matchers
        .iter()
        .flat_map(|matcher| matcher.hooks.iter())
        .filter(|hook| matches!(hook, HookCommandSpec::Agent { .. }))
        .count();
    let supported_hook_count = command_hook_count
        .saturating_add(webhook_hook_count)
        .saturating_add(prompt_hook_count)
        .saturating_add(agent_hook_count);
    let unsupported_hook_count = matched_hook_count.saturating_sub(supported_hook_count);
    let permission_context =
        model_tool_permission_context(store, invocation.tool_input, invocation.host_context);
    let run_commands_allowed =
        permission_context.permissions.run_commands == PermissionDecision::Allow;
    let network_allowed = permission_context.permissions.network == PermissionDecision::Allow;
    let hook_input = model_tool_hook_input(&invocation, &permission_context);
    let mut hook_results = Vec::new();
    let mut blocking_error = None;
    let mut updated_input = None;
    let mut updated_tool_output = None;
    let mut hook_permission_behavior = None;
    let mut additional_contexts = Vec::new();
    let mut executed_hook_count = 0usize;
    let mut skipped_permission_count = 0usize;

    if !context.disable_all_hooks && matched_hook_count > 0 {
        for matcher in &matched_matchers {
            for hook in &matcher.hooks {
                match hook {
                    HookCommandSpec::Command { .. } if run_commands_allowed => {
                        let result = execute_command_model_tool_hook(
                            state.store.clone(),
                            hook,
                            invocation.event,
                            invocation.requested_tool_name,
                            &hook_input,
                            invocation.tool_input,
                        );
                        executed_hook_count += 1;
                        if updated_input.is_none() {
                            updated_input = result.effects.updated_input.clone();
                        }
                        if let Some(output) = result.effects.updated_tool_output.clone() {
                            updated_tool_output = Some(output);
                        }
                        hook_permission_behavior = merge_hook_permission_behavior(
                            hook_permission_behavior,
                            result.effects.permission_behavior,
                        );
                        if let Some(context) = result.effects.additional_context.clone() {
                            additional_contexts.push(context);
                        }
                        if blocking_error.is_none() {
                            blocking_error = result.blocking_error.clone();
                        }
                        hook_results.push(result.payload);
                        if blocking_error.is_some() {
                            break;
                        }
                    }
                    HookCommandSpec::Webhook { .. } if network_allowed => {
                        let result = execute_webhook_model_tool_hook(
                            hook,
                            invocation.event,
                            invocation.requested_tool_name,
                            &hook_input,
                            &context,
                        )
                        .await;
                        executed_hook_count += 1;
                        if updated_input.is_none() {
                            updated_input = result.effects.updated_input.clone();
                        }
                        if let Some(output) = result.effects.updated_tool_output.clone() {
                            updated_tool_output = Some(output);
                        }
                        hook_permission_behavior = merge_hook_permission_behavior(
                            hook_permission_behavior,
                            result.effects.permission_behavior,
                        );
                        if let Some(context) = result.effects.additional_context.clone() {
                            additional_contexts.push(context);
                        }
                        if blocking_error.is_none() {
                            blocking_error = result.blocking_error.clone();
                        }
                        hook_results.push(result.payload);
                        if blocking_error.is_some() {
                            break;
                        }
                    }
                    HookCommandSpec::Prompt { .. } => {
                        let result = execute_prompt_model_tool_hook(
                            state,
                            hook,
                            invocation.event,
                            invocation.requested_tool_name,
                            &hook_input,
                            invocation.host_context,
                            &context,
                        )
                        .await;
                        executed_hook_count += 1;
                        if blocking_error.is_none() {
                            blocking_error = result.blocking_error.clone();
                        }
                        hook_results.push(result.payload);
                        if blocking_error.is_some() {
                            break;
                        }
                    }
                    HookCommandSpec::Agent { .. } => {
                        let result = execute_agent_model_tool_hook(
                            state,
                            hook,
                            invocation.event,
                            invocation.requested_tool_name,
                            &hook_input,
                            invocation.host_context,
                            &context,
                        )
                        .await;
                        executed_hook_count += 1;
                        if blocking_error.is_none() {
                            blocking_error = result.blocking_error.clone();
                        }
                        hook_results.push(result.payload);
                        if blocking_error.is_some() {
                            break;
                        }
                    }
                    HookCommandSpec::Command { command, .. } => {
                        skipped_permission_count += 1;
                        hook_results.push(json!({
                            "type": "command",
                            "command": command,
                            "outcome": "skipped_permission_required",
                            "required_permission": "run_commands",
                            "permission_behavior": permission_context.permissions.run_commands,
                            "permission_policy_source": permission_context.source
                        }));
                    }
                    HookCommandSpec::Webhook { url, .. } => {
                        skipped_permission_count += 1;
                        hook_results.push(json!({
                            "type": "webhook",
                            "url": url,
                            "outcome": "skipped_permission_required",
                            "required_permission": "network",
                            "permission_behavior": permission_context.permissions.network,
                            "permission_policy_source": permission_context.source
                        }));
                    }
                }
            }
            if blocking_error.is_some() {
                break;
            }
        }
    }

    let status = if blocking_error.is_some() {
        "blocked"
    } else if context.disable_all_hooks {
        "disabled"
    } else if hook_count == 0 {
        "not_configured"
    } else if matched_hook_count == 0 {
        "skipped"
    } else if executed_hook_count > 0 {
        "completed"
    } else if skipped_permission_count > 0 {
        "permission_blocked"
    } else {
        "configured_not_executed"
    };
    let should_apply_updated_input =
        invocation.event == HookEvent::PreToolUse && blocking_error.is_none();
    let should_apply_updated_tool_output = invocation.event == HookEvent::PostToolUse
        && blocking_error.is_none()
        && updated_tool_output.is_some();

    ModelToolHookPhase {
        status,
        payload: json!({
            "contract": "coder.model_tool_hooks.v1",
            "hook_event": event_name,
            "tool_name": invocation.requested_tool_name,
            "canonical_tool_name": invocation.canonical_tool_name,
            "hook_config_source": context.source,
            "disable_all_hooks": context.disable_all_hooks,
            "matcher_count": matcher_count,
            "hook_count": hook_count,
            "matched_matcher_count": matched_matchers.len(),
            "matched_hook_count": matched_hook_count,
            "matched_hook_types": matched_hook_types,
            "command_hook_count": command_hook_count,
            "webhook_hook_count": webhook_hook_count,
            "prompt_hook_count": prompt_hook_count,
            "agent_hook_count": agent_hook_count,
            "unsupported_hook_count": unsupported_hook_count,
            "executed_hook_count": executed_hook_count,
            "skipped_permission_count": skipped_permission_count,
            "updated_input_applied": updated_input.is_some() && invocation.event == HookEvent::PreToolUse && blocking_error.is_none(),
            "updated_tool_output_applied": should_apply_updated_tool_output,
            "hook_permission_behavior": hook_permission_behavior,
            "additional_contexts": additional_contexts,
            "hook_results": hook_results,
            "blocking_error": blocking_error.clone(),
            "permission_policy_source": {
                "type": permission_context.source,
                "harness_id": permission_context.harness_id,
                "requested_harness_id": permission_context.requested_harness_id
            },
            "execution_status": if blocking_error.is_some() {
                "blocked"
            } else if executed_hook_count > 0 {
                "completed"
            } else if skipped_permission_count > 0 {
                "skipped_permission_required"
            } else if matched_hook_count > 0 && unsupported_hook_count > 0 && !context.disable_all_hooks {
                "unsupported_hook_types"
            } else if matched_hook_count > 0 && !context.disable_all_hooks {
                "runtime_not_implemented"
            } else {
                "not_applicable"
            },
            "claude_sources": [
                "src/schemas/hooks.ts HookMatcherSchema",
                "src/utils/hooks.ts executePreToolHooks/executePostToolHooks",
                "src/services/tools/toolHooks.ts runPreToolUseHooks/runPostToolUseHooks"
            ]
        }),
        blocking_error,
        updated_input: if should_apply_updated_input {
            updated_input
        } else {
            None
        },
        updated_tool_output: if should_apply_updated_tool_output {
            updated_tool_output
        } else {
            None
        },
    }
}

fn model_tool_hook_input(
    invocation: &ModelToolHookInvocation<'_>,
    permission_context: &ModelToolPermissionContext,
) -> Value {
    let session_id = model_tool_context_run_id(invocation.tool_input, invocation.host_context)
        .unwrap_or_else(|| "model-tool".to_owned());
    let cwd = invocation
        .tool_input
        .get("repo_root")
        .and_then(Value::as_str)
        .or_else(|| invocation.tool_input.get("cwd").and_then(Value::as_str))
        .unwrap_or(".");
    let mut input = json!({
        "session_id": session_id,
        "transcript_path": "",
        "cwd": cwd,
        "permission_mode": permission_context.permissions.mode,
        "hook_event_name": hook_event_name(invocation.event),
        "tool_name": invocation.requested_tool_name,
        "tool_input": invocation.tool_input,
        "tool_use_id": invocation.tool_use_id
    });
    if let Value::Object(object) = &mut input {
        if let Some(harness_id) = invocation.host_context.harness_id.as_deref() {
            object.insert(
                "agent_type".to_owned(),
                Value::String(harness_id.to_owned()),
            );
        }
        if let Some(agent_id) = invocation.host_context.agent_id.as_deref() {
            object.insert("agent_id".to_owned(), Value::String(agent_id.to_owned()));
            object.insert("agentId".to_owned(), Value::String(agent_id.to_owned()));
        }
        match invocation.event {
            HookEvent::PreToolUse => {}
            HookEvent::PostToolUse => {
                object.insert(
                    "tool_response".to_owned(),
                    invocation.tool_response.cloned().unwrap_or(Value::Null),
                );
            }
            HookEvent::PostToolUseFailure => {
                object.insert(
                    "error".to_owned(),
                    Value::String(invocation.tool_error.unwrap_or("").to_owned()),
                );
                object.insert("is_interrupt".to_owned(), Value::Bool(false));
            }
        }
    }
    input
}

fn model_tool_hook_context(
    store: &RunStore,
    input: &Value,
    host_context: &ModelToolHostContext,
) -> ModelToolHookContext {
    if let Some(run_id) = model_tool_context_run_id(input, host_context) {
        if let Some(config) = read_run_project_config_snapshot(store, &run_id) {
            return ModelToolHookContext {
                source: "run_config_snapshot",
                disable_all_hooks: config.disable_all_hooks,
                hooks: config.hooks,
                models: config.models,
                allowed_webhook_urls: config.allowed_webhook_urls,
                webhook_allowed_env_vars: config.webhook_allowed_env_vars,
            };
        }
    }
    let config = default_model_tool_config();
    ModelToolHookContext {
        source: "default_project_config",
        disable_all_hooks: config.disable_all_hooks,
        hooks: config.hooks.clone(),
        models: config.models.clone(),
        allowed_webhook_urls: config.allowed_webhook_urls.clone(),
        webhook_allowed_env_vars: config.webhook_allowed_env_vars.clone(),
    }
}

pub(crate) fn hook_event_name(event: HookEvent) -> &'static str {
    match event {
        HookEvent::PreToolUse => "PreToolUse",
        HookEvent::PostToolUse => "PostToolUse",
        HookEvent::PostToolUseFailure => "PostToolUseFailure",
    }
}

pub(crate) fn hook_command_kind(hook: &HookCommandSpec) -> &'static str {
    match hook {
        HookCommandSpec::Command { .. } => "command",
        HookCommandSpec::Prompt { .. } => "prompt",
        HookCommandSpec::Agent { .. } => "agent",
        HookCommandSpec::Webhook { .. } => "webhook",
    }
}

fn hook_matcher_matches_tool(
    matcher: Option<&str>,
    canonical_tool_name: &str,
    requested_tool_name: &str,
) -> bool {
    let matcher = matcher.map(str::trim).unwrap_or_default();
    if matcher.is_empty() || matcher == "*" {
        return true;
    }
    matcher.split('|').any(|candidate| {
        let candidate = normalize_hook_tool_name(candidate.trim());
        candidate == canonical_tool_name || candidate == requested_tool_name
    })
}

fn normalize_hook_tool_name(name: &str) -> &str {
    match name {
        "Bash" | "BashTool" => "command_run",
        "Read" | "View" => "repo_read_file",
        "Grep" => "repo_search_text",
        "Glob" => "repo_find_files",
        "Edit" | "Write" | "MultiEdit" => "patch_apply",
        other => other,
    }
}
