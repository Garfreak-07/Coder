use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::Value;

use crate::{
    ApiError, ApiState, GoalBlockedAttemptRequest, GoalClearResponse, GoalCreateRequest,
    GoalGetResponse, GoalMutationResponse, GoalRuntimePolicy, GoalState, GoalStatus,
    GoalTokenUpdateRequest,
};

pub(crate) const BLOCKED_CONSECUTIVE_THRESHOLD: u32 = 3;
pub(crate) const MAX_GOAL_TURNS: u32 = 150;

pub(crate) fn goal_runtime_policy() -> GoalRuntimePolicy {
    GoalRuntimePolicy {
        blocked_consecutive_threshold: BLOCKED_CONSECUTIVE_THRESHOLD,
        max_goal_turns: MAX_GOAL_TURNS,
        claude_sources: vec![
            "src/services/goal/goalState.ts BLOCKED_CONSECUTIVE_THRESHOLD = 3",
            "src/services/goal/goalState.ts MAX_GOAL_TURNS = 150",
            "src/services/goal/goalStorage.ts persisted per-session goal state",
        ],
    }
}

pub(crate) async fn get_goal_endpoint(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<GoalGetResponse>, ApiError> {
    Ok(Json(GoalGetResponse {
        goal: read_goal(&state, &session_id)?,
        policy: goal_runtime_policy(),
    }))
}

pub(crate) async fn create_goal_endpoint(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Json(request): Json<GoalCreateRequest>,
) -> Result<Json<GoalMutationResponse>, ApiError> {
    let goal = new_goal_state(session_id, request)?;
    persist_goal(&state, goal)
}

pub(crate) async fn pause_goal_endpoint(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<GoalMutationResponse>, ApiError> {
    mutate_goal(&state, &session_id, pause_goal)
}

pub(crate) async fn resume_goal_endpoint(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<GoalMutationResponse>, ApiError> {
    mutate_goal(&state, &session_id, resume_goal)
}

pub(crate) async fn complete_goal_endpoint(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<GoalMutationResponse>, ApiError> {
    mutate_goal(&state, &session_id, complete_goal)
}

pub(crate) async fn update_goal_tokens_endpoint(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Json(request): Json<GoalTokenUpdateRequest>,
) -> Result<Json<GoalMutationResponse>, ApiError> {
    mutate_goal(&state, &session_id, |goal| {
        update_goal_tokens(goal, request.delta)
    })
}

pub(crate) async fn increment_goal_turn_endpoint(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<GoalMutationResponse>, ApiError> {
    mutate_goal(&state, &session_id, increment_goal_turn)
}

pub(crate) async fn record_goal_blocked_endpoint(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Json(request): Json<GoalBlockedAttemptRequest>,
) -> Result<Json<GoalMutationResponse>, ApiError> {
    mutate_goal(&state, &session_id, |goal| {
        record_blocked_attempt(goal, request.reason)
    })
}

pub(crate) async fn mark_goal_usage_limited_endpoint(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<GoalMutationResponse>, ApiError> {
    mutate_goal(&state, &session_id, mark_usage_limited)
}

pub(crate) async fn continue_goal_from_max_turns_endpoint(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<GoalMutationResponse>, ApiError> {
    mutate_goal(&state, &session_id, continue_goal_from_max_turns)
}

pub(crate) async fn clear_goal_endpoint(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<GoalClearResponse>, ApiError> {
    let removed = state.store.delete_goal_state(&session_id)?;
    Ok(Json(GoalClearResponse {
        session_id,
        removed,
        policy: goal_runtime_policy(),
    }))
}

fn mutate_goal<F>(
    state: &ApiState,
    session_id: &str,
    mutate: F,
) -> Result<Json<GoalMutationResponse>, ApiError>
where
    F: FnOnce(GoalState) -> Result<GoalState, ApiError>,
{
    let goal = read_goal(state, session_id)?
        .ok_or_else(|| ApiError::not_found(format!("goal '{session_id}' was not found")))?;
    persist_goal(state, mutate(goal)?)
}

fn read_goal(state: &ApiState, session_id: &str) -> Result<Option<GoalState>, ApiError> {
    state
        .store
        .read_goal_state_json(session_id)?
        .map(goal_state_from_value)
        .transpose()
}

fn persist_goal(state: &ApiState, goal: GoalState) -> Result<Json<GoalMutationResponse>, ApiError> {
    let value =
        serde_json::to_value(&goal).map_err(|error| ApiError::internal(error.to_string()))?;
    let state_ref = state
        .store
        .write_goal_state_json(&goal.session_id, &value)?;
    Ok(Json(GoalMutationResponse {
        goal,
        state_ref,
        policy: goal_runtime_policy(),
    }))
}

fn goal_state_from_value(value: Value) -> Result<GoalState, ApiError> {
    serde_json::from_value(value).map_err(|error| ApiError::internal(error.to_string()))
}

fn new_goal_state(session_id: String, request: GoalCreateRequest) -> Result<GoalState, ApiError> {
    let objective = request.objective.trim();
    if objective.is_empty() {
        return Err(ApiError::bad_request("goal objective must not be empty"));
    }
    let now = now_ms();
    Ok(GoalState {
        session_id,
        objective: objective.to_owned(),
        status: GoalStatus::Active,
        token_budget: request.token_budget.filter(|budget| *budget > 0),
        tokens_used: 0,
        created_at_ms: now,
        updated_at_ms: now,
        active_started_at_ms: now,
        paused_at_ms: None,
        accumulated_active_ms: 0,
        blocked_attempts: 0,
        last_block_reason: None,
        turns_executed: 0,
    })
}

fn pause_goal(mut goal: GoalState) -> Result<GoalState, ApiError> {
    if goal.status != GoalStatus::Active || goal.paused_at_ms.is_some() {
        return Err(ApiError::bad_request("only active goals can be paused"));
    }
    let now = now_ms();
    goal.accumulated_active_ms = goal
        .accumulated_active_ms
        .saturating_add(now.saturating_sub(goal.active_started_at_ms));
    goal.status = GoalStatus::Paused;
    goal.paused_at_ms = Some(now);
    goal.updated_at_ms = now;
    Ok(goal)
}

fn resume_goal(mut goal: GoalState) -> Result<GoalState, ApiError> {
    if goal.status != GoalStatus::Paused {
        return Err(ApiError::bad_request("only paused goals can be resumed"));
    }
    let now = now_ms();
    goal.status = GoalStatus::Active;
    goal.active_started_at_ms = now;
    goal.paused_at_ms = None;
    goal.blocked_attempts = 0;
    goal.last_block_reason = None;
    goal.updated_at_ms = now;
    Ok(goal)
}

fn complete_goal(mut goal: GoalState) -> Result<GoalState, ApiError> {
    let now = now_ms();
    if goal.status == GoalStatus::Active && goal.paused_at_ms.is_none() {
        goal.accumulated_active_ms = goal
            .accumulated_active_ms
            .saturating_add(now.saturating_sub(goal.active_started_at_ms));
    }
    goal.status = GoalStatus::Complete;
    goal.updated_at_ms = now;
    Ok(goal)
}

fn update_goal_tokens(mut goal: GoalState, delta: u64) -> Result<GoalState, ApiError> {
    if goal.status != GoalStatus::Active {
        return Err(ApiError::bad_request(
            "only active goals can record token usage",
        ));
    }
    if delta == 0 {
        return Err(ApiError::bad_request(
            "token delta must be greater than zero",
        ));
    }
    goal.tokens_used = goal.tokens_used.saturating_add(delta);
    if let Some(token_budget) = goal.token_budget {
        if goal.tokens_used >= token_budget {
            goal.status = GoalStatus::BudgetLimited;
        }
    }
    goal.updated_at_ms = now_ms();
    Ok(goal)
}

fn increment_goal_turn(mut goal: GoalState) -> Result<GoalState, ApiError> {
    goal.turns_executed = goal.turns_executed.saturating_add(1);
    if goal.status == GoalStatus::Active && goal.turns_executed >= MAX_GOAL_TURNS {
        goal.status = GoalStatus::MaxTurns;
    }
    goal.updated_at_ms = now_ms();
    Ok(goal)
}

fn record_blocked_attempt(mut goal: GoalState, reason: String) -> Result<GoalState, ApiError> {
    if goal.status != GoalStatus::Active {
        return Err(ApiError::bad_request(
            "only active goals can record blocked attempts",
        ));
    }
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(ApiError::bad_request("blocked reason must not be empty"));
    }
    let normalized = normalize_block_reason(reason);
    let previous = goal
        .last_block_reason
        .as_deref()
        .map(normalize_block_reason);
    if previous.as_deref() != Some(normalized.as_str()) {
        goal.blocked_attempts = 0;
    }
    goal.last_block_reason = Some(reason.to_owned());
    goal.blocked_attempts = goal.blocked_attempts.saturating_add(1);
    if goal.blocked_attempts >= BLOCKED_CONSECUTIVE_THRESHOLD {
        goal.status = GoalStatus::Blocked;
    }
    goal.updated_at_ms = now_ms();
    Ok(goal)
}

fn mark_usage_limited(mut goal: GoalState) -> Result<GoalState, ApiError> {
    if goal.status != GoalStatus::Active {
        return Err(ApiError::bad_request(
            "only active goals can be usage limited",
        ));
    }
    goal.status = GoalStatus::UsageLimited;
    goal.updated_at_ms = now_ms();
    Ok(goal)
}

fn continue_goal_from_max_turns(mut goal: GoalState) -> Result<GoalState, ApiError> {
    if goal.status != GoalStatus::MaxTurns {
        return Err(ApiError::bad_request(
            "only max_turns goals can continue from max turns",
        ));
    }
    let now = now_ms();
    goal.status = GoalStatus::Active;
    goal.turns_executed = 0;
    goal.active_started_at_ms = now;
    goal.paused_at_ms = None;
    goal.blocked_attempts = 0;
    goal.last_block_reason = None;
    goal.updated_at_ms = now;
    Ok(goal)
}

fn normalize_block_reason(reason: &str) -> String {
    reason.trim().to_lowercase()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_goal() -> GoalState {
        new_goal_state(
            "session-1".to_owned(),
            GoalCreateRequest {
                objective: "Ship the runtime".to_owned(),
                token_budget: Some(10),
            },
        )
        .unwrap()
    }

    #[test]
    fn blocked_attempts_require_three_matching_reasons() {
        let goal = record_blocked_attempt(active_goal(), "Need provider".to_owned()).unwrap();
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.blocked_attempts, 1);

        let goal = record_blocked_attempt(goal, "Need approval".to_owned()).unwrap();
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.blocked_attempts, 1);

        let goal = record_blocked_attempt(goal, " need approval ".to_owned()).unwrap();
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.blocked_attempts, 2);

        let goal = record_blocked_attempt(goal, "Need Approval".to_owned()).unwrap();
        assert_eq!(goal.status, GoalStatus::Blocked);
        assert_eq!(goal.blocked_attempts, BLOCKED_CONSECUTIVE_THRESHOLD);
    }

    #[test]
    fn token_budget_and_turn_limit_follow_claude_bounds() {
        let goal = update_goal_tokens(active_goal(), 4).unwrap();
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.tokens_used, 4);

        let goal = update_goal_tokens(goal, 6).unwrap();
        assert_eq!(goal.status, GoalStatus::BudgetLimited);
        assert_eq!(goal.tokens_used, 10);

        let mut goal = active_goal();
        for _ in 0..MAX_GOAL_TURNS {
            goal = increment_goal_turn(goal).unwrap();
        }
        assert_eq!(goal.status, GoalStatus::MaxTurns);
        assert_eq!(goal.turns_executed, MAX_GOAL_TURNS);

        let goal = continue_goal_from_max_turns(goal).unwrap();
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.turns_executed, 0);
    }

    #[test]
    fn pause_resume_and_complete_preserve_active_elapsed() {
        let mut goal = active_goal();
        goal.active_started_at_ms = goal.active_started_at_ms.saturating_sub(10);
        let paused = pause_goal(goal).unwrap();
        assert_eq!(paused.status, GoalStatus::Paused);
        assert!(paused.accumulated_active_ms >= 10);

        let resumed = resume_goal(paused).unwrap();
        assert_eq!(resumed.status, GoalStatus::Active);
        assert_eq!(resumed.blocked_attempts, 0);

        let completed = complete_goal(resumed).unwrap();
        assert_eq!(completed.status, GoalStatus::Complete);
    }
}
