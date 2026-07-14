use std::{
    env,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Path, State},
    Json,
};
use coder_config::{resolve_task_tools, validate_project_config, HarnessSpec, ProjectConfig};
use coder_core::{FinalReport, RunId};
use coder_harness::HarnessRunEvent;
use coder_store::{DurableJsonlPageOptions, SubagentBackgroundTaskRecord};
use coder_workflow::{BackendRegistry, SubagentInvocationKind, SubagentRunInput, SubagentRuntime};
use serde_json::{json, Value};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::{
    ensure_tool_boundary, validation_issue_summary, ApiError, ApiState,
    SubagentBackgroundCancelResponse, SubagentBackgroundStartResponse,
    SubagentBackgroundStatusResponse, SubagentRunToolRequest, SubagentRunToolResponse,
};

pub(crate) const SUBAGENT_RESPONSE_EVENT_PREVIEW_LIMIT: usize = 1000;
// Codex MultiAgentV2 defaults to four total session threads: one root plus
// three child agents. Coder's registry tracks only the children.
const MAX_CONCURRENT_BACKGROUND_SUBAGENTS: usize = 3;

#[derive(Debug)]
pub(crate) struct BackgroundSubagentTask {
    task_id: String,
    run_id: String,
    agent_id: String,
    status: String,
    created_at_ms: u64,
    updated_at_ms: u64,
    metadata_ref: String,
    transcript_ref: String,
    report: Option<coder_core::FinalReport>,
    events: Vec<HarnessRunEvent>,
    event_count: usize,
    events_truncated: bool,
    error: Option<String>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Debug, Default)]
struct RecoveredSubagentPreview {
    events: Vec<HarnessRunEvent>,
    report: Option<FinalReport>,
    event_count: usize,
    events_truncated: bool,
}

impl BackgroundSubagentTask {
    fn to_record(&self) -> SubagentBackgroundTaskRecord {
        SubagentBackgroundTaskRecord {
            task_id: self.task_id.clone(),
            run_id: self.run_id.clone(),
            agent_id: self.agent_id.clone(),
            status: self.status.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            metadata_ref: self.metadata_ref.clone(),
            transcript_ref: self.transcript_ref.clone(),
            report: self.report.clone(),
            event_count: self.event_count,
            events_truncated: self.events_truncated,
            error: self.error.clone(),
        }
    }

    fn status_response(&self) -> SubagentBackgroundStatusResponse {
        SubagentBackgroundStatusResponse {
            task_id: self.task_id.clone(),
            status: self.status.clone(),
            run_id: self.run_id.clone(),
            agent_id: self.agent_id.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            metadata_ref: self.metadata_ref.clone(),
            transcript_ref: self.transcript_ref.clone(),
            report: self.report.clone(),
            event_count: self.event_count,
            event_preview_limit: SUBAGENT_RESPONSE_EVENT_PREVIEW_LIMIT,
            events_truncated: self.events_truncated,
            events: self.events.clone(),
            error: self.error.clone(),
        }
    }
}

pub(crate) async fn run_subagent_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<SubagentRunToolRequest>,
) -> Result<Json<SubagentRunToolResponse>, ApiError> {
    ensure_tool_boundary("agent_subagent")?;
    if request.run_in_background.unwrap_or(false) {
        let background_task = start_background_subagent_task(&state, request)?;
        return Ok(Json(backgrounded_subagent_response(background_task)));
    }
    Ok(Json(run_subagent_request(&state, request).await?))
}

pub(crate) async fn get_background_subagent_endpoint(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
) -> Result<Json<SubagentBackgroundStatusResponse>, ApiError> {
    ensure_tool_boundary("read_subagent_status")?;
    Ok(Json(background_subagent_status(&state, &task_id)?))
}

pub(crate) async fn cancel_background_subagent_endpoint(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
) -> Result<Json<SubagentBackgroundCancelResponse>, ApiError> {
    ensure_tool_boundary("cancel_subagent_background")?;
    let Some(task) = find_background_subagent_task(&state, &task_id) else {
        return Ok(Json(cancel_durable_background_subagent_task(
            &state, &task_id,
        )?));
    };
    let mut task = task.lock().unwrap();
    let run_id = RunId::from_string(task.run_id.clone());
    let mut cancelled = false;
    match task.status.as_str() {
        "running" => {
            task.status = "cancelled".to_owned();
            task.updated_at_ms = unix_time_millis();
            cancelled = true;
            let runtime = SubagentRuntime::new(state.store.clone());
            if let Err(error) = runtime.record_cancelled(
                &run_id,
                &task.agent_id,
                "background subagent task cancelled",
            ) {
                task.error = Some(error.to_string());
            }
            if let Some(handle) = task.handle.take() {
                handle.abort();
            }
            persist_background_subagent_task(&state, &task)?;
        }
        "cancelled" => {
            cancelled = true;
        }
        _ => {}
    }
    let status = task.status.clone();
    let terminal = subagent_status_is_terminal(&status);
    drop(task);
    if terminal {
        state.background_subagents.lock().unwrap().remove(&task_id);
        crate::run_token_budget::clear_run_token_budget_if_inactive(&state, &run_id);
    }
    Ok(Json(SubagentBackgroundCancelResponse {
        task_id,
        cancelled,
        status,
    }))
}

pub(crate) fn background_subagent_status(
    state: &ApiState,
    task_id: &str,
) -> Result<SubagentBackgroundStatusResponse, ApiError> {
    if let Some(task) = find_background_subagent_task(state, task_id) {
        let response = task.lock().unwrap().status_response();
        return Ok(response);
    }
    recovered_background_subagent_status(state, task_id)
}

fn start_background_subagent_task(
    state: &ApiState,
    mut request: SubagentRunToolRequest,
) -> Result<SubagentBackgroundStartResponse, ApiError> {
    if subagent_background_tasks_disabled() {
        return Err(ApiError::bad_request(
            "background subagent tasks are disabled by CODER_DISABLE_BACKGROUND_TASKS or CLAUDE_CODE_DISABLE_BACKGROUND_TASKS",
        ));
    }
    let task_id = Uuid::new_v4().to_string();
    let run_id = request
        .run_id
        .clone()
        .unwrap_or_else(|| RunId::new().as_str().to_owned());
    request.run_id = Some(run_id.clone());
    let agent_id = request.agent_id.clone().unwrap_or_else(|| {
        let short = task_id.split('-').next().unwrap_or(task_id.as_str());
        format!("bg-{short}")
    });
    request.agent_id = Some(agent_id.clone());
    request.run_in_background = Some(false);

    let metadata_ref = format!(
        "subagent://runs/{}/subagents/agent-{agent_id}.meta.json",
        run_id
    );
    let transcript_ref = format!(
        "subagent://runs/{}/subagents/agent-{agent_id}.jsonl",
        run_id
    );
    let now = unix_time_millis();
    let task = Arc::new(Mutex::new(BackgroundSubagentTask {
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        agent_id: agent_id.clone(),
        status: "running".to_owned(),
        created_at_ms: now,
        updated_at_ms: now,
        metadata_ref: metadata_ref.clone(),
        transcript_ref: transcript_ref.clone(),
        report: None,
        events: Vec::new(),
        event_count: 0,
        events_truncated: false,
        error: None,
        handle: None,
    }));
    {
        let mut tasks = state.background_subagents.lock().unwrap();
        if tasks.len() >= MAX_CONCURRENT_BACKGROUND_SUBAGENTS {
            return Err(ApiError::bad_request(format!(
                "background subagent limit reached ({MAX_CONCURRENT_BACKGROUND_SUBAGENTS})"
            )));
        }
        tasks.insert(task_id.clone(), task.clone());
    }
    if let Err(error) = persist_background_subagent_task(state, &task.lock().unwrap()) {
        state.background_subagents.lock().unwrap().remove(&task_id);
        return Err(error);
    }

    let state_for_worker = state.clone();
    let task_for_worker = task.clone();
    let task_id_for_worker = task_id.clone();
    let run_id_for_worker = run_id.clone();
    let handle = tokio::spawn(async move {
        let response = run_subagent_request(&state_for_worker, request).await;
        let mut task = task_for_worker.lock().unwrap();
        if task.status == "cancelled" {
            let run_id = RunId::from_string(task.run_id.clone());
            drop(task);
            state_for_worker
                .background_subagents
                .lock()
                .unwrap()
                .remove(&task_id_for_worker);
            crate::run_token_budget::clear_run_token_budget_if_inactive(&state_for_worker, &run_id);
            return;
        }
        task.updated_at_ms = unix_time_millis();
        match response {
            Ok(response) => {
                task.status = response.status;
                task.report = response.report;
                task.event_count = response.event_count;
                task.events_truncated = response.events_truncated;
                task.events = response.events;
                task.error = None;
            }
            Err(error) => {
                task.status = "failed".to_owned();
                task.error = Some(format!("{error:?}"));
            }
        }
        let _ = persist_background_subagent_task(&state_for_worker, &task);
        drop(task);
        state_for_worker
            .background_subagents
            .lock()
            .unwrap()
            .remove(&task_id_for_worker);
        crate::run_token_budget::clear_run_token_budget_if_inactive(
            &state_for_worker,
            &RunId::from_string(run_id_for_worker),
        );
    });
    task.lock().unwrap().handle = Some(handle);

    Ok(SubagentBackgroundStartResponse {
        task_id: task_id.clone(),
        status: "running".to_owned(),
        run_id,
        agent_id,
        status_url: format!("/api/v3/tools/subagent/background/{task_id}"),
        cancel_url: format!("/api/v3/tools/subagent/background/{task_id}"),
        metadata_ref,
        transcript_ref,
    })
}

pub(crate) fn has_background_subagents_for_run(state: &ApiState, run_id: &RunId) -> bool {
    state
        .background_subagents
        .lock()
        .map(|tasks| {
            tasks.values().any(|task| {
                task.lock()
                    .map(|task| task.run_id == run_id.as_str() && task.status == "running")
                    .unwrap_or(true)
            })
        })
        .unwrap_or(true)
}

async fn run_subagent_request(
    state: &ApiState,
    request: SubagentRunToolRequest,
) -> Result<SubagentRunToolResponse, ApiError> {
    let validation = validate_project_config(&request.config);
    if !validation.is_pass() {
        return Err(ApiError::bad_request(format!(
            "invalid subagent config: {}",
            validation_issue_summary(&validation)
        )));
    }
    let run_id = request
        .run_id
        .as_deref()
        .map(RunId::from_string)
        .unwrap_or_default();
    let repo_root = request.repo_root.clone().unwrap_or_else(|| ".".to_owned());
    let harness = request
        .config
        .harnesses
        .get(&request.parent_harness_id)
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "parent harness '{}' was not found",
                request.parent_harness_id
            ))
        })?;
    let backend_registry = BackendRegistry::for_host().with_native_backend(Arc::new(
        crate::native_model_backend::NativeModelBackend::new(state.clone()),
    ));
    let backend = backend_registry
        .backend_for(&harness.backend)
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "backend '{}' is not available for subagent harness '{}'",
                harness.backend, request.parent_harness_id
            ))
        })?;
    let invocation_kind = parse_subagent_invocation_kind(request.invocation_kind.as_deref())?;
    let backend_context = project_subagent_backend_context(
        &request.config,
        harness,
        request.subagent_name.as_deref(),
        request.model_override.as_deref(),
        request.effort_override.as_ref(),
        &request.backend_context,
    );
    let runtime = SubagentRuntime::new(state.store.clone());
    let output = runtime
        .run(SubagentRunInput {
            backend,
            run_id: &run_id,
            workflow_id: &request.workflow_id,
            node_id: &request.node_id,
            parent_agent_id: &request.parent_agent_id,
            parent_harness_id: &request.parent_harness_id,
            harness,
            repo_root: &repo_root,
            task: &request.task,
            backend_context: &backend_context,
            agent_id: request.agent_id.clone(),
            subagent_name: request.subagent_name.as_deref(),
            is_built_in: request.is_built_in,
            invoking_request_id: request.invoking_request_id.as_deref(),
            invocation_kind,
            parent_query_depth: request.parent_query_depth,
            parent_sequence: request.parent_sequence,
        })
        .await
        .map_err(|error| ApiError::internal(format!("subagent run failed: {error}")))?;
    let status = output.result.status;
    let report = output.result.report;
    let (events, event_count, events_truncated) =
        subagent_response_event_preview(output.result.events);
    Ok(SubagentRunToolResponse {
        run_id: run_id.as_str().to_owned(),
        agent_id: output.agent_id,
        metadata_ref: output.metadata_ref,
        transcript_ref: output.transcript_ref,
        status,
        report,
        event_count,
        event_preview_limit: SUBAGENT_RESPONSE_EVENT_PREVIEW_LIMIT,
        events_truncated,
        events,
        background_task: None,
    })
}

fn project_subagent_backend_context(
    config: &ProjectConfig,
    harness: &HarnessSpec,
    subagent_name: Option<&str>,
    model_override: Option<&str>,
    effort_override: Option<&Value>,
    parent_context: &Value,
) -> Value {
    let mut context = if parent_context.is_object() {
        parent_context.clone()
    } else {
        json!({})
    };
    let coder = context
        .as_object_mut()
        .expect("subagent context is an object")
        .entry("coder")
        .or_insert_with(|| json!({}));
    if !coder.is_object() {
        *coder = json!({});
    }

    if let Some((profile_id, profile)) =
        subagent_name.and_then(|profile_id| config.task_profiles.get_key_value(profile_id))
    {
        coder["agent"] = json!({
            "agent_type": profile_id,
            "system": &profile.instructions,
            "runtime": &profile.runtime
        });
        let selected_tools = resolve_task_tools(profile, harness).selected_tools;
        if !coder.get("harness").is_some_and(Value::is_object) {
            coder["harness"] = json!({});
        }
        coder["harness"]["selected_tools"] = json!(selected_tools);
        if let Some(model) = config.models.get(&profile.model) {
            coder["model"] = json!({
                "provider": &model.provider,
                "model": &model.model,
                "base_url_env": &model.base_url_env,
                "api_key_env": &model.api_key_env
            });
        }
    }

    if let Some(model) = model_override
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        if !coder.get("model").is_some_and(Value::is_object) {
            coder["model"] = json!({});
        }
        coder["model"]["model"] = json!(model);
    }
    if let Some(effort) = effort_override.filter(|effort| !effort.is_null()) {
        if !coder.get("agent").is_some_and(Value::is_object) {
            coder["agent"] = json!({});
        }
        if !coder["agent"].get("runtime").is_some_and(Value::is_object) {
            coder["agent"]["runtime"] = json!({});
        }
        coder["agent"]["runtime"]["effort"] = effort.clone();
    }
    context
}

pub(crate) fn subagent_response_event_preview(
    mut events: Vec<HarnessRunEvent>,
) -> (Vec<HarnessRunEvent>, usize, bool) {
    let event_count = events.len();
    let events_truncated = event_count > SUBAGENT_RESPONSE_EVENT_PREVIEW_LIMIT;
    if events_truncated {
        events.truncate(SUBAGENT_RESPONSE_EVENT_PREVIEW_LIMIT);
    }
    (events, event_count, events_truncated)
}

fn find_background_subagent_task(
    state: &ApiState,
    task_id: &str,
) -> Option<Arc<Mutex<BackgroundSubagentTask>>> {
    state
        .background_subagents
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
}

fn persist_background_subagent_task(
    state: &ApiState,
    task: &BackgroundSubagentTask,
) -> Result<(), ApiError> {
    state
        .store
        .write_subagent_background_task_record(&task.to_record())
        .map(|_| ())
        .map_err(|error| ApiError::internal(error.to_string()))
}

fn recovered_background_subagent_status(
    state: &ApiState,
    task_id: &str,
) -> Result<SubagentBackgroundStatusResponse, ApiError> {
    let mut record = state
        .store
        .read_subagent_background_task_record(task_id)
        .map_err(|error| ApiError::internal(error.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("background subagent task not found: {task_id}"))
        })?;
    let run_id = RunId::from_string(record.run_id.clone());
    let mut metadata = state
        .store
        .read_subagent_metadata(&run_id, &record.agent_id)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    if recovered_background_subagent_is_lost(&record, metadata.as_ref()) {
        let reason =
            "background subagent task was running, but no live task registry exists after restart";
        let runtime = SubagentRuntime::new(state.store.clone());
        runtime
            .record_lost(&run_id, &record.agent_id, reason)
            .map_err(|error| ApiError::internal(error.to_string()))?;
        record.status = "lost".to_owned();
        record.updated_at_ms = unix_time_millis();
        record.error = Some(reason.to_owned());
        state
            .store
            .write_subagent_background_task_record(&record)
            .map_err(|error| ApiError::internal(error.to_string()))?;
        metadata = state
            .store
            .read_subagent_metadata(&run_id, &record.agent_id)
            .map_err(|error| ApiError::internal(error.to_string()))?;
    }
    let status = metadata
        .as_ref()
        .and_then(|metadata| metadata.status.clone())
        .unwrap_or_else(|| record.status.clone());
    let error = metadata
        .as_ref()
        .and_then(|metadata| metadata.error.clone())
        .or_else(|| record.error.clone());
    let preview = recovered_background_subagent_preview(state, &run_id, &record.agent_id)?;
    let event_count = record.event_count.max(preview.event_count);
    Ok(SubagentBackgroundStatusResponse {
        task_id: record.task_id,
        status,
        run_id: record.run_id,
        agent_id: record.agent_id,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
        metadata_ref: record.metadata_ref,
        transcript_ref: record.transcript_ref,
        report: record.report.or(preview.report),
        event_count,
        event_preview_limit: SUBAGENT_RESPONSE_EVENT_PREVIEW_LIMIT,
        events_truncated: record.events_truncated || preview.events_truncated,
        events: preview.events,
        error,
    })
}

fn recovered_background_subagent_is_lost(
    record: &SubagentBackgroundTaskRecord,
    metadata: Option<&coder_store::SubagentMetadata>,
) -> bool {
    if record.status != "running" {
        return false;
    }
    metadata
        .and_then(|metadata| metadata.status.as_deref())
        .map(|status| !subagent_status_is_terminal(status))
        .unwrap_or(true)
}

fn recovered_background_subagent_preview(
    state: &ApiState,
    run_id: &RunId,
    agent_id: &str,
) -> Result<RecoveredSubagentPreview, ApiError> {
    let page = state
        .store
        .read_subagent_transcript_records_page(
            run_id,
            agent_id,
            DurableJsonlPageOptions::tail(SUBAGENT_RESPONSE_EVENT_PREVIEW_LIMIT)
                .map_err(|error| ApiError::internal(error.to_string()))?,
        )
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let events_truncated = page.truncated;
    let mut preview = RecoveredSubagentPreview {
        events_truncated,
        ..RecoveredSubagentPreview::default()
    };
    for record in page.records {
        match record.kind.as_str() {
            "subagent.event" => {
                if let Some(event) = harness_event_from_subagent_record_payload(&record.payload) {
                    preview.events.push(event);
                }
            }
            "subagent.report" => {
                if let Some(report) = record
                    .payload
                    .get("report")
                    .cloned()
                    .and_then(|value| serde_json::from_value(value).ok())
                {
                    preview.report = Some(report);
                }
            }
            _ => {}
        }
    }
    preview.event_count = preview.events.len();
    Ok(preview)
}

fn harness_event_from_subagent_record_payload(payload: &Value) -> Option<HarnessRunEvent> {
    let kind = payload.get("kind")?.as_str()?.to_owned();
    let event_payload = payload.get("payload").cloned().unwrap_or(Value::Null);
    let refs = payload
        .get("refs")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    Some(HarnessRunEvent {
        kind,
        payload: event_payload,
        refs,
    })
}

fn cancel_durable_background_subagent_task(
    state: &ApiState,
    task_id: &str,
) -> Result<SubagentBackgroundCancelResponse, ApiError> {
    let mut record = state
        .store
        .read_subagent_background_task_record(task_id)
        .map_err(|error| ApiError::internal(error.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("background subagent task not found: {task_id}"))
        })?;
    let run_id = RunId::from_string(record.run_id.clone());
    if let Some(metadata) = state
        .store
        .read_subagent_metadata(&run_id, &record.agent_id)
        .map_err(|error| ApiError::internal(error.to_string()))?
    {
        if let Some(status) = metadata.status.as_deref() {
            if subagent_status_is_terminal(status) {
                record.status = status.to_owned();
                record.updated_at_ms = unix_time_millis();
                record.error = metadata.error;
                state
                    .store
                    .write_subagent_background_task_record(&record)
                    .map_err(|error| ApiError::internal(error.to_string()))?;
                return Ok(SubagentBackgroundCancelResponse {
                    task_id: record.task_id,
                    cancelled: status == "cancelled" || status == "canceled",
                    status: record.status,
                });
            }
        }
    }
    let mut cancelled = false;
    match record.status.as_str() {
        "running" => {
            let runtime = SubagentRuntime::new(state.store.clone());
            runtime
                .record_cancelled(
                    &run_id,
                    &record.agent_id,
                    "background subagent task cancelled after registry recovery",
                )
                .map_err(|error| ApiError::internal(error.to_string()))?;
            record.status = "cancelled".to_owned();
            record.updated_at_ms = unix_time_millis();
            record.error =
                Some("background subagent task cancelled after registry recovery".to_owned());
            state
                .store
                .write_subagent_background_task_record(&record)
                .map_err(|error| ApiError::internal(error.to_string()))?;
            cancelled = true;
        }
        "cancelled" => {
            cancelled = true;
        }
        _ => {}
    }
    Ok(SubagentBackgroundCancelResponse {
        task_id: record.task_id,
        cancelled,
        status: record.status,
    })
}

fn subagent_status_is_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed" | "blocked" | "failed" | "cancelled" | "canceled" | "lost"
    )
}

fn backgrounded_subagent_response(
    background_task: SubagentBackgroundStartResponse,
) -> SubagentRunToolResponse {
    SubagentRunToolResponse {
        run_id: background_task.run_id.clone(),
        agent_id: background_task.agent_id.clone(),
        metadata_ref: background_task.metadata_ref.clone(),
        transcript_ref: background_task.transcript_ref.clone(),
        status: "backgrounded".to_owned(),
        report: None,
        event_count: 0,
        event_preview_limit: SUBAGENT_RESPONSE_EVENT_PREVIEW_LIMIT,
        events_truncated: false,
        events: Vec::new(),
        background_task: Some(background_task),
    }
}

fn parse_subagent_invocation_kind(value: Option<&str>) -> Result<SubagentInvocationKind, ApiError> {
    match value.unwrap_or("spawn") {
        "spawn" => Ok(SubagentInvocationKind::Spawn),
        "resume" => Ok(SubagentInvocationKind::Resume),
        other => Err(ApiError::bad_request(format!(
            "unsupported subagent invocation_kind '{other}'"
        ))),
    }
}

fn subagent_background_tasks_disabled() -> bool {
    env_truthy("CODER_DISABLE_BACKGROUND_TASKS")
        || env_truthy("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS")
}

fn env_truthy(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_subagent_projects_task_profile_runtime_and_typed_overrides() {
        let config = crate::default_project_config();
        let harness = config.harnesses.get("native-code-edit").unwrap();
        let context = project_subagent_backend_context(
            &config,
            harness,
            Some("code"),
            Some("override-model"),
            Some(&json!("max")),
            &json!({"coder": {"task_context": {"marker": "preserved"}}}),
        );

        assert_eq!(context["coder"]["task_context"]["marker"], "preserved");
        assert_eq!(context["coder"]["agent"]["agent_type"], "code");
        assert_eq!(
            context["coder"]["agent"]["system"].as_str(),
            Some(config.task_profiles["code"].instructions.as_str())
        );
        assert_eq!(context["coder"]["agent"]["runtime"]["effort"], "max");
        assert_eq!(context["coder"]["model"]["model"], "override-model");
        assert!(context["coder"]["harness"]["selected_tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()));
        let serialized = context.to_string();
        assert!(!serialized.contains("model_tool_agent_bridge"));
        assert!(!serialized.contains("skill_tool_fork_bridge"));
    }
}
