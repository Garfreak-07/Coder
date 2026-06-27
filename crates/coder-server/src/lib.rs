use std::{collections::BTreeSet, fs, net::SocketAddr, path::PathBuf};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use coder_config::{
    validate_project_config, ProjectConfig, ValidationIssue, ValidationLevel, ValidationReport,
};
use coder_core::{FinalReport, RunId, RunState, RunStatus};
use coder_memory::{load_project_memory_file, memory_read_event, MemoryError, ProjectMemoryFile};
use coder_store::{
    RepoEvidenceKind, RepoEvidenceRef, RunCheckpointRef, RunStore, StoreError, StoredRunSummary,
};
use coder_tools::{
    apply_patch_file, preview_command, preview_patch_file, CommandPreview, PatchApplyEvidence,
    PatchApplyRequest as ToolPatchApplyRequest, PatchPreviewEvidence, RepoToolError,
};
use coder_workflow::{MockWorkflowRunner, WorkflowError};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone)]
pub struct ApiState {
    pub store: RunStore,
}

impl ApiState {
    pub fn new(store: RunStore) -> Self {
        Self { store }
    }
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/v3/health", get(health))
        .route("/api/v3/memory/project/load", post(load_project_memory))
        .route("/api/v3/config/validate", post(validate_config))
        .route("/api/v3/workflows/validate", post(validate_workflow))
        .route("/api/v3/runs", get(list_runs))
        .route("/api/v3/runs/preview", post(preview_run))
        .route(
            "/api/v3/tools/command/preview",
            post(preview_command_endpoint),
        )
        .route("/api/v3/tools/patch/preview", post(preview_patch_endpoint))
        .route("/api/v3/tools/patch/apply", post(apply_patch_endpoint))
        .route("/api/v3/runs/mock", post(run_mock_workflow))
        .route("/api/v3/runs/{run_id}", get(get_run_detail))
        .route("/api/v3/runs/{run_id}/events", get(list_run_events))
        .route("/api/v3/runs/{run_id}/pause", post(pause_run))
        .route("/api/v3/runs/{run_id}/resume", post(resume_run))
        .route("/api/v3/runs/{run_id}/cancel", post(cancel_run))
        .route("/api/v3/runs/{run_id}/heartbeat", get(run_heartbeat))
        .route(
            "/api/v3/runs/{run_id}/report/preview",
            get(preview_run_report),
        )
        .route("/api/v3/runs/{run_id}/report", post(write_run_report))
        .route(
            "/api/v3/runs/{run_id}/repo-evidence",
            get(list_run_repo_evidence),
        )
        .route(
            "/api/v3/runs/{run_id}/artifacts/{artifact_name}",
            get(get_run_artifact),
        )
        .route(
            "/api/v3/runs/{run_id}/checkpoints",
            get(list_run_checkpoints),
        )
        .route(
            "/api/v3/runs/{run_id}/checkpoints/{checkpoint_name}",
            get(get_run_checkpoint).post(write_run_checkpoint),
        )
        .route("/api/v3/blobs/sha256/{digest}", get(get_blob_sha256))
        .route("/api/v3/repo-evidence/{ref_id}", get(get_repo_evidence))
        .with_state(state)
}

pub async fn serve(addr: SocketAddr, state: ApiState) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(state)).await
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "coder-server",
        api_version: "v3",
    })
}

async fn load_project_memory(
    State(state): State<ApiState>,
    Json(request): Json<ProjectMemoryLoadRequest>,
) -> Result<Json<ProjectMemoryLoadResponse>, ApiError> {
    let memory_path = resolve_repo_relative_path(&request.repo_root, &request.memory_path)?;
    let memory = load_project_memory_file(&memory_path)?;
    let mut event_recorded = false;
    if let Some(run_id) = request.run_id {
        let run_id = RunId::from_string(run_id);
        if !stored_run_exists(&state.store, &run_id)? {
            return Err(ApiError::not_found(format!(
                "run '{}' was not found",
                run_id.as_str()
            )));
        }
        let sequence = state.store.read_events(&run_id)?.len() as u64 + 1;
        state.store.append_event(
            &run_id,
            &memory_read_event(run_id.clone(), sequence, &memory.records),
        )?;
        event_recorded = true;
    }
    Ok(Json(ProjectMemoryLoadResponse {
        record_count: memory.records.len(),
        event_recorded,
        memory,
    }))
}

async fn validate_config(Json(request): Json<ConfigValidationRequest>) -> Json<ValidationReport> {
    Json(validate_project_config(&request.config))
}

async fn validate_workflow(
    Json(request): Json<WorkflowValidationRequest>,
) -> Result<Json<ValidationReport>, ApiError> {
    let report = validate_project_config(&request.config);
    if !request.config.workflows.contains_key(&request.workflow_id) {
        return Err(ApiError::not_found(format!(
            "workflow '{}' was not found",
            request.workflow_id
        )));
    }
    Ok(Json(report))
}

async fn run_mock_workflow(
    State(state): State<ApiState>,
    Json(request): Json<MockRunRequest>,
) -> Result<Json<MockRunResponse>, ApiError> {
    let runner = MockWorkflowRunner::new(&request.config, state.store);
    let output = runner.run(&request.workflow_id, &request.task)?;
    Ok(Json(MockRunResponse {
        run_id: output.run_id.to_string(),
        report_ref: output.report_ref,
        report: output.report,
        events_url: format!("/api/v3/runs/{}/events", output.run_id.as_str()),
    }))
}

async fn preview_run(Json(request): Json<RunPreviewRequest>) -> Json<RunPreviewResponse> {
    let mut issues = validate_project_config(&request.config).issues;
    let workflow = request.config.workflows.get(&request.workflow_id);
    if workflow.is_none() {
        issues.push(validation_issue(
            ValidationLevel::Error,
            "workflow_not_found",
            format!("workflow '{}' was not found", request.workflow_id),
            "workflow_id",
        ));
    }
    if request.task.trim().is_empty() {
        issues.push(validation_issue(
            ValidationLevel::Error,
            "task_empty",
            "task must not be empty",
            "task",
        ));
    }

    let status = if issues
        .iter()
        .any(|issue| issue.level == ValidationLevel::Error)
    {
        "blocked"
    } else {
        "ready"
    };
    let backends = workflow
        .map(|workflow| {
            workflow
                .nodes
                .iter()
                .filter_map(|node| request.config.harnesses.get(&node.harness))
                .map(|harness| harness.backend.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Json(RunPreviewResponse {
        status,
        requires_confirmation: status == "ready",
        workflow_id: request.workflow_id,
        task: request.task,
        backends,
        issues,
    })
}

async fn preview_command_endpoint(
    Json(request): Json<CommandPreviewRequest>,
) -> Result<Json<CommandPreview>, ApiError> {
    let preview = preview_command(
        &request.repo_root,
        request.cwd.unwrap_or_else(|| ".".to_owned()),
        request.argv,
        request.source.as_deref().unwrap_or("model"),
        request.sandbox.unwrap_or(false),
    )?;
    Ok(Json(preview))
}

async fn preview_patch_endpoint(
    Json(request): Json<PatchPreviewRequest>,
) -> Result<Json<PatchPreviewEvidence>, ApiError> {
    let preview = preview_patch_file(
        &request.repo_root,
        PathBuf::from(&request.patch_file),
        request
            .max_patch_bytes
            .unwrap_or(coder_tools::DEFAULT_MAX_PATCH_BYTES),
    )?;
    Ok(Json(preview))
}

async fn apply_patch_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<PatchApplyToolRequest>,
) -> Result<Json<PatchApplyResponse>, ApiError> {
    let run_id = request
        .run_id
        .as_deref()
        .map(RunId::from_string)
        .ok_or_else(|| ApiError::bad_request("run_id is required for patch apply"))?;
    let result = apply_patch_file(
        &request.repo_root,
        ToolPatchApplyRequest {
            patch_file: PathBuf::from(&request.patch_file),
            max_patch_bytes: request
                .max_patch_bytes
                .unwrap_or(coder_tools::DEFAULT_MAX_PATCH_BYTES),
            source: request.source.unwrap_or_else(|| "model".to_owned()),
            approved: request.approved.unwrap_or(false),
        },
    )?;
    let result_json =
        serde_json::to_value(&result).map_err(|error| ApiError::internal(error.to_string()))?;
    let evidence_ref = state.store.write_repo_evidence(
        &run_id,
        RepoEvidenceKind::RepoDiff,
        result.repo_root.clone(),
        Vec::new(),
        format!(
            "Patch apply {}: {} file(s).",
            result.status, result.preview.file_count
        ),
        json!({
            "evidence_kind": "patch_apply",
            "operation": "patch_apply",
            "result": result_json,
        }),
    )?;
    record_patch_apply_event(&state.store, &run_id, &result, &evidence_ref)?;
    Ok(Json(PatchApplyResponse {
        run_id: run_id.to_string(),
        evidence_ref,
        result,
    }))
}

async fn list_run_events(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunEventsResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let events = state.store.read_events(&run_id)?;
    Ok(Json(RunEventsResponse {
        run_id: run_id.to_string(),
        events,
    }))
}

async fn list_runs(State(state): State<ApiState>) -> Result<Json<RunListResponse>, ApiError> {
    Ok(Json(RunListResponse {
        runs: state.store.list_run_summaries()?,
    }))
}

async fn get_run_detail(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunDetailResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let metadata = state.store.read_metadata(&run_id)?;
    let events = state.store.read_events(&run_id)?;
    let report = state.store.read_report(&run_id)?;
    let repo_evidence_count = state.store.repo_evidence_count(&run_id)?;
    if metadata.is_none() && events.is_empty() && report.is_none() && repo_evidence_count == 0 {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    Ok(Json(RunDetailResponse {
        run_id: run_id.to_string(),
        metadata,
        events,
        report,
        repo_evidence_count,
    }))
}

async fn pause_run(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunControlResponse>, ApiError> {
    control_run(state, RunId::from_string(run_id), RunControlAction::Pause)
}

async fn resume_run(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunControlResponse>, ApiError> {
    control_run(state, RunId::from_string(run_id), RunControlAction::Resume)
}

async fn cancel_run(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunControlResponse>, ApiError> {
    control_run(state, RunId::from_string(run_id), RunControlAction::Cancel)
}

async fn run_heartbeat(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunHeartbeatResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let metadata = state.store.read_metadata(&run_id)?;
    let events = state.store.read_events(&run_id)?;
    let repo_evidence_count = state.store.repo_evidence_count(&run_id)?;
    let has_report = state.store.read_report(&run_id)?.is_some();
    if metadata.is_none() && events.is_empty() && repo_evidence_count == 0 && !has_report {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    Ok(Json(RunHeartbeatResponse {
        run_id: run_id.to_string(),
        status: metadata.as_ref().map(|state| state.status),
        event_count: events.len(),
        has_report,
        repo_evidence_count,
    }))
}

async fn preview_run_report(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunReportResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let report = state.store.build_evidence_report(&run_id)?;
    Ok(Json(RunReportResponse {
        run_id: run_id.to_string(),
        report_ref: None,
        report,
    }))
}

async fn write_run_report(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunReportResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let report = state.store.build_evidence_report(&run_id)?;
    let report_ref = state.store.write_report(&run_id, &report)?;
    Ok(Json(RunReportResponse {
        run_id: run_id.to_string(),
        report_ref: Some(report_ref),
        report,
    }))
}

async fn get_repo_evidence(
    State(state): State<ApiState>,
    Path(ref_id): Path<String>,
) -> Result<Json<RepoEvidenceResponse>, ApiError> {
    let payload = state.store.read_repo_evidence(&ref_id)?;
    Ok(Json(RepoEvidenceResponse { ref_id, payload }))
}

async fn list_run_repo_evidence(
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

async fn get_run_artifact(
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

async fn list_run_checkpoints(
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

async fn get_run_checkpoint(
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

async fn write_run_checkpoint(
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

async fn get_blob_sha256(
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

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    api_version: &'static str,
}

#[derive(Debug, Deserialize)]
pub struct ProjectMemoryLoadRequest {
    pub repo_root: String,
    pub memory_path: String,
    pub run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectMemoryLoadResponse {
    pub record_count: usize,
    pub event_recorded: bool,
    pub memory: ProjectMemoryFile,
}

#[derive(Debug, Deserialize)]
pub struct ConfigValidationRequest {
    pub config: ProjectConfig,
}

#[derive(Debug, Deserialize)]
pub struct WorkflowValidationRequest {
    pub config: ProjectConfig,
    pub workflow_id: String,
}

#[derive(Debug, Deserialize)]
pub struct MockRunRequest {
    pub config: ProjectConfig,
    pub workflow_id: String,
    pub task: String,
}

#[derive(Debug, Deserialize)]
pub struct RunPreviewRequest {
    pub config: ProjectConfig,
    pub workflow_id: String,
    pub task: String,
}

#[derive(Debug, Deserialize)]
pub struct CommandPreviewRequest {
    pub repo_root: String,
    pub cwd: Option<String>,
    pub argv: Vec<String>,
    pub source: Option<String>,
    pub sandbox: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PatchPreviewRequest {
    pub repo_root: String,
    pub patch_file: String,
    pub max_patch_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct PatchApplyToolRequest {
    pub repo_root: String,
    pub patch_file: String,
    pub max_patch_bytes: Option<usize>,
    pub source: Option<String>,
    pub approved: Option<bool>,
    pub run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PatchApplyResponse {
    pub run_id: String,
    pub evidence_ref: RepoEvidenceRef,
    pub result: PatchApplyEvidence,
}

#[derive(Debug, Serialize)]
pub struct MockRunResponse {
    pub run_id: String,
    pub report_ref: String,
    pub report: coder_core::FinalReport,
    pub events_url: String,
}

#[derive(Debug, Serialize)]
pub struct RunEventsResponse {
    pub run_id: String,
    pub events: Vec<coder_events::CoderEvent>,
}

#[derive(Debug, Serialize)]
pub struct RunListResponse {
    pub runs: Vec<StoredRunSummary>,
}

#[derive(Debug, Serialize)]
pub struct RunDetailResponse {
    pub run_id: String,
    pub metadata: Option<RunState>,
    pub events: Vec<coder_events::CoderEvent>,
    pub report: Option<FinalReport>,
    pub repo_evidence_count: usize,
}

#[derive(Debug, Serialize)]
pub struct RunReportResponse {
    pub run_id: String,
    pub report_ref: Option<String>,
    pub report: FinalReport,
}

#[derive(Debug, Serialize)]
pub struct RunControlResponse {
    pub run_id: String,
    pub status: RunStatus,
    pub control_state: String,
    pub event_count: usize,
    pub report_ref: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunHeartbeatResponse {
    pub run_id: String,
    pub status: Option<RunStatus>,
    pub event_count: usize,
    pub has_report: bool,
    pub repo_evidence_count: usize,
}

#[derive(Debug, Serialize)]
pub struct RepoEvidenceResponse {
    pub ref_id: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RunRepoEvidenceResponse {
    pub run_id: String,
    pub evidence: Vec<RepoEvidenceRef>,
}

#[derive(Debug, Serialize)]
pub struct RunArtifactResponse {
    pub run_id: String,
    pub artifact_name: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RunCheckpointListResponse {
    pub run_id: String,
    pub checkpoints: Vec<RunCheckpointRef>,
}

#[derive(Debug, Serialize)]
pub struct RunCheckpointResponse {
    pub run_id: String,
    pub checkpoint_name: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RunCheckpointWriteResponse {
    pub run_id: String,
    pub checkpoint_name: String,
    pub checkpoint_ref: String,
}

#[derive(Debug, Serialize)]
pub struct RunPreviewResponse {
    pub status: &'static str,
    pub requires_confirmation: bool,
    pub workflow_id: String,
    pub task: String,
    pub backends: Vec<String>,
    pub issues: Vec<ValidationIssue>,
}

fn validation_issue(
    level: ValidationLevel,
    code: impl Into<String>,
    message: impl Into<String>,
    target: impl Into<String>,
) -> ValidationIssue {
    ValidationIssue {
        level,
        code: code.into(),
        message: message.into(),
        target: target.into(),
    }
}

#[derive(Debug, Clone, Copy)]
enum RunControlAction {
    Pause,
    Resume,
    Cancel,
}

fn stored_run_exists(store: &RunStore, run_id: &RunId) -> Result<bool, StoreError> {
    Ok(store.read_metadata(run_id)?.is_some()
        || !store.read_events(run_id)?.is_empty()
        || store.read_report(run_id)?.is_some()
        || store.repo_evidence_count(run_id)? > 0
        || !store.list_checkpoints(run_id)?.is_empty())
}

fn resolve_repo_relative_path(repo_root: &str, relative_path: &str) -> Result<PathBuf, ApiError> {
    let root = fs::canonicalize(repo_root)
        .map_err(|error| ApiError::bad_request(format!("invalid repo_root: {error}")))?;
    let requested = PathBuf::from(relative_path);
    if requested.is_absolute() {
        return Err(ApiError::bad_request("memory_path must be relative"));
    }
    let resolved = fs::canonicalize(root.join(&requested))
        .map_err(|error| ApiError::bad_request(format!("invalid memory_path: {error}")))?;
    if !resolved.starts_with(&root) {
        return Err(ApiError::bad_request("memory_path escapes repo_root"));
    }
    Ok(resolved)
}

fn control_run(
    state: ApiState,
    run_id: RunId,
    action: RunControlAction,
) -> Result<Json<RunControlResponse>, ApiError> {
    let mut metadata = state
        .store
        .read_metadata(&run_id)?
        .ok_or_else(|| ApiError::not_found(format!("run '{}' was not found", run_id.as_str())))?;
    let events = state.store.read_events(&run_id)?;
    let (kind, status_text) = match action {
        RunControlAction::Pause => ("run.paused", "paused"),
        RunControlAction::Resume => {
            metadata.status = RunStatus::Running;
            ("run.resumed", "running")
        }
        RunControlAction::Cancel => {
            metadata.status = RunStatus::Cancelled;
            ("run.cancelled", "cancelled")
        }
    };
    let sequence = events.len() as u64 + 1;
    let event_count = events.len() + 1;
    let event = coder_events::CoderEvent::new(
        run_id.clone(),
        sequence,
        kind,
        json!({
            "status": status_text,
        }),
    );
    metadata.updated_at = event.timestamp;
    state.store.write_metadata(&metadata)?;
    state.store.append_event(&run_id, &event)?;
    let report_ref = if matches!(action, RunControlAction::Cancel) {
        let report = state.store.build_evidence_report(&run_id)?;
        Some(state.store.write_report(&run_id, &report)?)
    } else {
        None
    };
    Ok(Json(RunControlResponse {
        run_id: run_id.to_string(),
        status: metadata.status,
        control_state: status_text.to_owned(),
        event_count,
        report_ref,
    }))
}

fn record_patch_apply_event(
    store: &RunStore,
    run_id: &RunId,
    output: &PatchApplyEvidence,
    evidence_ref: &RepoEvidenceRef,
) -> Result<(), StoreError> {
    let sequence = store.read_events(run_id)?.len() as u64 + 1;
    let evidence_uri = format!("repo-evidence://{}", evidence_ref.ref_id);
    if output.requires_approval {
        store.append_event(
            run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                sequence,
                "approval.requested",
                json!({
                    "approval_type": "patch_apply",
                    "approval_key": &output.approval_key,
                    "patch_file": &output.patch_file,
                    "reason": &output.reason,
                    "files": &output.preview.files,
                    "evidence_ref": &evidence_ref.ref_id,
                }),
            )
            .with_ref("patch_evidence", evidence_uri),
        )?;
        return Ok(());
    }

    let kind = if output.applied {
        "patch.applied"
    } else {
        "patch.failed"
    };
    store.append_event(
        run_id,
        &coder_events::CoderEvent::new(
            run_id.clone(),
            sequence,
            kind,
            json!({
                "status": &output.status,
                "patch_file": &output.patch_file,
                "applied": output.applied,
                "reason": &output.reason,
                "approval_key": &output.approval_key,
                "file_count": output.preview.file_count,
                "files": &output.preview.files,
                "evidence_ref": &evidence_ref.ref_id,
            }),
        )
        .with_ref("patch_evidence", evidence_uri),
    )?;
    Ok(())
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": self.message,
            })),
        )
            .into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::RunNotFound(_)
            | StoreError::RepoEvidenceNotFound(_)
            | StoreError::ArtifactNotFound { .. }
            | StoreError::CheckpointNotFound { .. }
            | StoreError::BlobNotFound(_) => Self::not_found(error.to_string()),
            StoreError::InvalidStoreSegment { .. }
            | StoreError::InvalidFileName(_)
            | StoreError::InvalidBlobDigest(_) => Self {
                status: StatusCode::BAD_REQUEST,
                message: error.to_string(),
            },
            other => Self::internal(other.to_string()),
        }
    }
}

impl From<WorkflowError> for ApiError {
    fn from(error: WorkflowError) -> Self {
        match error {
            WorkflowError::InvalidConfig(_) | WorkflowError::WorkflowNotFound(_) => Self {
                status: StatusCode::BAD_REQUEST,
                message: error.to_string(),
            },
            WorkflowError::Store(error) => Self::from(error),
        }
    }
}

impl From<RepoToolError> for ApiError {
    fn from(error: RepoToolError) -> Self {
        match error {
            RepoToolError::InvalidRoot { .. }
            | RepoToolError::InvalidRootKind(_)
            | RepoToolError::PathNotFound { .. }
            | RepoToolError::PathOutsideRepo(_)
            | RepoToolError::NotAFile(_)
            | RepoToolError::NotADirectory(_)
            | RepoToolError::SensitivePath(_)
            | RepoToolError::BinaryFile(_)
            | RepoToolError::FileTooLarge { .. }
            | RepoToolError::EmptyQuery
            | RepoToolError::PatchNoFiles(_)
            | RepoToolError::EmptyCommandArgv => Self {
                status: StatusCode::BAD_REQUEST,
                message: error.to_string(),
            },
            other => Self::internal(other.to_string()),
        }
    }
}

impl From<MemoryError> for ApiError {
    fn from(error: MemoryError) -> Self {
        Self::bad_request(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn health_endpoint_returns_v3_status() {
        let app = test_router();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v3/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "ok");
        assert_eq!(body["api_version"], "v3");
    }

    #[tokio::test]
    async fn project_memory_load_records_summary_event_without_full_content() {
        let repo = temp_root();
        let store_root = temp_root();
        fs::create_dir_all(&repo).unwrap();
        fs::write(
            repo.join("memory.json"),
            r#"{
              "version": 1,
              "records": [
                {
                  "id": "mem_1",
                  "scope": "project",
                  "key": "architecture",
                  "content": "Rust owns the control plane.",
                  "tags": ["rust"],
                  "source_ref": "memory://project/architecture"
                }
              ]
            }"#,
        )
        .unwrap();
        let store = RunStore::new(&store_root);
        let run_id = RunId::from_string("run-1");
        let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
        store.write_metadata(&state).unwrap();
        let app = router(ApiState::new(store.clone()));

        let response = post_json(
            app,
            "/api/v3/memory/project/load",
            json!({
                "repo_root": repo.display().to_string(),
                "memory_path": "memory.json",
                "run_id": "run-1"
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["record_count"], 1);
        assert_eq!(body["event_recorded"], true);
        assert_eq!(body["memory"]["records"][0]["key"], "architecture");
        let events = store.read_events(&run_id).unwrap();
        assert_eq!(events[0].kind, "memory.read");
        assert_eq!(events[0].payload["records"][0]["key"], "architecture");
        assert!(!events[0].payload.to_string().contains("control plane"));
        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(store_root);
    }

    #[tokio::test]
    async fn config_validate_endpoint_returns_report() {
        let app = test_router();
        let response = post_json(
            app,
            "/api/v3/config/validate",
            json!({"config": example_config()}),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "pass");
    }

    #[tokio::test]
    async fn run_list_endpoint_returns_empty_store() {
        let app = test_router();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["runs"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn run_preview_is_side_effect_free_and_reports_ready() {
        let root = temp_root();
        let app = router(ApiState::new(RunStore::new(&root)));
        let response = post_json(
            app,
            "/api/v3/runs/preview",
            json!({
                "config": example_config(),
                "workflow_id": "planner-led",
                "task": "summarize the repo"
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "ready");
        assert_eq!(body["requires_confirmation"], true);
        assert_eq!(body["issues"].as_array().unwrap().len(), 0);
        assert!(body["backends"]
            .as_array()
            .unwrap()
            .iter()
            .any(|backend| backend.as_str() == Some("openhands")));
        assert!(!root.join("runs").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn run_preview_blocks_missing_workflow_and_empty_task() {
        let app = test_router();
        let response = post_json(
            app,
            "/api/v3/runs/preview",
            json!({
                "config": example_config(),
                "workflow_id": "missing",
                "task": "  "
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "blocked");
        assert_eq!(body["requires_confirmation"], false);
        let codes = body["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|issue| issue["code"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"workflow_not_found"));
        assert!(codes.contains(&"task_empty"));
    }

    #[tokio::test]
    async fn command_preview_endpoint_returns_policy_without_running() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        let app = test_router();
        let response = post_json(
            app,
            "/api/v3/tools/command/preview",
            json!({
                "repo_root": root.display().to_string(),
                "cwd": ".",
                "argv": ["cmd.exe", "/C", "echo", "preview"],
                "source": "model",
                "sandbox": false
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["cwd"], ".");
        assert_eq!(body["requires_approval"], true);
        assert_eq!(body["policy"]["risk"], "medium");
        assert!(body["approval_key"].as_str().unwrap().starts_with("cmd:"));
        assert_eq!(body["evidence_kind"], "command_preview");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn command_preview_endpoint_rejects_cwd_escape() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        let app = test_router();
        let response = post_json(
            app,
            "/api/v3/tools/command/preview",
            json!({
                "repo_root": root.display().to_string(),
                "cwd": "..",
                "argv": ["cmd.exe", "/C", "echo", "preview"],
                "source": "discovered"
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn patch_preview_endpoint_summarizes_patch_without_writing_store() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("tracked.txt"), "base\n").unwrap();
        fs::write(
            root.join("change.patch"),
            "\
diff --git a/tracked.txt b/tracked.txt
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
        )
        .unwrap();
        let app = test_router();

        let response = post_json(
            app,
            "/api/v3/tools/patch/preview",
            json!({
                "repo_root": root.display().to_string(),
                "patch_file": "change.patch"
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["file_count"], 1);
        assert_eq!(body["files"][0]["new_path"], "tracked.txt");
        assert!(!root.join("runs").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn patch_apply_endpoint_requires_run_id_and_records_approval() {
        let repo = temp_root();
        let store_root = temp_root();
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        fs::write(
            repo.join("change.patch"),
            "\
diff --git a/tracked.txt b/tracked.txt
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
        )
        .unwrap();
        let app = router(ApiState::new(RunStore::new(&store_root)));

        let missing_run_response = post_json(
            app.clone(),
            "/api/v3/tools/patch/apply",
            json!({
                "repo_root": repo.display().to_string(),
                "patch_file": "change.patch",
                "source": "model"
            }),
        )
        .await;
        assert_eq!(missing_run_response.status(), StatusCode::BAD_REQUEST);

        let apply_response = post_json(
            app.clone(),
            "/api/v3/tools/patch/apply",
            json!({
                "repo_root": repo.display().to_string(),
                "patch_file": "change.patch",
                "source": "model",
                "approved": false,
                "run_id": "run-1"
            }),
        )
        .await;
        assert_eq!(apply_response.status(), StatusCode::OK);
        let apply_body = response_json(apply_response).await;
        assert_eq!(apply_body["result"]["status"], "blocked");
        assert!(apply_body["evidence_ref"]["ref_id"]
            .as_str()
            .unwrap()
            .starts_with("repo-diff:"));

        let report_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs/run-1/report/preview")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let report_body = response_json(report_response).await;
        assert_eq!(report_body["report"]["status"], "blocked");
        assert_eq!(report_body["report"]["changed_files"][0], "tracked.txt");
        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(store_root);
    }

    #[tokio::test]
    async fn run_report_preview_and_write_are_evidence_backed() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &coder_events::CoderEvent::new(
                    run_id.clone(),
                    1,
                    "command.completed",
                    json!({
                        "command": "cargo test",
                        "status": "completed",
                        "passed": true,
                        "returncode": 0
                    }),
                ),
            )
            .unwrap();
        let app = router(ApiState::new(store));

        let preview_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs/run-1/report/preview")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(preview_response.status(), StatusCode::OK);
        let preview_body = response_json(preview_response).await;
        assert_eq!(preview_body["report_ref"], Value::Null);
        assert_eq!(preview_body["report"]["status"], "completed");
        assert!(preview_body["report"]["checks"][0]
            .as_str()
            .unwrap()
            .contains("cargo test"));

        let write_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v3/runs/run-1/report")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(write_response.status(), StatusCode::OK);
        let write_body = response_json(write_response).await;
        assert!(write_body["report_ref"]
            .as_str()
            .unwrap()
            .ends_with("/final-report.json"));

        let detail_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs/run-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let detail_body = response_json(detail_response).await;
        assert_eq!(
            detail_body["report"]["checks"][0],
            "cargo test: completed exit 0"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn run_control_endpoints_record_events_and_cancel_report() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
        state.status = RunStatus::Running;
        store.write_metadata(&state).unwrap();
        store
            .append_event(
                &run_id,
                &coder_events::CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
            )
            .unwrap();
        let app = router(ApiState::new(store));

        let heartbeat_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs/run-1/heartbeat")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(heartbeat_response.status(), StatusCode::OK);
        let heartbeat_body = response_json(heartbeat_response).await;
        assert_eq!(heartbeat_body["status"], "running");
        assert_eq!(heartbeat_body["event_count"], 1);

        let pause_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v3/runs/run-1/pause")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let pause_body = response_json(pause_response).await;
        assert_eq!(pause_body["status"], "running");
        assert_eq!(pause_body["control_state"], "paused");
        assert_eq!(pause_body["event_count"], 2);

        let resume_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v3/runs/run-1/resume")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let resume_body = response_json(resume_response).await;
        assert_eq!(resume_body["status"], "running");
        assert_eq!(resume_body["control_state"], "running");
        assert_eq!(resume_body["event_count"], 3);

        let cancel_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v3/runs/run-1/cancel")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let cancel_body = response_json(cancel_response).await;
        assert_eq!(cancel_body["status"], "cancelled");
        assert_eq!(cancel_body["control_state"], "cancelled");
        assert_eq!(cancel_body["event_count"], 4);
        assert!(cancel_body["report_ref"]
            .as_str()
            .unwrap()
            .ends_with("/final-report.json"));

        let detail_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs/run-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let detail_body = response_json(detail_response).await;
        assert_eq!(detail_body["metadata"]["status"], "cancelled");
        assert_eq!(detail_body["report"]["status"], "cancelled");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn mock_run_endpoint_writes_events_visible_through_events_endpoint() {
        let root = temp_root();
        let app = router(ApiState::new(RunStore::new(&root)));
        let response = post_json(
            app.clone(),
            "/api/v3/runs/mock",
            json!({
                "config": example_config(),
                "workflow_id": "planner-led",
                "task": "summarize the repo"
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let events_url = body["events_url"].as_str().unwrap();
        let run_id = body["run_id"].as_str().unwrap();

        let events_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(events_url)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(events_response.status(), StatusCode::OK);
        let events_body = response_json(events_response).await;
        assert_eq!(events_body["events"][0]["kind"], "run.started");

        let detail_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v3/runs/{run_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail_response.status(), StatusCode::OK);
        let detail_body = response_json(detail_response).await;
        assert_eq!(detail_body["metadata"]["status"], "completed");
        assert_eq!(detail_body["report"]["status"], "completed");
        assert_eq!(
            detail_body["report"]["evidence_refs"][0]["kind"],
            "event_log"
        );

        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body = response_json(list_response).await;
        assert_eq!(list_body["runs"].as_array().unwrap().len(), 1);
        assert_eq!(list_body["runs"][0]["run_id"], run_id);
        assert_eq!(list_body["runs"][0]["metadata"]["status"], "completed");
        assert!(list_body["runs"][0]["event_count"].as_u64().unwrap() >= 1);
        assert_eq!(list_body["runs"][0]["has_report"], true);

        let artifact_response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v3/runs/{run_id}/artifacts/final-report.json"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(artifact_response.status(), StatusCode::OK);
        let artifact_body = response_json(artifact_response).await;
        assert_eq!(artifact_body["artifact_name"], "final-report.json");
        assert_eq!(artifact_body["payload"]["status"], "completed");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn repo_evidence_endpoint_returns_payload_by_ref() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let reference = store
            .write_repo_evidence(
                &RunId::from_string("run-1"),
                coder_store::RepoEvidenceKind::RepoRead,
                "repo",
                Vec::new(),
                "Read src/app.py.",
                json!({
                    "evidence_kind": "repo_evidence",
                    "operation": "read_file_range",
                    "snippet": {"path": "src/app.py", "text": "safe"}
                }),
            )
            .unwrap();
        let app = router(ApiState::new(store));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v3/repo-evidence/{}", reference.ref_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["ref_id"], reference.ref_id);
        assert_eq!(body["payload"]["operation"], "read_file_range");
        assert_eq!(body["payload"]["snippet"]["path"], "src/app.py");

        let detail_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs/run-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail_response.status(), StatusCode::OK);
        let detail_body = response_json(detail_response).await;
        assert_eq!(detail_body["run_id"], "run-1");
        assert_eq!(detail_body["repo_evidence_count"], 1);
        assert_eq!(detail_body["metadata"], Value::Null);

        let list_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs/run-1/repo-evidence")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body = response_json(list_response).await;
        assert_eq!(list_body["run_id"], "run-1");
        assert_eq!(list_body["evidence"][0]["ref_id"], reference.ref_id);
        assert_eq!(list_body["evidence"][0]["summary"], "Read src/app.py.");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn repo_evidence_endpoint_reports_missing_and_invalid_refs() {
        let app = test_router();
        let missing_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/repo-evidence/repo-read:missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let invalid_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v3/repo-evidence/bad*ref")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
        assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn artifact_endpoint_reports_missing_and_invalid_names() {
        let app = test_router();
        let missing_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs/run-1/artifacts/missing.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let invalid_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs/run-1/artifacts/bad*name.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
        assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn checkpoint_endpoints_roundtrip_and_validate_names() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
        store.write_metadata(&state).unwrap();
        let app = router(ApiState::new(store));

        let write_response = post_json(
            app.clone(),
            "/api/v3/runs/run-1/checkpoints/resume.json",
            json!({"step": 2}),
        )
        .await;
        assert_eq!(write_response.status(), StatusCode::OK);
        let write_body = response_json(write_response).await;
        assert_eq!(write_body["checkpoint_name"], "resume.json");
        assert!(write_body["checkpoint_ref"]
            .as_str()
            .unwrap()
            .ends_with("/resume.json"));

        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs/run-1/checkpoints")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let list_body = response_json(list_response).await;
        assert_eq!(list_body["checkpoints"][0]["name"], "resume.json");

        let read_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs/run-1/checkpoints/resume.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let read_body = response_json(read_response).await;
        assert_eq!(read_body["payload"]["step"], 2);

        let missing_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs/run-1/checkpoints/missing.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let invalid_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v3/runs/run-1/checkpoints/bad*name.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
        assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn blob_endpoint_returns_content_by_digest() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let blob_ref = store.write_blob(b"hello blob").unwrap();
        let digest = blob_ref.strip_prefix("blob://sha256/").unwrap().to_owned();
        let app = router(ApiState::new(store));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v3/blobs/sha256/{digest}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(bytes.as_ref(), b"hello blob");

        let missing_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/blobs/sha256/0000000000000000000000000000000000000000000000000000000000000000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let invalid_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v3/blobs/sha256/not-a-digest")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
        assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
        let _ = fs::remove_dir_all(root);
    }

    fn test_router() -> Router {
        router(ApiState::new(RunStore::new(temp_root())))
    }

    async fn post_json(app: Router, uri: &str, body: Value) -> axum::response::Response {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    async fn response_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn example_config() -> Value {
        serde_yaml::from_str::<ProjectConfig>(include_str!("../../../examples/coder.yaml"))
            .map(|config| serde_json::to_value(config).unwrap())
            .unwrap()
    }

    fn temp_root() -> PathBuf {
        static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("coder-server-{}-{}", std::process::id(), id))
    }
}
