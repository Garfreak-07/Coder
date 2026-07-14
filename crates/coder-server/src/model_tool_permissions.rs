use std::sync::OnceLock;

use coder_config::{
    evaluate_permission, permission_policy_explanation, resolve_task_tools, PermissionDecision,
    PermissionPolicy, PermissionRuleValue, PermissionSettingsRecord, PermissionUpdate,
    PermissionUpdateDestination, ProjectConfig,
};
use coder_core::RunId;
use coder_store::{DurableJsonlPageOptions, RunStore};
use coder_tools::{builtin_tool, ToolPermission};
use coder_workflow::TurnContext;
use serde_json::{json, Value};

use crate::model_tool_run_context::latest_run_context;
use crate::model_tool_skill_context::model_tool_skill_context_modifier_permission_decision;
use crate::skill_model_tool::{load_model_skill, model_tool_skill_name};
use crate::ApiState;

pub(crate) const DEFAULT_MODEL_TOOL_PERMISSION_HARNESS_ID: &str = "native-code-edit";
static DEFAULT_MODEL_TOOL_CONFIG: OnceLock<ProjectConfig> = OnceLock::new();

pub(crate) fn default_model_tool_config() -> &'static ProjectConfig {
    DEFAULT_MODEL_TOOL_CONFIG.get_or_init(crate::default_project_config)
}

pub(crate) fn required_permission_for_model_tool(
    canonical_tool_name: &str,
) -> Option<&'static str> {
    match builtin_tool(canonical_tool_name).map(|tool| tool.permission) {
        Some(ToolPermission::None) | None => None,
        Some(permission) => Some(permission.as_str()),
    }
}

fn required_permission_for_model_tool_with_context(
    state: &ApiState,
    canonical_tool_name: &str,
    input: &Value,
    host_context: &TurnContext,
    tool_use_id: &str,
) -> Option<&'static str> {
    if canonical_tool_name == "skill" {
        return required_permission_for_skill_model_tool(state, input, host_context, tool_use_id);
    }
    required_permission_for_model_tool(canonical_tool_name)
}

fn required_permission_for_skill_model_tool(
    state: &ApiState,
    input: &Value,
    host_context: &TurnContext,
    tool_use_id: &str,
) -> Option<&'static str> {
    let skill_name = model_tool_skill_name(input)?;
    let run_id = model_tool_context_run_id(input, host_context)?;
    match load_model_skill(state, &skill_name, &run_id, tool_use_id) {
        Ok(Some(skill)) if skill.execution_policy.context == "fork" => {
            Some("child_harness_permissions")
        }
        _ => required_permission_for_model_tool("skill"),
    }
}

pub(crate) struct ModelToolPermissionContext {
    pub(crate) harness_id: String,
    pub(crate) requested_harness_id: Option<String>,
    pub(crate) source: &'static str,
    pub(crate) permissions: PermissionPolicy,
}

pub(crate) fn model_tool_permission_phase_payload(
    state: &ApiState,
    canonical_tool_name: &str,
    tool_use_id: &str,
    input: &Value,
    host_context: &TurnContext,
) -> Value {
    let store = &state.store;
    let required_permission = required_permission_for_model_tool_with_context(
        state,
        canonical_tool_name,
        input,
        host_context,
        tool_use_id,
    );
    let approved_supplied = input
        .get("approved")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let context = model_tool_permission_context(store, input, host_context);
    let permission_result = required_permission
        .and_then(|permission| evaluate_permission(&context.permissions, permission))
        .and_then(|evaluation| serde_json::to_value(evaluation).ok());
    let policy_decision_status = model_tool_policy_decision_status(
        required_permission,
        permission_result.as_ref(),
        approved_supplied,
    );
    let skill_context_modifier_decision = model_tool_skill_context_modifier_permission_decision(
        required_permission,
        canonical_tool_name,
        input,
        host_context,
        policy_decision_status,
    );
    let agent_tool_allowlist_decision =
        model_tool_agent_tool_allowlist_decision(state, canonical_tool_name, input, host_context);
    let agent_tool_deny_rule_decision =
        model_tool_agent_deny_rule_decision(state, canonical_tool_name, input, host_context);
    let mut effective_policy_decision_status = if skill_context_modifier_decision.allowed {
        "allowed_by_skill_context_modifier"
    } else {
        policy_decision_status
    };
    if agent_tool_deny_rule_decision.is_some() {
        effective_policy_decision_status = "denied_by_agent_type_rule";
    } else if agent_tool_allowlist_decision
        .as_ref()
        .and_then(|decision| decision.get("allowed"))
        .and_then(Value::as_bool)
        == Some(false)
        && model_tool_permission_allows_execution(effective_policy_decision_status)
    {
        effective_policy_decision_status = "denied_by_agent_tool_allowlist";
    }

    let mut payload = json!({
        "required_permission": required_permission,
        "approved_supplied": approved_supplied,
        "permission_policy_source": {
            "type": context.source,
            "harness_id": context.harness_id,
            "requested_harness_id": context.requested_harness_id
        },
        "permission_policy": permission_policy_explanation(&context.permissions),
        "permission_result": permission_result,
        "policy_decision_status": effective_policy_decision_status
    });
    if let Some(decision) = skill_context_modifier_decision.payload {
        if let Some(object) = payload.as_object_mut() {
            object.insert("skill_context_modifier".to_owned(), decision);
        }
    }
    if let Some(decision) = agent_tool_allowlist_decision {
        if let Some(object) = payload.as_object_mut() {
            object.insert("agent_tool_allowed_types".to_owned(), decision);
        }
    }
    if let Some(decision) = agent_tool_deny_rule_decision {
        if let Some(object) = payload.as_object_mut() {
            object.insert("agent_tool_deny_rule".to_owned(), decision);
        }
    }
    payload
}

fn model_tool_agent_deny_rule_decision(
    state: &ApiState,
    canonical_tool_name: &str,
    input: &Value,
    host_context: &TurnContext,
) -> Option<Value> {
    if canonical_tool_name != "agent_subagent" {
        return None;
    }
    let requested_subagent_type = model_tool_input_string(input, &["subagent_type"])?;
    if let Some(run_id) = model_tool_context_run_id(input, host_context) {
        if let Some(runtime_rule) =
            runtime_agent_deny_rule_decision(&state.store, &run_id, &requested_subagent_type)
        {
            return Some(runtime_rule);
        }
    }
    for destination in [
        PermissionUpdateDestination::LocalSettings,
        PermissionUpdateDestination::ProjectSettings,
        PermissionUpdateDestination::UserSettings,
    ] {
        let Some(settings) = state
            .store
            .read_permission_settings::<PermissionSettingsRecord>(destination.as_str())
            .ok()
            .flatten()
        else {
            continue;
        };
        if let Some(rule) = settings
            .rules
            .deny
            .iter()
            .find(|rule| agent_deny_rule_matches(rule, &requested_subagent_type))
        {
            return Some(json!({
                "contract": "coder.agent_tool_deny_rule.v1",
                "source": "persisted PermissionSettingsRecord deny rules",
                "destination": destination.as_str(),
                "requested_subagent_type": requested_subagent_type,
                "rule": {
                    "toolName": &rule.tool_name,
                    "ruleContent": &rule.rule_content,
                    "ruleBehavior": "deny"
                }
            }));
        }
    }
    None
}

fn runtime_agent_deny_rule_decision(
    store: &RunStore,
    run_id: &str,
    requested_subagent_type: &str,
) -> Option<Value> {
    let run_id = RunId::from_string(run_id.to_owned());
    let page = store
        .read_events_page(&run_id, DurableJsonlPageOptions::tail(1000).ok()?)
        .ok()?;
    let mut denied_rules = Vec::<RuntimeAgentDenyRule>::new();
    for event in page
        .records
        .iter()
        .filter(|event| event.kind == "permission.updated")
    {
        let Some(updates) = event.payload.get("updates").and_then(Value::as_array) else {
            continue;
        };
        for update in updates {
            let Ok(update) = serde_json::from_value::<PermissionUpdate>(update.clone()) else {
                continue;
            };
            apply_runtime_agent_update(&mut denied_rules, &update);
        }
    }
    denied_rules
        .into_iter()
        .find(|rule| rule.rule_content == requested_subagent_type)
        .map(|rule| {
            json!({
                "contract": "coder.agent_tool_deny_rule.v1",
                "source": "runtime permission.updated event rules",
                "destination": rule.destination.as_str(),
                "requested_subagent_type": requested_subagent_type,
                "rule": {
                    "toolName": rule.tool_name,
                    "ruleContent": rule.rule_content,
                    "ruleBehavior": "deny"
                }
            })
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeAgentDenyRule {
    destination: PermissionUpdateDestination,
    tool_name: String,
    rule_content: String,
}

fn apply_runtime_agent_update(
    denied_rules: &mut Vec<RuntimeAgentDenyRule>,
    update: &PermissionUpdate,
) {
    let destination = update.destination();
    if !matches!(
        destination,
        PermissionUpdateDestination::Session | PermissionUpdateDestination::CliArg
    ) {
        return;
    }
    match update {
        PermissionUpdate::AddRules {
            behavior, rules, ..
        } if *behavior == PermissionDecision::Deny => {
            for rule in rules
                .iter()
                .filter_map(|rule| runtime_agent_deny_rule(destination, rule))
            {
                if !denied_rules.iter().any(|existing| existing == &rule) {
                    denied_rules.push(rule);
                }
            }
        }
        PermissionUpdate::ReplaceRules {
            behavior, rules, ..
        } if *behavior == PermissionDecision::Deny => {
            denied_rules.retain(|rule| rule.destination != destination);
            denied_rules.extend(
                rules
                    .iter()
                    .filter_map(|rule| runtime_agent_deny_rule(destination, rule)),
            );
        }
        PermissionUpdate::RemoveRules {
            behavior, rules, ..
        } if *behavior == PermissionDecision::Deny => {
            let removed = rules
                .iter()
                .filter_map(|rule| runtime_agent_deny_rule(destination, rule))
                .collect::<Vec<_>>();
            denied_rules.retain(|rule| !removed.iter().any(|removed| removed == rule));
        }
        _ => {}
    }
}

fn runtime_agent_deny_rule(
    destination: PermissionUpdateDestination,
    rule: &PermissionRuleValue,
) -> Option<RuntimeAgentDenyRule> {
    if !matches!(
        normalized_agent_rule_tool_name(&rule.tool_name),
        Some("agent_subagent")
    ) {
        return None;
    }
    Some(RuntimeAgentDenyRule {
        destination,
        tool_name: rule.tool_name.clone(),
        rule_content: normalized_agent_rule_content(rule.rule_content.as_deref())?,
    })
}

fn agent_deny_rule_matches(rule: &PermissionRuleValue, requested_subagent_type: &str) -> bool {
    if !matches!(
        normalized_agent_rule_tool_name(&rule.tool_name),
        Some("agent_subagent")
    ) {
        return false;
    }
    let Some(rule_content) = normalized_agent_rule_content(rule.rule_content.as_deref()) else {
        return false;
    };
    rule_content == requested_subagent_type
}

fn normalized_agent_rule_tool_name(tool_name: &str) -> Option<&'static str> {
    match tool_name.trim() {
        "Agent" | "agent" | "Task" | "task" | "agent_subagent" | "subagent" => {
            Some("agent_subagent")
        }
        _ => None,
    }
}

fn normalized_agent_rule_content(rule_content: Option<&str>) -> Option<String> {
    let content = rule_content?.trim();
    if content.is_empty() || content == "*" {
        return None;
    }
    if let Some((name, rest)) = content.split_once('(') {
        if normalized_agent_rule_tool_name(name).is_some() {
            return rest
                .strip_suffix(')')
                .map(str::trim)
                .filter(|value| !value.is_empty() && *value != "*")
                .map(str::to_owned);
        }
    }
    Some(content.to_owned())
}

fn model_tool_agent_tool_allowlist_decision(
    state: &ApiState,
    canonical_tool_name: &str,
    input: &Value,
    host_context: &TurnContext,
) -> Option<Value> {
    if canonical_tool_name != "agent_subagent" {
        return None;
    }
    let run_id = model_tool_context_run_id(input, host_context);
    let run_context = run_id
        .as_deref()
        .and_then(|run_id| latest_run_context(&state.store, run_id));
    let config_snapshot = run_id
        .as_deref()
        .and_then(|run_id| read_run_project_config_snapshot(&state.store, run_id));
    let config = config_snapshot
        .as_ref()
        .unwrap_or_else(|| default_model_tool_config());
    let parent_agent_id = host_context
        .agent_id
        .as_ref()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            run_context
                .as_ref()
                .and_then(|context| context.agent_id.clone())
        })
        .or_else(|| model_tool_input_string(input, &["parent_agent_id"]));
    let parent_harness_id = host_context
        .harness_id
        .as_ref()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            run_context
                .as_ref()
                .and_then(|context| context.harness_id.clone())
        })
        .or_else(|| model_tool_input_string(input, &["parent_harness_id", "harness_id"]))
        .unwrap_or_else(|| DEFAULT_MODEL_TOOL_PERMISSION_HARNESS_ID.to_owned());
    let parent_agent_id = parent_agent_id?;
    let configured_resolution = config
        .task_profiles
        .get(&parent_agent_id)
        .zip(config.harnesses.get(&parent_harness_id))
        .map(|(profile, harness)| resolve_task_tools(profile, harness));
    let inherited_selected_tools = input
        .pointer("/backend_context/coder/harness/selected_tools")
        .and_then(Value::as_array);
    let (tool_selected, allowed_agent_types, selection_source) =
        if let Some(resolution) = configured_resolution {
            (
                resolution
                    .selected_tools
                    .iter()
                    .any(|tool| tool == "agent_subagent"),
                resolution.allowed_agent_types,
                "project_config",
            )
        } else {
            let selected_tools = inherited_selected_tools?;
            (
                selected_tools.iter().filter_map(Value::as_str).any(|tool| {
                    crate::model_tool_input::canonical_model_tool_name(tool) == "agent_subagent"
                }),
                None,
                "inherited_backend_context",
            )
        };
    let requested_subagent_type = model_tool_input_string(input, &["subagent_type"]);
    let (allowed, reason) = if !tool_selected {
        (false, "agent_tool_not_selected_for_parent_agent")
    } else if let Some(allowed_agent_types) = allowed_agent_types.as_ref() {
        match requested_subagent_type.as_deref() {
            Some(subagent_type) if allowed_agent_types.iter().any(|item| item == subagent_type) => {
                (true, "subagent_type_allowed")
            }
            Some(_) => (false, "subagent_type_not_allowed"),
            None => (false, "missing_subagent_type"),
        }
    } else {
        (true, "unrestricted_agent_tool")
    };
    Some(json!({
        "contract": "coder.agent_tool_allowed_types.v1",
        "source": "ProjectConfig.task_profiles tools Agent(...) plus harness.tools",
        "run_id": run_id,
        "parent_agent_id": parent_agent_id,
        "parent_harness_id": parent_harness_id,
        "tool_selected": tool_selected,
        "selection_source": selection_source,
        "requested_subagent_type": requested_subagent_type,
        "allowed_agent_types": allowed_agent_types,
        "allowed": allowed,
        "reason": reason
    }))
}

fn model_tool_input_string(input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        input
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

pub(crate) fn model_tool_permission_context(
    store: &RunStore,
    input: &Value,
    host_context: &TurnContext,
) -> ModelToolPermissionContext {
    let run_id = model_tool_context_run_id(input, host_context);
    let input_harness_id = input
        .get("harness_id")
        .or_else(|| input.get("parent_harness_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|harness_id| !harness_id.is_empty())
        .map(str::to_owned);
    let host_harness_id = host_context
        .harness_id
        .as_ref()
        .cloned()
        .map(|value| value.trim().to_owned())
        .filter(|harness_id| !harness_id.is_empty());
    if let Some(permissions) = host_context.permission_policy.as_ref() {
        let requested_harness_id = host_harness_id.clone().or_else(|| input_harness_id.clone());
        return ModelToolPermissionContext {
            harness_id: requested_harness_id
                .clone()
                .unwrap_or_else(|| DEFAULT_MODEL_TOOL_PERMISSION_HARNESS_ID.to_owned()),
            requested_harness_id,
            source: "turn_context_snapshot",
            permissions: permissions.clone(),
        };
    }
    if let (Some(run_id), Some(harness_id)) = (run_id.as_deref(), host_harness_id.as_deref()) {
        if let Some(context) = model_tool_permission_context_from_run_config(
            store,
            run_id,
            harness_id,
            Some(harness_id.to_owned()),
            "run_config_snapshot_host_context",
        ) {
            return context;
        }
    }

    if let Some(run_id) = run_id.as_deref() {
        if let Some(harness_id) = latest_run_context(store, run_id).and_then(|context| {
            context
                .harness_id
                .map(|value| value.trim().to_owned())
                .filter(|harness_id| !harness_id.is_empty())
        }) {
            if let Some(context) = model_tool_permission_context_from_run_config(
                store,
                run_id,
                &harness_id,
                Some(harness_id.clone()),
                "run_config_snapshot_event_inferred",
            ) {
                return context;
            }
        }
    }

    if let (Some(run_id), Some(harness_id)) = (run_id.as_deref(), input_harness_id.as_deref()) {
        if let Some(context) = model_tool_permission_context_from_run_config(
            store,
            run_id,
            harness_id,
            Some(harness_id.to_owned()),
            "run_config_snapshot_model_input_fallback",
        ) {
            return context;
        }
    }

    let using_host_harness = host_harness_id.is_some();
    let requested_harness_id = host_harness_id.or_else(|| input_harness_id.clone());
    let harness_id = requested_harness_id
        .as_deref()
        .unwrap_or(DEFAULT_MODEL_TOOL_PERMISSION_HARNESS_ID);
    let config = default_model_tool_config();
    let source = if using_host_harness {
        "host_context"
    } else if input_harness_id.is_some() {
        "model_tool_input_fallback"
    } else {
        "default_project_config"
    };

    if let Some(harness) = config.harnesses.get(harness_id) {
        return ModelToolPermissionContext {
            harness_id: harness_id.to_owned(),
            requested_harness_id,
            source,
            permissions: harness.permissions.clone(),
        };
    }

    if harness_id != DEFAULT_MODEL_TOOL_PERMISSION_HARNESS_ID {
        if let Some(harness) = config
            .harnesses
            .get(DEFAULT_MODEL_TOOL_PERMISSION_HARNESS_ID)
        {
            return ModelToolPermissionContext {
                harness_id: DEFAULT_MODEL_TOOL_PERMISSION_HARNESS_ID.to_owned(),
                requested_harness_id,
                source: "default_project_config_fallback",
                permissions: harness.permissions.clone(),
            };
        }
    }

    ModelToolPermissionContext {
        harness_id: harness_id.to_owned(),
        requested_harness_id,
        source: "permission_policy_default",
        permissions: PermissionPolicy::default(),
    }
}

pub(crate) fn model_tool_context_run_id(
    input: &Value,
    host_context: &TurnContext,
) -> Option<String> {
    host_context
        .run_id
        .as_deref()
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            input
                .get("run_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|run_id| !run_id.is_empty())
                .map(str::to_owned)
        })
}

fn model_tool_permission_context_from_run_config(
    store: &RunStore,
    run_id: &str,
    harness_id: &str,
    requested_harness_id: Option<String>,
    source: &'static str,
) -> Option<ModelToolPermissionContext> {
    let config = read_run_project_config_snapshot(store, run_id)?;
    let harness = config.harnesses.get(harness_id)?;
    Some(ModelToolPermissionContext {
        harness_id: harness_id.to_owned(),
        requested_harness_id,
        source,
        permissions: harness.permissions.clone(),
    })
}

pub(crate) fn read_run_project_config_snapshot(
    store: &RunStore,
    run_id: &str,
) -> Option<ProjectConfig> {
    store
        .read_run_config_snapshot_json(&RunId::from_string(run_id.to_owned()))
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn model_tool_policy_decision_status(
    required_permission: Option<&str>,
    permission_result: Option<&Value>,
    approved_supplied: bool,
) -> &'static str {
    let Some(_required_permission) = required_permission else {
        return "not_applicable";
    };
    let Some(permission_result) = permission_result else {
        return "unresolved_permission";
    };
    match permission_result.get("behavior").and_then(Value::as_str) {
        Some("allow") => "allowed_by_policy",
        Some("ask") if approved_supplied => "confirmation_supplied",
        Some("ask") => "requires_confirmation",
        Some("deny") => "denied_by_policy",
        _ => "unknown_policy_behavior",
    }
}

pub(crate) fn model_tool_permission_allows_execution(policy_decision_status: &str) -> bool {
    matches!(
        policy_decision_status,
        "allowed_by_policy"
            | "confirmation_supplied"
            | "allowed_by_skill_context_modifier"
            | "not_applicable"
    )
}

pub(crate) fn model_tool_permission_phase_status(policy_decision_status: &str) -> &'static str {
    if model_tool_permission_allows_execution(policy_decision_status) {
        "delegated_to_tool_endpoint"
    } else {
        "blocked_before_tool_endpoint"
    }
}
