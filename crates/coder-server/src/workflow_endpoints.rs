use axum::{extract::State, Json};
use coder_config::{validate_project_config, ProjectConfig, ValidationLevel, ValidationReport};
use coder_store::RunStore;
use coder_workflow::WorkflowRunOptions;

use crate::api_types::validation_issue;
use crate::provider_settings::apply_provider_settings_to_project_config;
use crate::{
    ApiError, ApiState, ConfigValidationRequest, RunPreviewRequest, RunPreviewResponse,
    TaskRunRequest, TaskRunResponse,
};

pub(crate) fn default_project_config() -> ProjectConfig {
    serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap()
}

pub(crate) async fn validate_config(
    Json(request): Json<ConfigValidationRequest>,
) -> Json<ValidationReport> {
    Json(validate_project_config(&request.config))
}

pub(crate) async fn run_workflow(
    State(state): State<ApiState>,
    Json(request): Json<TaskRunRequest>,
) -> Result<Json<TaskRunResponse>, ApiError> {
    let response = state.session_host.run_task(state.clone(), request).await?;
    Ok(Json(response))
}

pub(crate) async fn preview_run(
    Json(request): Json<RunPreviewRequest>,
) -> Json<RunPreviewResponse> {
    let mut issues = validate_project_config(&request.config).issues;
    let workflow = request.config.task_profiles.get(&request.workflow_id);
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
        .and_then(|workflow| request.config.harnesses.get(&workflow.harness))
        .map(|harness| vec![harness.backend.clone()])
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

pub async fn run_embedded_workflow(
    mut config: ProjectConfig,
    store: RunStore,
    mut options: WorkflowRunOptions,
) -> Result<coder_workflow::WorkflowRunOutput, coder_workflow::WorkflowError> {
    let state = ApiState::new(store.clone());
    let provider_settings = state.provider_settings.lock().unwrap().clone();
    apply_provider_settings_to_project_config(&mut config, &provider_settings);
    let run_id = options.run_id.clone().unwrap_or_default();
    options.run_id = Some(run_id.clone());
    let token_budget = coder_config::resolve_task_cost_policy(&config, &options.workflow_id)
        .map(|policy| policy.token_budget);
    crate::run_token_budget::initialize_run_token_budget(&state, &run_id, token_budget);
    let runner = crate::code_task_runtime::CodeTaskRuntime::runner(config, store, state.clone());
    let result = runner.run(options).await;
    crate::run_token_budget::clear_run_token_budget_if_inactive(&state, &run_id);
    result
}
