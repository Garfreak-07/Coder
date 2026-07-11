use coder_config::AgentRuntimePolicy;
use coder_store::CompactionCircuitState;
use serde_json::{json, Map, Value};
use time::format_description::well_known::Rfc3339;

use crate::context_budget::context_budget_for_runtime;

const PRIMARY_MAX_STRING_CHARS: usize = 2_000;
const PRIMARY_MAX_ARRAY_ITEMS: usize = 30;
const PRIMARY_MAX_OBJECT_KEYS: usize = 80;
const PRIMARY_MAX_DEPTH: usize = 6;
const AGGRESSIVE_MAX_STRING_CHARS: usize = 500;
const AGGRESSIVE_MAX_ARRAY_ITEMS: usize = 12;
const AGGRESSIVE_MAX_OBJECT_KEYS: usize = 40;
const AGGRESSIVE_MAX_DEPTH: usize = 4;

#[derive(Debug, Clone)]
pub struct ContextCompactionOutput {
    pub plan_context: Option<Value>,
    pub report: Value,
    pub circuit_outcome: Option<ContextCompactionCircuitOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextCompactionCircuitOutcome {
    Success,
    Failure,
}

impl ContextCompactionCircuitOutcome {
    pub fn success(self) -> bool {
        matches!(self, Self::Success)
    }
}

pub fn compact_plan_context_with_circuit(
    plan_context: Option<&Value>,
    runtime: &AgentRuntimePolicy,
    circuit_state: Option<&CompactionCircuitState>,
) -> ContextCompactionOutput {
    let budget = context_budget_for_runtime(runtime);
    let original_tokens = plan_context.map(estimate_json_tokens).unwrap_or(0);
    let original_projected_tokens =
        original_tokens.saturating_add(budget.estimated_max_turn_growth_tokens);
    let circuit = compaction_circuit_report(runtime, circuit_state);
    let base_report = json!({
        "contract": "coder.context_compaction.v1",
        "strategy": "deterministic_json_compaction",
        "threshold_tokens": budget.autocompact_threshold_tokens,
        "blocking_limit_tokens": budget.blocking_limit_tokens,
        "estimated_max_turn_growth_tokens": budget.estimated_max_turn_growth_tokens,
        "original_estimated_tokens": original_tokens,
        "original_projected_tokens": original_projected_tokens,
        "max_consecutive_failures": circuit.max_consecutive_failures,
        "consecutive_failures": circuit.consecutive_failures,
        "circuit_breaker_open": circuit.circuit_breaker_open,
        "circuit_state_loaded": circuit.state_loaded,
        "circuit_scope_id": circuit.scope_id,
        "circuit_updated_at": circuit.updated_at,
        "persisted_max_consecutive_failures": circuit.persisted_max_consecutive_failures
    });

    let Some(plan_context) = plan_context else {
        return ContextCompactionOutput {
            plan_context: None,
            report: report_with_status(base_report, "not_needed", false, 0, 0),
            circuit_outcome: None,
        };
    };

    if original_projected_tokens <= budget.autocompact_threshold_tokens {
        return ContextCompactionOutput {
            plan_context: Some(plan_context.clone()),
            report: report_with_status(
                base_report,
                "not_needed",
                false,
                original_tokens,
                original_projected_tokens,
            ),
            circuit_outcome: None,
        };
    }

    let primary = compact_value(
        plan_context,
        PRIMARY_MAX_DEPTH,
        PRIMARY_MAX_STRING_CHARS,
        PRIMARY_MAX_ARRAY_ITEMS,
        PRIMARY_MAX_OBJECT_KEYS,
    );
    let primary_tokens = estimate_json_tokens(&primary);
    let primary_projected_tokens =
        primary_tokens.saturating_add(budget.estimated_max_turn_growth_tokens);
    if primary_projected_tokens <= budget.blocking_limit_tokens {
        return ContextCompactionOutput {
            plan_context: Some(primary),
            report: report_with_status(
                base_report,
                "completed",
                true,
                primary_tokens,
                primary_projected_tokens,
            ),
            circuit_outcome: Some(ContextCompactionCircuitOutcome::Success),
        };
    }

    let aggressive = compact_value(
        plan_context,
        AGGRESSIVE_MAX_DEPTH,
        AGGRESSIVE_MAX_STRING_CHARS,
        AGGRESSIVE_MAX_ARRAY_ITEMS,
        AGGRESSIVE_MAX_OBJECT_KEYS,
    );
    let aggressive_tokens = estimate_json_tokens(&aggressive);
    let aggressive_projected_tokens =
        aggressive_tokens.saturating_add(budget.estimated_max_turn_growth_tokens);
    let status = if aggressive_projected_tokens <= budget.blocking_limit_tokens {
        "completed_aggressive"
    } else {
        "over_blocking_limit"
    };
    ContextCompactionOutput {
        plan_context: Some(aggressive),
        report: report_with_status(
            base_report,
            status,
            true,
            aggressive_tokens,
            aggressive_projected_tokens,
        ),
        circuit_outcome: Some(if status == "over_blocking_limit" {
            ContextCompactionCircuitOutcome::Failure
        } else {
            ContextCompactionCircuitOutcome::Success
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompactionCircuitReport {
    max_consecutive_failures: u8,
    consecutive_failures: u8,
    circuit_breaker_open: bool,
    state_loaded: bool,
    scope_id: Option<String>,
    updated_at: Option<String>,
    persisted_max_consecutive_failures: Option<u8>,
}

fn compaction_circuit_report(
    runtime: &AgentRuntimePolicy,
    state: Option<&CompactionCircuitState>,
) -> CompactionCircuitReport {
    let max_consecutive_failures = runtime.max_consecutive_compaction_failures;
    let consecutive_failures = state
        .map(|state| state.consecutive_failures)
        .unwrap_or_default();
    CompactionCircuitReport {
        max_consecutive_failures,
        consecutive_failures,
        circuit_breaker_open: max_consecutive_failures > 0
            && consecutive_failures >= max_consecutive_failures,
        state_loaded: state.is_some(),
        scope_id: state.map(|state| state.scope_id.clone()),
        updated_at: state.and_then(|state| state.updated_at.format(&Rfc3339).ok()),
        persisted_max_consecutive_failures: state.map(|state| state.max_consecutive_failures),
    }
}

fn report_with_status(
    mut report: Value,
    status: &str,
    applied: bool,
    compacted_tokens: u32,
    compacted_projected_tokens: u32,
) -> Value {
    report["status"] = Value::String(status.to_owned());
    report["applied"] = Value::Bool(applied);
    report["compacted_estimated_tokens"] = json!(compacted_tokens);
    report["compacted_projected_tokens"] = json!(compacted_projected_tokens);
    report
}

fn compact_value(
    value: &Value,
    max_depth: usize,
    max_string_chars: usize,
    max_array_items: usize,
    max_object_keys: usize,
) -> Value {
    if max_depth == 0 {
        return compact_leaf(value);
    }
    match value {
        Value::String(text) => Value::String(compact_string(text, max_string_chars)),
        Value::Array(items) => {
            let omitted_items = items.len().saturating_sub(max_array_items);
            let mut compacted = items
                .iter()
                .take(max_array_items)
                .map(|item| {
                    compact_value(
                        item,
                        max_depth - 1,
                        max_string_chars,
                        max_array_items,
                        max_object_keys,
                    )
                })
                .collect::<Vec<_>>();
            if omitted_items > 0 {
                compacted.push(json!({
                    "_compacted": true,
                    "omitted_items": omitted_items
                }));
            }
            Value::Array(compacted)
        }
        Value::Object(object) => {
            let omitted_keys = object.len().saturating_sub(max_object_keys);
            let mut compacted = Map::new();
            for (key, value) in object.iter().take(max_object_keys) {
                compacted.insert(
                    key.clone(),
                    compact_value(
                        value,
                        max_depth - 1,
                        max_string_chars,
                        max_array_items,
                        max_object_keys,
                    ),
                );
            }
            if omitted_keys > 0 {
                compacted.insert(
                    "_compacted_omitted_keys".to_owned(),
                    json!({
                        "count": omitted_keys,
                        "sample": object.keys().skip(max_object_keys).take(20).cloned().collect::<Vec<_>>()
                    }),
                );
            }
            Value::Object(compacted)
        }
        other => other.clone(),
    }
}

fn compact_leaf(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(compact_string(text, 160)),
        Value::Array(items) => json!({
            "_compacted": true,
            "type": "array",
            "items": items.len()
        }),
        Value::Object(object) => json!({
            "_compacted": true,
            "type": "object",
            "keys": object.keys().take(20).cloned().collect::<Vec<_>>(),
            "omitted_keys": object.len().saturating_sub(20)
        }),
        other => other.clone(),
    }
}

fn compact_string(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut compacted = value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    compacted.truncate(compacted.trim_end().len());
    compacted.push_str("...");
    compacted
}

fn estimate_json_tokens(value: &Value) -> u32 {
    let chars = serde_json::to_string(value)
        .map(|text| text.chars().count())
        .unwrap_or_default();
    chars.div_ceil(4).max(1).min(u32::MAX as usize) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_plan_context_does_not_compact() {
        let context = json!({
            "plan_draft": {
                "goal": "Update docs",
                "acceptance_criteria": ["tests pass"]
            }
        });

        let output =
            compact_plan_context_with_circuit(Some(&context), &AgentRuntimePolicy::default(), None);

        assert_eq!(output.report["status"], "not_needed");
        assert_eq!(output.report["applied"], false);
        assert_eq!(output.plan_context, Some(context));
    }

    #[test]
    fn large_plan_context_compacts_before_backend_payload() {
        let runtime = AgentRuntimePolicy {
            context_window_tokens: 32_000,
            compact_output_reserve_tokens: 1_000,
            autocompact_buffer_tokens: 1_000,
            max_output_tokens: Some(8_000),
            ..AgentRuntimePolicy::default()
        };
        let huge = "x".repeat(140_000);
        let context = json!({
            "original_user_request": huge,
            "acceptance_criteria": (0..100).map(|index| format!("criterion-{index}")).collect::<Vec<_>>()
        });

        let output = compact_plan_context_with_circuit(Some(&context), &runtime, None);
        assert_eq!(
            output.circuit_outcome,
            Some(ContextCompactionCircuitOutcome::Success)
        );
        let compacted = output.plan_context.unwrap();

        assert_eq!(output.report["applied"], true);
        assert!(matches!(
            output.report["status"].as_str(),
            Some("completed" | "completed_aggressive" | "over_blocking_limit")
        ));
        assert!(estimate_json_tokens(&compacted) < estimate_json_tokens(&context));
        assert!(compacted["original_user_request"]
            .as_str()
            .unwrap()
            .ends_with("..."));
    }
}
