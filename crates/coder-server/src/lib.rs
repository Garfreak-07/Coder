use std::{collections::BTreeSet, net::SocketAddr};

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
use coder_core::{FinalReport, RunId, RunState};
use coder_store::{RunStore, StoreError, StoredRunSummary};
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
        .route("/api/v3/config/validate", post(validate_config))
        .route("/api/v3/workflows/validate", post(validate_workflow))
        .route("/api/v3/runs", get(list_runs))
        .route("/api/v3/runs/preview", post(preview_run))
        .route("/api/v3/runs/mock", post(run_mock_workflow))
        .route("/api/v3/runs/{run_id}", get(get_run_detail))
        .route("/api/v3/runs/{run_id}/events", get(list_run_events))
        .route(
            "/api/v3/runs/{run_id}/artifacts/{artifact_name}",
            get(get_run_artifact),
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

async fn get_repo_evidence(
    State(state): State<ApiState>,
    Path(ref_id): Path<String>,
) -> Result<Json<RepoEvidenceResponse>, ApiError> {
    let payload = state.store.read_repo_evidence(&ref_id)?;
    Ok(Json(RepoEvidenceResponse { ref_id, payload }))
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
pub struct RepoEvidenceResponse {
    pub ref_id: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RunArtifactResponse {
    pub run_id: String,
    pub artifact_name: String,
    pub payload: serde_json::Value,
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

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
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
            StoreError::RepoEvidenceNotFound(_)
            | StoreError::ArtifactNotFound { .. }
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
        std::env::temp_dir().join(format!(
            "coder-server-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
