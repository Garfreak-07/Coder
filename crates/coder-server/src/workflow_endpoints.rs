use axum::{
    extract::{Path, State},
    Json,
};
use coder_config::{validate_project_config, ProjectConfig, ValidationLevel, ValidationReport};
use coder_core::RunId;
use coder_store::RunStore;
use coder_workflow::{
    BackendRegistry, MockWorkflowRunner, WorkflowRunControl, WorkflowRunOptions, WorkflowRunner,
};
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use crate::api_types::validation_issue;
use crate::native_model_backend::NativeModelBackend;
use crate::provider_settings::apply_provider_settings_to_project_config;
use crate::workflow_planner_backend::WorkflowPlannerBackend;
use crate::{
    ApiError, ApiState, ConfigValidationRequest, DefaultWorkflowResponse, LibraryResponse,
    LibraryWorkflowGetResponse, LibraryWorkflowSaveRequest, LibraryWorkflowSaveResponse,
    LibraryWorkflowSummary, MockRunRequest, MockRunResponse, RunPreviewRequest, RunPreviewResponse,
    WorkflowValidationRequest,
};

pub(crate) async fn default_workflow() -> Json<DefaultWorkflowResponse> {
    let config = default_project_config();
    let workflow_id = "planner-led".to_owned();
    let workflow = config.workflows.get(&workflow_id).cloned();
    Json(DefaultWorkflowResponse {
        workflow_id,
        config,
        workflow,
    })
}

pub(crate) fn default_project_config() -> ProjectConfig {
    serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap()
}

pub(crate) async fn get_library(State(state): State<ApiState>) -> Json<LibraryResponse> {
    let workflows = state
        .library_workflows
        .lock()
        .unwrap()
        .iter()
        .map(|(id, workflow)| LibraryWorkflowSummary {
            id: id.clone(),
            workflow: workflow.clone(),
        })
        .collect();
    Json(LibraryResponse { workflows })
}

pub(crate) async fn save_library_workflow(
    State(state): State<ApiState>,
    Json(request): Json<LibraryWorkflowSaveRequest>,
) -> Result<Json<LibraryWorkflowSaveResponse>, ApiError> {
    if request.workflow_id.trim().is_empty() {
        return Err(ApiError::bad_request("workflow_id must not be empty"));
    }
    state
        .library_workflows
        .lock()
        .unwrap()
        .insert(request.workflow_id.clone(), request.workflow.clone());
    Ok(Json(LibraryWorkflowSaveResponse {
        workflow_id: request.workflow_id,
        workflow: request.workflow,
        saved: true,
    }))
}

pub(crate) async fn get_library_workflow(
    State(state): State<ApiState>,
    Path(workflow_id): Path<String>,
) -> Result<Json<LibraryWorkflowGetResponse>, ApiError> {
    let workflow = state
        .library_workflows
        .lock()
        .unwrap()
        .get(&workflow_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("workflow '{workflow_id}' was not found")))?;
    Ok(Json(LibraryWorkflowGetResponse {
        workflow_id,
        workflow,
    }))
}

pub(crate) async fn validate_config(
    Json(request): Json<ConfigValidationRequest>,
) -> Json<ValidationReport> {
    Json(validate_project_config(&request.config))
}

pub(crate) async fn validate_workflow(
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

pub(crate) async fn run_mock_workflow(
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

pub(crate) async fn run_workflow(
    State(state): State<ApiState>,
    Json(request): Json<MockRunRequest>,
) -> Result<Json<MockRunResponse>, ApiError> {
    let provider_settings = state.provider_settings.lock().unwrap().clone();
    let run_id = requested_or_new_run_id(request.run_id.as_deref())?;
    if state
        .active_run_controls
        .lock()
        .unwrap()
        .contains_key(run_id.as_str())
        || state.store.read_metadata(&run_id)?.is_some()
    {
        return Err(ApiError::conflict(format!(
            "run '{}' already exists",
            run_id.as_str()
        )));
    }
    let mut config = request.config;
    apply_provider_settings_to_project_config(&mut config, &provider_settings);
    let token_budget = config
        .workflows
        .get(&request.workflow_id)
        .and_then(|workflow| workflow.token_budget);
    crate::run_token_budget::initialize_run_token_budget(&state, &run_id, token_budget);
    let mut options = WorkflowRunOptions::new(&request.workflow_id, &request.task);
    if let Some(repo_root) = &request.repo_root {
        options.repo_root = PathBuf::from(repo_root);
    }
    options.plan_context = request.plan_context.clone();
    options.run_id = Some(run_id.clone());
    let (control_sender, control_receiver) =
        tokio::sync::watch::channel(WorkflowRunControl::Running);
    options.control = Some(control_receiver);
    state
        .active_run_controls
        .lock()
        .unwrap()
        .insert(run_id.to_string(), control_sender);
    let runner = workflow_runner_for_api(config, state.store.clone(), state.clone());
    let run_result = runner.run(options).await;
    state
        .active_run_controls
        .lock()
        .unwrap()
        .remove(run_id.as_str());
    crate::run_token_budget::clear_run_token_budget_if_inactive(&state, &run_id);
    let output = run_result?;
    Ok(Json(MockRunResponse {
        run_id: output.run_id.to_string(),
        report_ref: output.report_ref,
        report: output.report,
        events_url: format!("/api/v3/runs/{}/events", output.run_id.as_str()),
    }))
}

fn requested_or_new_run_id(requested: Option<&str>) -> Result<RunId, ApiError> {
    let Some(requested) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(RunId::new());
    };
    if requested.len() > 128
        || !requested
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(ApiError::bad_request(
            "run_id must contain only ASCII letters, digits, '-' or '_' and be at most 128 characters",
        ));
    }
    Ok(RunId::from_string(requested))
}

pub(crate) async fn preview_run(
    Json(request): Json<RunPreviewRequest>,
) -> Json<RunPreviewResponse> {
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

pub(crate) fn workflow_runner_for_api(
    config: ProjectConfig,
    store: RunStore,
    state: ApiState,
) -> WorkflowRunner {
    let registry = BackendRegistry::from_project_config(&config, store.clone())
        .with_planner_backend(Arc::new(WorkflowPlannerBackend::new(state.clone())))
        .with_native_backend(Arc::new(NativeModelBackend::new(state)));
    WorkflowRunner::with_registry(config, store, registry)
}
