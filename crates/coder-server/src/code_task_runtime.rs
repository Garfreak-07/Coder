use std::{path::PathBuf, sync::Arc};

use coder_config::{resolve_task_cost_policy, ProjectConfig};
use coder_core::RunId;
use coder_events::{OutputEvent, OutputPriority};
use coder_workflow::{
    BackendRegistry, WorkflowEventSink, WorkflowRunControl, WorkflowRunOptions, WorkflowRunner,
};

use crate::native_model_backend::NativeModelBackend;
use crate::provider_settings::apply_provider_settings_to_project_config;
use crate::{ApiError, ApiState, TaskRunRequest, TaskRunResponse};

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CodeTaskRuntime;

impl CodeTaskRuntime {
    pub(crate) async fn run(
        &self,
        state: ApiState,
        request: TaskRunRequest,
    ) -> Result<TaskRunResponse, ApiError> {
        let provider_settings = state.provider_settings.lock().unwrap().clone();
        let run_id = requested_or_new_run_id(request.run_id.as_deref())?;
        let output_session_id = request.session_id.clone();
        if let Some(session_id) = output_session_id.as_deref() {
            state.session_host.get_conversation(&state, session_id)?;
        }
        if state.session_host.task_is_active(&run_id)
            || state.store.read_metadata(&run_id)?.is_some()
        {
            return Err(ApiError::conflict(format!(
                "run '{}' already exists",
                run_id.as_str()
            )));
        }
        let mut config = request.config;
        apply_provider_settings_to_project_config(&mut config, &provider_settings);
        let token_budget = resolve_task_cost_policy(&config, &request.workflow_id)
            .map(|policy| policy.token_budget);
        crate::run_token_budget::initialize_run_token_budget(&state, &run_id, token_budget);
        let mut options = WorkflowRunOptions::new(&request.workflow_id, &request.task);
        if let Some(repo_root) = &request.repo_root {
            options.repo_root = PathBuf::from(repo_root);
        }
        options.task_context = request.task_context;
        options.run_id = Some(run_id.clone());
        let (control_sender, control_receiver) =
            tokio::sync::watch::channel(WorkflowRunControl::Running);
        options.control = Some(control_receiver);
        state
            .session_host
            .register_task(&run_id, control_sender)
            .map_err(ApiError::conflict)?;
        let mut runner = Self::runner(config, state.store.clone(), state.clone());
        if let Some(session_id) = output_session_id {
            let session_host = state.session_host.clone();
            let event_sink: WorkflowEventSink = Arc::new(move |event| {
                session_host.publish_output(
                    &session_id,
                    None,
                    "code",
                    OutputPriority::Normal,
                    OutputEvent::CodeEvent {
                        event: Box::new(event.clone()),
                    },
                );
            });
            runner = runner.with_event_sink(event_sink);
        }
        let run_result = runner.run(options).await;
        state.session_host.deactivate_task(&run_id);
        crate::run_token_budget::clear_run_token_budget_if_inactive(&state, &run_id);
        let output = run_result?;
        Ok(TaskRunResponse {
            run_id: output.run_id.to_string(),
            report_ref: output.report_ref,
            report: output.report,
            events_url: format!("/api/v3/runs/{}/events", output.run_id.as_str()),
        })
    }

    pub(crate) fn runner(
        config: ProjectConfig,
        store: coder_store::RunStore,
        state: ApiState,
    ) -> WorkflowRunner {
        let registry = BackendRegistry::for_host()
            .with_native_backend(Arc::new(NativeModelBackend::new(state)));
        WorkflowRunner::with_registry(config, store, registry)
    }
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
