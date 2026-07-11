use coder_config::{
    permission_policy_explanation, AgentRuntimePolicy, AgentSpec, HarnessSpec, ModelSpec,
};
use serde_json::{json, Value};

use crate::context_budget::context_budget_for_runtime;

pub(crate) fn model_reference(agent: &AgentSpec, model: &ModelSpec) -> Value {
    json!({
        "profile_ref": &agent.model,
        "provider": &model.provider,
        "model": &model.model,
        "base_url_env": &model.base_url_env,
        "api_key_env": &model.api_key_env
    })
}

pub(crate) fn memory_scope_summary(agent: &AgentSpec, harness: &HarnessSpec) -> Value {
    json!({
        "agent": &agent.memory,
        "harness": &harness.memory,
        "note": "scope names only; memory contents are not embedded"
    })
}

pub(crate) fn permission_summary(harness: &HarnessSpec) -> Value {
    let mut explanation = permission_policy_explanation(&harness.permissions);
    if let Some(object) = explanation.as_object_mut() {
        object.insert("policy".to_owned(), json!(&harness.permissions));
        object.insert("harness_backend".to_owned(), json!(harness.backend));
        object.insert("selected_tools".to_owned(), json!(harness.tools));
    }
    explanation
}

pub(crate) fn agent_runtime_event_summary(runtime: &AgentRuntimePolicy) -> Value {
    let budget = context_budget_for_runtime(runtime);
    json!({
        "output_cap": runtime.max_output_tokens,
        "max_turns": runtime.max_turns,
        "context_window": runtime.context_window_tokens,
        "compact_reserve": runtime.compact_output_reserve_tokens,
        "autocompact_buffer": runtime.autocompact_buffer_tokens,
        "output_recovery_attempts": runtime.max_output_recovery_attempts,
        "compaction_failure_limit": runtime.max_consecutive_compaction_failures,
        "stream_idle_timeout_ms": runtime.stream_idle_timeout_ms,
        "context_budget": {
            "configured_context_window": budget.configured_context_window_tokens,
            "context_window_override": budget.context_window_override_tokens,
            "effective_context_window": budget.effective_context_window_tokens,
            "configured_autocompact_buffer": budget.configured_autocompact_buffer_tokens,
            "effective_autocompact_buffer": budget.effective_autocompact_buffer_tokens,
            "autocompact_threshold": budget.autocompact_threshold_tokens,
            "autocompact_threshold_overridden": budget.autocompact_threshold_overridden,
            "warning_threshold": budget.warning_threshold_tokens,
            "error_threshold": budget.error_threshold_tokens,
            "blocking_limit": budget.blocking_limit_tokens,
            "blocking_limit_overridden": budget.blocking_limit_overridden,
            "estimated_max_turn_growth": budget.estimated_max_turn_growth_tokens
        }
    })
}
