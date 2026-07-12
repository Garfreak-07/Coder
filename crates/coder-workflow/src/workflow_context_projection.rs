use coder_config::{
    permission_policy_explanation, resolve_agent_runtime_policy, AgentRuntimePolicy, HarnessSpec,
    ModelSpec,
};
use serde_json::{json, Value};

use crate::context_budget::context_budget_for_runtime;

pub(crate) fn model_reference(model: &ModelSpec) -> Value {
    json!({
        "provider": &model.provider,
        "model": &model.model,
        "base_url_env": &model.base_url_env,
        "api_key_env": &model.api_key_env,
        "capabilities": model.resolved_capabilities()
    })
}

pub(crate) fn permission_summary(harness: &HarnessSpec) -> Value {
    let explanation = permission_policy_explanation(&harness.permissions);
    json!({
        "contract": explanation.get("contract").cloned().unwrap_or(Value::Null),
        "decisions": explanation.get("decisions").cloned().unwrap_or(Value::Null)
    })
}

pub(crate) fn agent_runtime_event_summary(
    model: &ModelSpec,
    runtime: &AgentRuntimePolicy,
) -> Value {
    let resolved = resolve_agent_runtime_policy(model, runtime);
    let budget = context_budget_for_runtime(&resolved);
    json!({
        "configured_output_cap": runtime.max_output_tokens,
        "output_cap": resolved.max_output_tokens,
        "configured_max_turns": runtime.max_turns,
        "max_turns": resolved.max_turns,
        "context_window": resolved.context_window_tokens,
        "effective_context_window": resolved.effective_context_window_tokens,
        "configured_compact_reserve": runtime.compact_output_reserve_tokens,
        "compact_reserve": resolved.compact_output_reserve_tokens,
        "auto_compact_token_limit": resolved.auto_compact_token_limit,
        "supports_streaming": resolved.supports_streaming,
        "supports_tool_calls": resolved.supports_tool_calls,
        "supports_parallel_tool_calls": resolved.supports_parallel_tool_calls,
        "output_recovery_attempts": runtime.max_output_recovery_attempts,
        "compaction_failure_limit": runtime.max_consecutive_compaction_failures,
        "context_budget": {
            "configured_context_window": budget.configured_context_window_tokens,
            "configured_auto_compact_token_limit": budget.configured_auto_compact_token_limit,
            "autocompact_threshold": budget.autocompact_threshold_tokens,
            "blocking_limit": budget.blocking_limit_tokens,
            "estimated_max_turn_growth": budget.estimated_max_turn_growth_tokens
        }
    })
}
