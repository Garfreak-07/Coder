use coder_config::{validate_project_config, ProjectConfig, TaskProfile};
use coder_core::{FinalReport, RunId, RunRequest, RunState, RunStatus, WorkflowId};
use coder_events::CoderEvent;
use coder_store::RunStore;
use serde_json::json;

use crate::WorkflowError;

pub struct MockWorkflowRunner<'a> {
    config: &'a ProjectConfig,
    store: RunStore,
}

impl<'a> MockWorkflowRunner<'a> {
    pub fn new(config: &'a ProjectConfig, store: RunStore) -> Self {
        Self { config, store }
    }

    pub fn run(&self, workflow_id: &str, task: &str) -> Result<MockRunOutput, WorkflowError> {
        self.run_with_options(workflow_id, task, MockRunOptions::default())
    }

    pub fn run_with_options(
        &self,
        workflow_id: &str,
        task: &str,
        options: MockRunOptions,
    ) -> Result<MockRunOutput, WorkflowError> {
        let validation = validate_project_config(self.config);
        if !validation.is_pass() {
            return Err(WorkflowError::InvalidConfig(validation.status));
        }
        let workflow = self
            .config
            .task_profiles
            .get(workflow_id)
            .ok_or_else(|| WorkflowError::WorkflowNotFound(workflow_id.to_owned()))?;

        let run_id = RunId::new();
        let request = RunRequest {
            repo_root: ".".to_owned(),
            task: task.to_owned(),
            workflow_id: WorkflowId::new(workflow_id),
        };
        let mut state = RunState::new(run_id.clone(), request.workflow_id.clone());
        state.status = RunStatus::Running;
        self.store.write_metadata(&state)?;

        let mut sequence = 1;
        self.emit(
            &run_id,
            sequence,
            "run.started",
            json!({
                "task_profile_id": workflow_id,
                "task": task
            }),
        )?;
        sequence += 1;

        self.emit(
            &run_id,
            sequence,
            "agent.started",
            json!({
                "agent_id": workflow_id,
                "harness_id": workflow.harness,
                "mock": true
            }),
        )?;
        sequence += 1;
        self.emit(
            &run_id,
            sequence,
            "agent.completed",
            json!({
                "agent_id": workflow_id,
                "status": "completed",
                "mock": true
            }),
        )?;
        sequence += 1;

        let outcome = options.outcome;
        let report = report_for_mock_run(&run_id, workflow_id, workflow, task, outcome);
        let report_ref = self.store.write_report(&run_id, &report)?;
        self.emit(
            &run_id,
            sequence,
            "report.created",
            json!({"report_ref": report_ref}),
        )?;
        sequence += 1;
        let terminal_event = match outcome {
            MockRunOutcome::Completed => "run.completed",
            MockRunOutcome::Blocked => "run.blocked",
            MockRunOutcome::Failed => "run.failed",
        };
        self.emit(
            &run_id,
            sequence,
            terminal_event,
            json!({
                "status": outcome.as_status_str(),
                "report_ref": report_ref
            }),
        )?;

        state.status = outcome.run_status();
        state.updated_at = time::OffsetDateTime::now_utc();
        self.store.write_metadata(&state)?;

        Ok(MockRunOutput {
            run_id,
            report,
            report_ref,
        })
    }

    fn emit(
        &self,
        run_id: &RunId,
        sequence: u64,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<(), WorkflowError> {
        let event = CoderEvent::new(run_id.clone(), sequence, kind, payload);
        self.store.append_event(run_id, &event)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockRunOutcome {
    Completed,
    Blocked,
    Failed,
}

impl MockRunOutcome {
    fn as_status_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }

    fn run_status(self) -> RunStatus {
        match self {
            Self::Completed => RunStatus::Completed,
            Self::Blocked => RunStatus::Blocked,
            Self::Failed => RunStatus::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MockRunOptions {
    pub outcome: MockRunOutcome,
}

impl Default for MockRunOptions {
    fn default() -> Self {
        Self {
            outcome: MockRunOutcome::Completed,
        }
    }
}

#[derive(Debug)]
pub struct MockRunOutput {
    pub run_id: RunId,
    pub report: FinalReport,
    pub report_ref: String,
}

fn report_for_mock_run(
    run_id: &RunId,
    workflow_id: &str,
    workflow: &TaskProfile,
    task: &str,
    outcome: MockRunOutcome,
) -> FinalReport {
    let visited_nodes = usize::from(!workflow.harness.is_empty());
    let summary = format!(
        "Mock task profile '{workflow_id}' accepted task '{task}' and ran {visited_nodes} agent(s)."
    );
    let mut report = match outcome {
        MockRunOutcome::Completed => FinalReport::completed(summary),
        MockRunOutcome::Blocked => {
            FinalReport::blocked(summary, "mock run requested blocked outcome")
        }
        MockRunOutcome::Failed => FinalReport::failed(summary, "mock run requested failed outcome"),
    };
    report = report
        .with_check(format!("mock node visits: {visited_nodes}"))
        .with_evidence(
            "event_log",
            format!("eventlog://runs/{}/events.jsonl", run_id.as_str()),
        );
    report.refresh_evidence_summary(Some(task), &[format!("Ran {visited_nodes} task agent(s).")]);
    report
}
