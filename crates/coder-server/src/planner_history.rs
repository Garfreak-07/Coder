use coder_store::RunStore;
use serde_json::{json, Value};

use crate::{
    estimate_text_tokens, ApiError, PlannerChatTurn, PlannerConversationRequest,
    PLANNER_CHAT_HISTORY_RECENT_TURN_LIMIT,
};

pub(crate) struct CompactedPlannerHistory<'a> {
    pub(crate) summary: Option<String>,
    pub(crate) recent_turns: Vec<&'a PlannerChatTurn>,
    pub(crate) report: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct PlannerHistoryCompactionAttempt {
    scope_id: String,
    max_consecutive_failures: u8,
    omitted_turns: usize,
    recent_turn_limit: usize,
}

pub(crate) fn planner_history_compaction_attempt(
    request: &PlannerConversationRequest,
) -> Option<PlannerHistoryCompactionAttempt> {
    if request.history.len() <= PLANNER_CHAT_HISTORY_RECENT_TURN_LIMIT {
        return None;
    }
    Some(PlannerHistoryCompactionAttempt {
        scope_id: format!("planner-chat-{}", request.session_id),
        max_consecutive_failures: request
            .runtime
            .agent
            .runtime
            .max_consecutive_compaction_failures,
        omitted_turns: request.history.len() - PLANNER_CHAT_HISTORY_RECENT_TURN_LIMIT,
        recent_turn_limit: PLANNER_CHAT_HISTORY_RECENT_TURN_LIMIT,
    })
}

pub(crate) fn record_planner_history_compaction_outcome(
    store: &RunStore,
    attempt: PlannerHistoryCompactionAttempt,
) -> Result<Value, ApiError> {
    let state = store.record_compaction_circuit_outcome(
        &attempt.scope_id,
        attempt.max_consecutive_failures,
        true,
    )?;
    Ok(json!({
        "contract": "coder.planner_history_compaction.v1",
        "strategy": "deterministic_recent_turn_summary",
        "scope_id": attempt.scope_id,
        "success": true,
        "outcome": "success",
        "omitted_turns": attempt.omitted_turns,
        "recent_turn_limit": attempt.recent_turn_limit,
        "consecutive_failures": state.consecutive_failures,
        "max_consecutive_failures": state.max_consecutive_failures,
        "circuit_breaker_open": state.circuit_breaker_open
    }))
}

pub(crate) fn compact_planner_history(
    history: &[PlannerChatTurn],
    recent_turn_limit: usize,
) -> CompactedPlannerHistory<'_> {
    if history.len() <= recent_turn_limit {
        return CompactedPlannerHistory {
            summary: None,
            recent_turns: history.iter().collect(),
            report: json!({
                "contract": "coder.planner_history_compaction.v1",
                "strategy": "deterministic_recent_turn_summary",
                "status": "not_needed",
                "applied": false,
                "total_turns": history.len(),
                "omitted_turns": 0,
                "recent_turn_limit": recent_turn_limit,
                "recent_turns": history.len(),
                "summary_estimated_tokens": 0,
                "kept_estimated_tokens": estimate_planner_history_tokens(history),
                "original_estimated_tokens": estimate_planner_history_tokens(history)
            }),
        };
    }
    let omitted = history.len() - recent_turn_limit;
    let recent_turns = history
        .iter()
        .skip(omitted)
        .collect::<Vec<&PlannerChatTurn>>();
    let user_turns = history[..omitted]
        .iter()
        .filter(|turn| turn.role == "user")
        .count();
    let assistant_turns = history[..omitted]
        .iter()
        .filter(|turn| turn.role == "assistant")
        .count();
    let artifact_count = history[..omitted]
        .iter()
        .map(|turn| turn.artifacts.len())
        .sum::<usize>();
    let first_user = history[..omitted]
        .iter()
        .find(|turn| turn.role == "user")
        .map(|turn| compact_planner_summary_text(&turn.content, 240));
    let last_assistant = history[..omitted]
        .iter()
        .rev()
        .find(|turn| turn.role == "assistant")
        .map(|turn| compact_planner_summary_text(&turn.content, 320));
    let mut summary = format!(
        "Compacted earlier planner chat history: omitted_turns={omitted}; user_turns={user_turns}; assistant_turns={assistant_turns}; artifact_count={artifact_count}."
    );
    if let Some(first_user) = first_user {
        summary.push_str("\nFirst earlier user request: ");
        summary.push_str(&first_user);
    }
    if let Some(last_assistant) = last_assistant {
        summary.push_str("\nLast earlier assistant summary: ");
        summary.push_str(&last_assistant);
    }
    let summary_estimated_tokens = estimate_text_tokens(&summary);
    let kept_estimated_tokens = summary_estimated_tokens
        .saturating_add(estimate_planner_history_tokens(&history[omitted..]));
    let original_estimated_tokens = estimate_planner_history_tokens(history);
    CompactedPlannerHistory {
        summary: Some(summary),
        recent_turns,
        report: json!({
            "contract": "coder.planner_history_compaction.v1",
            "strategy": "deterministic_recent_turn_summary",
            "status": "completed",
            "applied": true,
            "total_turns": history.len(),
            "omitted_turns": omitted,
            "recent_turn_limit": recent_turn_limit,
            "recent_turns": recent_turn_limit,
            "omitted_user_turns": user_turns,
            "omitted_assistant_turns": assistant_turns,
            "omitted_artifact_count": artifact_count,
            "summary_estimated_tokens": summary_estimated_tokens,
            "kept_estimated_tokens": kept_estimated_tokens,
            "original_estimated_tokens": original_estimated_tokens,
            "token_savings_estimate": original_estimated_tokens.saturating_sub(kept_estimated_tokens)
        }),
    }
}

fn estimate_planner_history_tokens(history: &[PlannerChatTurn]) -> u32 {
    history
        .iter()
        .map(|turn| {
            estimate_text_tokens(&turn.content)
                .saturating_add((turn.artifacts.len() as u32).saturating_mul(128))
        })
        .sum()
}

fn compact_planner_summary_text(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut compacted = normalized
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    compacted.truncate(compacted.trim_end().len());
    compacted.push_str("...");
    compacted
}
