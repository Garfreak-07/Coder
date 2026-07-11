use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};

use coder_store::{RunStore, StoreError};
use serde_json::{json, Value};

use crate::api_types::{PlannerChatSession, PlannerConversationResponse};
use crate::planner_conversation::numbered_lines;

// Claude Code keeps at most 200 cached session file lookups
// (utils/sessionStorage.ts: MAX_CACHED_SESSION_FILES). Coder applies the same
// cap to live Planner Chat sessions, while keeping fewer in-memory turns
// because each Coder turn can include structured artifacts and plan state.
pub(crate) const PLANNER_SESSION_CACHE_LIMIT: usize = 200;
pub(crate) const PLANNER_SESSION_MAX_TURNS: usize = 64;
const PLANNER_SESSION_IDLE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone)]
pub(crate) struct StoredPlannerChatSession {
    pub(crate) session: PlannerChatSession,
    pub(crate) last_accessed: SystemTime,
    pub(crate) revision: u64,
}

impl StoredPlannerChatSession {
    fn new(session: PlannerChatSession, last_accessed: SystemTime, revision: u64) -> Self {
        Self {
            session,
            last_accessed,
            revision,
        }
    }
}

pub(crate) fn trim_planner_session_turns(session: &mut PlannerChatSession) {
    if session.turns.len() <= PLANNER_SESSION_MAX_TURNS {
        return;
    }
    let trim_count = session.turns.len() - PLANNER_SESSION_MAX_TURNS;
    session.turns.drain(0..trim_count);
}

pub(crate) fn prune_planner_sessions(
    sessions: &mut BTreeMap<String, StoredPlannerChatSession>,
    now: SystemTime,
    keep_session_id: Option<&str>,
) {
    sessions.retain(|session_id, stored| {
        if keep_session_id == Some(session_id.as_str()) {
            return true;
        }
        now.duration_since(stored.last_accessed)
            .map(|age| age <= PLANNER_SESSION_IDLE_TTL)
            .unwrap_or(true)
    });

    while sessions.len() > PLANNER_SESSION_CACHE_LIMIT {
        let Some(session_id) = sessions
            .iter()
            .filter(|(session_id, _)| keep_session_id != Some(session_id.as_str()))
            .min_by_key(|(_, stored)| stored.last_accessed)
            .map(|(session_id, _)| session_id.clone())
        else {
            break;
        };
        sessions.remove(&session_id);
    }
}

pub(crate) fn store_planner_session_snapshot(
    sessions: &mut BTreeMap<String, StoredPlannerChatSession>,
    mut session: PlannerChatSession,
    now: SystemTime,
) -> PlannerChatSession {
    trim_planner_session_turns(&mut session);
    let session_id = session.session_id.clone();
    let revision = sessions
        .get(&session_id)
        .map(|stored| stored.revision.saturating_add(1))
        .unwrap_or(0);
    sessions.insert(
        session_id.clone(),
        StoredPlannerChatSession::new(session.clone(), now, revision),
    );
    prune_planner_sessions(sessions, now, Some(&session_id));
    session
}

pub(crate) fn planner_turn_events(
    session: &PlannerChatSession,
    response: &PlannerConversationResponse,
) -> Vec<Value> {
    let mut events = vec![json!({
        "type": "planner.message.completed",
        "session_id": session.session_id,
        "workflow_id": session.workflow_id,
        "readiness": response.readiness,
        "response_truncated": response.response_truncated,
        "artifact_count": response.artifacts.len()
    })];
    if let Some(plan) = &response.plan_draft {
        events.push(json!({
            "type": "planner.plan.updated",
            "session_id": session.session_id,
            "selected_workflow_id": plan.selected_workflow_id,
            "open_questions": plan.open_questions,
            "acceptance_criteria": plan.acceptance_criteria,
            "risks": plan.risks
        }));
        for proposal in &plan.memory_proposals {
            events.push(json!({
                "type": "planner.memory.proposed",
                "session_id": session.session_id,
                "scope": proposal.scope,
                "key": proposal.key,
                "requires_confirmation": proposal.requires_confirmation
            }));
        }
    }
    if let Some(trace) = &response.provider_trace {
        events.push(json!({
            "type": "planner.provider.completed",
            "session_id": session.session_id,
            "requested_stream": trace.requested_stream,
            "response_transport": trace.response_transport,
            "streaming_fallback": trace.streaming_fallback,
            "fallback_status": trace.fallback_status,
            "finish_reason": trace.finish_reason,
            "provider_turns": trace.provider_turns,
            "estimated_input_tokens": trace.estimated_input_tokens,
            "estimated_output_tokens": trace.estimated_output_tokens,
            "input_tokens": trace.input_tokens,
            "output_tokens": trace.output_tokens,
            "total_tokens": trace.total_tokens,
            "cache_read_tokens": trace.cache_read_tokens,
            "usage_reported": trace.usage_reported
        }));
    }
    events.push(json!({
        "type": "planner.readiness.changed",
        "session_id": session.session_id,
        "readiness": response.readiness
    }));
    events
}

fn planner_session_record_payload(session: &PlannerChatSession) -> Value {
    json!({
        "workflow_id": session.workflow_id,
        "mode": session.mode,
        "ready": session.ready,
        "readiness": session.readiness,
        "turn_count": session.turns.len(),
        "has_plan_draft": session.plan_draft.is_some(),
        "open_question_count": session.open_questions.len(),
        "acceptance_criteria_count": session.acceptance_criteria.len(),
        "risk_count": session.risks.len(),
        "work_in_progress": session.work_in_progress,
        "active_run_id": session.active_run_id,
        "latest_run_id": session.latest_run_id
    })
}

pub(crate) fn append_planner_session_record(
    store: &RunStore,
    session: &PlannerChatSession,
    kind: &str,
    extra_payload: Value,
) -> Result<(), StoreError> {
    let mut payload = planner_session_record_payload(session);
    if let (Value::Object(payload), Value::Object(extra_payload)) = (&mut payload, extra_payload) {
        payload.extend(extra_payload);
    }
    store.append_session_record_next(&session.session_id, kind, payload)?;
    Ok(())
}

pub(crate) fn start_work_clarification(session: &PlannerChatSession) -> String {
    if session.plan_draft.is_none() {
        return "I need to turn this into a concrete plan before starting work.".to_owned();
    }
    if !session.open_questions.is_empty() {
        return format!(
            "I need clarification before starting work:\n{}",
            numbered_lines(&session.open_questions)
        );
    }
    "I am not ready to start work yet. Please confirm the goal, scope, and acceptance criteria."
        .to_owned()
}
