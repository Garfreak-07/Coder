use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use coder_core::RunId;
use coder_store::{DurableJsonlPageOptions, RunStore, StoreError};

use crate::model_tool_async_attachments::drain_idle_queue_async_rewake_notification_attachments;
use crate::model_tool_command_hooks::ASYNC_REWAKE_NOTIFICATION_EVENT_KIND;
use crate::run_transcript_compaction::validated_jsonl_page_limit;
use crate::timeline_projection::project_timeline_items;
use crate::{
    ApiError, ApiState, RepoEvidenceResponse, RunArtifactResponse,
    RunAsyncNotificationDrainResponse, RunAsyncNotificationsResponse, RunCheckpointListResponse,
    RunCheckpointResponse, RunCheckpointWriteResponse, RunDetailQuery, RunDetailResponse,
    RunEventsQuery, RunEventsResponse, RunListResponse, RunRepoEvidenceResponse,
    RunTimelineResponse,
};

const RUN_ASYNC_NOTIFICATIONS_CONTRACT: &str = "coder.run_async_notifications.v1";
const RUN_ASYNC_NOTIFICATION_DRAIN_CONTRACT: &str = "coder.run_async_notification_drain.v1";

pub(crate) async fn list_run_events(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    Query(query): Query<RunEventsQuery>,
) -> Result<Json<RunEventsResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    if query.after_sequence.is_none() && query.limit.is_none() && !query.tail {
        let events = state.store.read_events(&run_id)?;
        return Ok(Json(RunEventsResponse {
            run_id: run_id.to_string(),
            event_count: events.len(),
            returned_count: events.len(),
            truncated: false,
            next_after_sequence: events.last().map(|event| event.sequence),
            events,
        }));
    }
    let limit = validated_jsonl_page_limit(query.limit.unwrap_or(200))?;
    let options = if query.tail {
        DurableJsonlPageOptions::tail(limit)?
    } else {
        DurableJsonlPageOptions::with_after_sequence(query.after_sequence, limit)?
    };
    let page = state.store.read_events_page(&run_id, options)?;
    Ok(Json(RunEventsResponse {
        run_id: run_id.to_string(),
        event_count: page.total_records,
        returned_count: page.returned_records,
        truncated: page.truncated,
        next_after_sequence: page.next_after_sequence,
        events: page.records,
    }))
}

pub(crate) async fn list_run_async_notifications(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    Query(query): Query<RunEventsQuery>,
) -> Result<Json<RunAsyncNotificationsResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let limit = validated_jsonl_page_limit(query.limit.unwrap_or(200))?;
    let events = state.store.read_events(&run_id)?;
    if events.is_empty() && !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }

    let event_count = events.len();
    let mut notifications = events
        .into_iter()
        .filter(|event| event.kind == ASYNC_REWAKE_NOTIFICATION_EVENT_KIND)
        .collect::<Vec<_>>();
    let notification_count = notifications.len();
    if let Some(after_sequence) = query.after_sequence {
        notifications.retain(|event| event.sequence > after_sequence);
    }

    let matching_count = notifications.len();
    let notifications = if query.tail && notifications.len() > limit {
        notifications.split_off(notifications.len() - limit)
    } else if notifications.len() > limit {
        notifications.truncate(limit);
        notifications
    } else {
        notifications
    };
    let returned_count = notifications.len();
    let next_after_sequence = notifications.last().map(|event| event.sequence);

    Ok(Json(RunAsyncNotificationsResponse {
        contract: RUN_ASYNC_NOTIFICATIONS_CONTRACT,
        source: "coder-server",
        policy: if query.tail {
            "tail_page"
        } else {
            "incremental_page"
        },
        run_id: run_id.to_string(),
        notifications_url: format!("/api/v3/runs/{}/async-notifications", run_id.as_str()),
        notifications,
        event_count,
        notification_count,
        returned_count,
        truncated: matching_count > returned_count,
        next_after_sequence,
        delivery_status: "durable_read_available",
    }))
}

pub(crate) async fn drain_run_async_notifications(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunAsyncNotificationDrainResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let events_before = state.store.read_events(&run_id)?;
    if events_before.is_empty() && !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }

    let notification_count = events_before
        .iter()
        .filter(|event| event.kind == ASYNC_REWAKE_NOTIFICATION_EVENT_KIND)
        .count();
    let attachments = drain_idle_queue_async_rewake_notification_attachments(&state.store, &run_id);
    let returned_count = attachments.len();
    let next_after_sequence = attachments
        .last()
        .and_then(|attachment| attachment["notification_sequence"].as_u64());
    let event_count = state.store.event_count(&run_id)?;

    Ok(Json(RunAsyncNotificationDrainResponse {
        contract: RUN_ASYNC_NOTIFICATION_DRAIN_CONTRACT,
        source: "coder-server",
        policy: "main_thread_idle_queue_task_notification_batch",
        run_id: run_id.to_string(),
        delivery_channel: "idle_queue_processor",
        mode: "task-notification",
        processed: returned_count > 0,
        delivery_status: if returned_count > 0 {
            "delivered"
        } else {
            "no_main_thread_notifications"
        },
        attachments,
        event_count,
        notification_count,
        returned_count,
        next_after_sequence,
    }))
}

pub(crate) async fn list_run_timeline(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    Query(query): Query<RunEventsQuery>,
) -> Result<Json<RunTimelineResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let report = state.store.read_report(&run_id)?;
    if query.after_sequence.is_none() && query.limit.is_none() && !query.tail {
        let events = state.store.read_events(&run_id)?;
        if events.is_empty() && report.is_none() && !stored_run_exists(&state.store, &run_id)? {
            return Err(ApiError::not_found(format!(
                "run '{}' was not found",
                run_id.as_str()
            )));
        }
        let items = project_timeline_items(&run_id, &events, report.as_ref());
        let returned_count = events.len();
        return Ok(Json(RunTimelineResponse {
            run_id: run_id.to_string(),
            items,
            event_count: returned_count,
            returned_count,
            truncated: false,
            next_after_sequence: events.last().map(|event| event.sequence),
        }));
    }

    let limit = validated_jsonl_page_limit(query.limit.unwrap_or(200))?;
    let options = if query.tail {
        DurableJsonlPageOptions::tail(limit)?
    } else {
        DurableJsonlPageOptions::with_after_sequence(query.after_sequence, limit)?
    };
    let page = state.store.read_events_page(&run_id, options)?;
    if page.total_records == 0 && report.is_none() && !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    let items = project_timeline_items(&run_id, &page.records, report.as_ref());
    Ok(Json(RunTimelineResponse {
        run_id: run_id.to_string(),
        items,
        event_count: page.total_records,
        returned_count: page.returned_records,
        truncated: page.truncated,
        next_after_sequence: page.next_after_sequence,
    }))
}

pub(crate) async fn list_runs(
    State(state): State<ApiState>,
) -> Result<Json<RunListResponse>, ApiError> {
    Ok(Json(RunListResponse {
        runs: state.store.list_run_summaries()?,
    }))
}

pub(crate) async fn get_run_detail(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    Query(query): Query<RunDetailQuery>,
) -> Result<Json<RunDetailResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let metadata = state.store.read_metadata(&run_id)?;
    let event_count = state.store.event_count(&run_id)?;
    let events = if query.include_events {
        state.store.read_events(&run_id)?
    } else {
        Vec::new()
    };
    let report = state.store.read_report(&run_id)?;
    let repo_evidence_count = state.store.repo_evidence_count(&run_id)?;
    if metadata.is_none() && event_count == 0 && report.is_none() && repo_evidence_count == 0 {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    let returned_count = events.len();
    Ok(Json(RunDetailResponse {
        run_id: run_id.to_string(),
        metadata,
        events,
        event_count,
        returned_count,
        report,
        repo_evidence_count,
    }))
}

pub(crate) async fn get_repo_evidence(
    State(state): State<ApiState>,
    Path(ref_id): Path<String>,
) -> Result<Json<RepoEvidenceResponse>, ApiError> {
    let payload = state.store.read_repo_evidence(&ref_id)?;
    Ok(Json(RepoEvidenceResponse { ref_id, payload }))
}

pub(crate) async fn list_run_repo_evidence(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunRepoEvidenceResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let evidence = state.store.list_repo_evidence(&run_id)?;
    Ok(Json(RunRepoEvidenceResponse {
        run_id: run_id.to_string(),
        evidence,
    }))
}

pub(crate) async fn get_run_artifact(
    State(state): State<ApiState>,
    Path((run_id, artifact_name)): Path<(String, String)>,
) -> Result<Json<RunArtifactResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let payload = state.store.read_artifact_json(&run_id, &artifact_name)?;
    Ok(Json(RunArtifactResponse {
        run_id: run_id.to_string(),
        artifact_name,
        payload,
    }))
}

pub(crate) async fn list_run_checkpoints(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunCheckpointListResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let checkpoints = state.store.list_checkpoints(&run_id)?;
    if checkpoints.is_empty() && !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    Ok(Json(RunCheckpointListResponse {
        run_id: run_id.to_string(),
        checkpoints,
    }))
}

pub(crate) async fn get_run_checkpoint(
    State(state): State<ApiState>,
    Path((run_id, checkpoint_name)): Path<(String, String)>,
) -> Result<Json<RunCheckpointResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let payload = state
        .store
        .read_checkpoint_json(&run_id, &checkpoint_name)?;
    Ok(Json(RunCheckpointResponse {
        run_id: run_id.to_string(),
        checkpoint_name,
        payload,
    }))
}

pub(crate) async fn write_run_checkpoint(
    State(state): State<ApiState>,
    Path((run_id, checkpoint_name)): Path<(String, String)>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<RunCheckpointWriteResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    if !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    let checkpoint_ref = state
        .store
        .write_checkpoint(&run_id, &checkpoint_name, &payload)?;
    Ok(Json(RunCheckpointWriteResponse {
        run_id: run_id.to_string(),
        checkpoint_name,
        checkpoint_ref,
    }))
}

pub(crate) async fn get_blob_sha256(
    State(state): State<ApiState>,
    Path(digest): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let content = state.store.read_blob_sha256(&digest)?;
    Ok((
        StatusCode::OK,
        [("content-type", "application/octet-stream")],
        content,
    ))
}

pub(crate) fn stored_run_exists(store: &RunStore, run_id: &RunId) -> Result<bool, StoreError> {
    Ok(store.read_metadata(run_id)?.is_some()
        || store.event_count(run_id)? > 0
        || store.read_report(run_id)?.is_some()
        || store.repo_evidence_count(run_id)? > 0
        || !store.list_checkpoints(run_id)?.is_empty())
}
