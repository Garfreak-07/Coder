use axum::{
    extract::{Path, State},
    Json,
};
use coder_core::{RunId, RunStatus};
use coder_store::DurableJsonlPageOptions;
use coder_workflow::WorkflowRunControl;
use serde_json::json;

use crate::api_types::{RunControlResponse, RunHeartbeatResponse};
use crate::run_transcript_compaction::{
    run_content_replacement_replay_summary, run_content_replacements_response,
};
use crate::{ApiError, ApiState, RUN_RESUME_CONTENT_REPLACEMENT_RECORD_LIMIT};

pub(crate) async fn pause_run(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunControlResponse>, ApiError> {
    control_run(&state, RunId::from_string(run_id), RunControlAction::Pause).map(Json)
}

pub(crate) async fn resume_run(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunControlResponse>, ApiError> {
    control_run(&state, RunId::from_string(run_id), RunControlAction::Resume).map(Json)
}

pub(crate) async fn cancel_run(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunControlResponse>, ApiError> {
    request_run_cancel(&state, &RunId::from_string(run_id)).map(Json)
}

pub(crate) async fn run_heartbeat(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunHeartbeatResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let metadata = state.store.read_metadata(&run_id)?;
    let event_count = state.store.event_count(&run_id)?;
    let repo_evidence_count = state.store.repo_evidence_count(&run_id)?;
    let has_report = state.store.read_report(&run_id)?.is_some();
    if metadata.is_none() && event_count == 0 && repo_evidence_count == 0 && !has_report {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    Ok(Json(RunHeartbeatResponse {
        run_id: run_id.to_string(),
        status: metadata.as_ref().map(|state| state.status),
        event_count,
        has_report,
        repo_evidence_count,
    }))
}

#[derive(Debug, Clone, Copy)]
enum RunControlAction {
    Pause,
    Resume,
    Cancel,
}

fn control_run(
    state: &ApiState,
    run_id: RunId,
    action: RunControlAction,
) -> Result<RunControlResponse, ApiError> {
    let active_control = match action {
        RunControlAction::Pause => WorkflowRunControl::Paused,
        RunControlAction::Resume => WorkflowRunControl::Running,
        RunControlAction::Cancel => WorkflowRunControl::Cancelled,
    };
    let active_run = signal_active_run_control(state, &run_id, active_control);
    let metadata = state.store.read_metadata(&run_id)?;
    if metadata.is_none() && active_run && matches!(action, RunControlAction::Cancel) {
        return Ok(RunControlResponse {
            run_id: run_id.to_string(),
            status: RunStatus::Cancelled,
            control_state: "cancelled".to_owned(),
            event_count: 0,
            report_ref: None,
            content_replacement_replay: None,
        });
    }
    let mut metadata = metadata
        .ok_or_else(|| ApiError::not_found(format!("run '{}' was not found", run_id.as_str())))?;
    if !active_run && matches!(action, RunControlAction::Pause | RunControlAction::Resume) {
        return Err(ApiError::conflict(format!(
            "run '{}' is not active in this Coder process",
            run_id.as_str()
        )));
    }
    if !active_run
        && matches!(action, RunControlAction::Cancel)
        && !matches!(metadata.status, RunStatus::Queued | RunStatus::Running)
    {
        return Err(ApiError::conflict(format!(
            "run '{}' is already terminal ({:?})",
            run_id.as_str(),
            metadata.status
        )));
    }
    let existing_event_count = state.store.event_count(&run_id)?;
    let (kind, status_text) = match action {
        RunControlAction::Pause => ("run.paused", "paused"),
        RunControlAction::Resume => {
            metadata.status = RunStatus::Running;
            ("run.resumed", "running")
        }
        RunControlAction::Cancel => {
            metadata.status = RunStatus::Cancelled;
            (
                if active_run {
                    "run.cancel_requested"
                } else {
                    "run.cancelled"
                },
                "cancelled",
            )
        }
    };
    let sequence = existing_event_count as u64 + 1;
    let event_count = existing_event_count + 1;
    let content_replacement_replay = if matches!(action, RunControlAction::Resume) {
        Some(run_content_replacements_response(
            &state.store,
            &run_id,
            DurableJsonlPageOptions::tail(RUN_RESUME_CONTENT_REPLACEMENT_RECORD_LIMIT)?,
            "resume_tail_replay",
        )?)
    } else {
        None
    };
    let mut event_payload = json!({
        "status": status_text,
    });
    if let Some(replay) = &content_replacement_replay {
        event_payload["content_replacement_replay"] =
            run_content_replacement_replay_summary(replay);
    }
    let event = coder_events::CoderEvent::new(run_id.clone(), sequence, kind, event_payload);
    metadata.updated_at = event.timestamp;
    state.store.write_metadata(&metadata)?;
    state.store.append_event(&run_id, &event)?;
    let report_ref = if matches!(action, RunControlAction::Cancel) && !active_run {
        let report = state.store.build_evidence_report(&run_id)?;
        Some(state.store.write_report(&run_id, &report)?)
    } else {
        None
    };
    Ok(RunControlResponse {
        run_id: run_id.to_string(),
        status: metadata.status,
        control_state: status_text.to_owned(),
        event_count,
        report_ref,
        content_replacement_replay,
    })
}

pub(crate) fn request_run_cancel(
    state: &ApiState,
    run_id: &RunId,
) -> Result<RunControlResponse, ApiError> {
    control_run(state, run_id.clone(), RunControlAction::Cancel)
}

pub(crate) fn signal_active_run_control(
    state: &ApiState,
    run_id: &RunId,
    control: WorkflowRunControl,
) -> bool {
    state.session_host.signal_task(run_id, control)
}
