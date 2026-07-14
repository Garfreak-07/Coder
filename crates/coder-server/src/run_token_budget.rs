use coder_core::RunId;
use coder_harness::HarnessRunRequest;
use serde_json::{json, Value};

use crate::ApiState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunTokenBudgetSnapshot {
    pub(crate) limit: u64,
    pub(crate) used: u64,
}

impl RunTokenBudgetSnapshot {
    pub(crate) fn exhausted(self) -> bool {
        self.used >= self.limit
    }

    pub(crate) fn as_json(self) -> Value {
        json!({
            "limit_tokens": self.limit,
            "used_tokens": self.used,
            "remaining_tokens": self.limit.saturating_sub(self.used),
            "exhausted": self.exhausted(),
            "sampling_token_weight": 1.0,
            "non_cached_input_token_weight": 1.0
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RunTokenUsage {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) cache_read_tokens: Option<u64>,
    pub(crate) estimated_input_tokens: u64,
    pub(crate) estimated_output_tokens: u64,
}

impl RunTokenUsage {
    fn charge(self) -> u64 {
        let input = self.input_tokens.unwrap_or(self.estimated_input_tokens);
        let cache_read = self.cache_read_tokens.unwrap_or(0).min(input);
        let output = self.output_tokens.unwrap_or(self.estimated_output_tokens);
        input.saturating_sub(cache_read).saturating_add(output)
    }
}

pub(crate) fn provider_token_usage(
    request_body: &Value,
    response_payload: &Value,
) -> RunTokenUsage {
    let usage = response_payload.get("usage").unwrap_or(&Value::Null);
    let input_tokens = usage_u64(usage, &["prompt_tokens", "input_tokens"]);
    let output_tokens = usage_u64(usage, &["completion_tokens", "output_tokens"]);
    let cache_read_tokens = usage_u64(
        usage,
        &[
            "prompt_cache_hit_tokens",
            "cache_read_input_tokens",
            "cached_tokens",
        ],
    )
    .or_else(|| {
        usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
    });
    let estimated_output_tokens = response_payload
        .pointer("/choices/0/message")
        .map(|message| u64::from(crate::estimate_text_tokens(&message.to_string())))
        .unwrap_or(0);
    RunTokenUsage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        estimated_input_tokens: u64::from(crate::estimate_text_tokens(&request_body.to_string())),
        estimated_output_tokens,
    }
}

fn usage_u64(usage: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| usage.get(key).and_then(Value::as_u64))
}

pub(crate) fn workflow_token_budget(request: &HarnessRunRequest) -> Option<u64> {
    request
        .backend_context
        .pointer("/coder/task/token_budget")
        .and_then(Value::as_u64)
        .filter(|limit| *limit > 0)
}

pub(crate) fn initialize_run_token_budget(state: &ApiState, run_id: &RunId, limit: Option<u64>) {
    let Some(limit) = limit.filter(|limit| *limit > 0) else {
        return;
    };
    state.session_host.initialize_token_budget(run_id, limit);
}

pub(crate) fn check_run_token_budget(
    state: &ApiState,
    request: &HarnessRunRequest,
) -> Option<RunTokenBudgetSnapshot> {
    let limit = workflow_token_budget(request)?;
    initialize_run_token_budget(state, &request.run_id, Some(limit));
    check_existing_run_token_budget(state, &request.run_id)
}

pub(crate) fn record_run_token_usage(
    state: &ApiState,
    request: &HarnessRunRequest,
    usage: RunTokenUsage,
) -> Option<RunTokenBudgetSnapshot> {
    let limit = workflow_token_budget(request)?;
    initialize_run_token_budget(state, &request.run_id, Some(limit));
    record_existing_run_token_usage(state, &request.run_id, usage)
}

pub(crate) fn check_existing_run_token_budget(
    state: &ApiState,
    run_id: &RunId,
) -> Option<RunTokenBudgetSnapshot> {
    let (limit, used) = state.session_host.token_budget(run_id)?;
    Some(RunTokenBudgetSnapshot { limit, used })
}

pub(crate) fn record_existing_run_token_usage(
    state: &ApiState,
    run_id: &RunId,
    usage: RunTokenUsage,
) -> Option<RunTokenBudgetSnapshot> {
    let (limit, used) = state.session_host.charge_tokens(run_id, usage.charge())?;
    Some(RunTokenBudgetSnapshot { limit, used })
}

pub(crate) fn clear_run_token_budget(state: &ApiState, run_id: &RunId) {
    state.session_host.clear_token_budget(run_id);
}

pub(crate) fn clear_run_token_budget_if_inactive(state: &ApiState, run_id: &RunId) {
    let run_active = state.session_host.task_is_active(run_id);
    if run_active || crate::subagent_tools::has_background_subagents_for_run(state, run_id) {
        return;
    }
    clear_run_token_budget(state, run_id);
}
