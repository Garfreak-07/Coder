use coder_config::{validate_project_config, ProjectConfig, WorkflowSpec};
use coder_core::{FinalReport, RunId, RunRequest, RunState, RunStatus, WorkflowId};
use coder_events::CoderEvent;
use coder_store::{RunStore, StoreError};
use serde_json::json;
use thiserror::Error;

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
            .workflows
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
                "workflow_id": workflow_id,
                "task": task,
                "max_rounds": workflow.max_rounds
            }),
        )?;
        sequence += 1;

        let requested_rounds = options.requested_rounds.max(1);
        let rounds_to_run = requested_rounds.min(workflow.max_rounds);
        let max_rounds_reached = requested_rounds > workflow.max_rounds;

        for round in 1..=rounds_to_run {
            self.emit(&run_id, sequence, "round.started", json!({"round": round}))?;
            sequence += 1;

            for node in &workflow.nodes {
                self.emit(
                    &run_id,
                    sequence,
                    "node.started",
                    json!({
                        "round": round,
                        "node_id": node.id,
                        "agent": node.agent,
                        "harness": node.harness
                    }),
                )?;
                sequence += 1;
                self.emit(
                    &run_id,
                    sequence,
                    "node.completed",
                    json!({
                        "round": round,
                        "node_id": node.id,
                        "status": "completed",
                        "mock": true
                    }),
                )?;
                sequence += 1;
            }

            self.emit(
                &run_id,
                sequence,
                "round.completed",
                json!({"round": round, "status": "completed"}),
            )?;
            sequence += 1;
        }

        let outcome = if max_rounds_reached {
            MockRunOutcome::Blocked
        } else {
            options.outcome
        };
        let report = report_for_mock_run(
            &run_id,
            workflow_id,
            workflow,
            task,
            rounds_to_run,
            outcome,
            max_rounds_reached,
        );
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
                "report_ref": report_ref,
                "max_rounds_reached": max_rounds_reached
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
    pub requested_rounds: u32,
}

impl Default for MockRunOptions {
    fn default() -> Self {
        Self {
            outcome: MockRunOutcome::Completed,
            requested_rounds: 1,
        }
    }
}

#[derive(Debug)]
pub struct MockRunOutput {
    pub run_id: RunId,
    pub report: FinalReport,
    pub report_ref: String,
}

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("workflow not found: {0}")]
    WorkflowNotFound(String),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
}

fn report_for_mock_run(
    run_id: &RunId,
    workflow_id: &str,
    workflow: &WorkflowSpec,
    task: &str,
    rounds: u32,
    outcome: MockRunOutcome,
    max_rounds_reached: bool,
) -> FinalReport {
    let visited_nodes = workflow.nodes.len() as u32 * rounds;
    let summary = format!(
        "Mock workflow '{workflow_id}' accepted task '{task}' and visited {visited_nodes} node(s) across {rounds} round(s)."
    );
    let report = match outcome {
        MockRunOutcome::Completed => FinalReport::completed(summary),
        MockRunOutcome::Blocked => FinalReport::blocked(
            summary,
            if max_rounds_reached {
                "max_rounds reached before a terminal completed outcome"
            } else {
                "mock run requested blocked outcome"
            },
        ),
        MockRunOutcome::Failed => FinalReport::failed(summary, "mock run requested failed outcome"),
    };
    report
        .with_check(format!("mock node visits: {visited_nodes}"))
        .with_evidence(
            "event_log",
            format!("eventlog://runs/{}/events.jsonl", run_id.as_str()),
        )
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use coder_config::ProjectConfig;
    use coder_core::ReportStatus;

    use super::*;

    #[test]
    fn mock_runner_writes_jsonl_events_and_report() {
        let (config, root, store) = fixture();
        let runner = MockWorkflowRunner::new(&config, store.clone());

        let output = runner.run("planner-led", "summarize the repo").unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        assert_eq!(events.first().unwrap().kind, "run.started");
        assert_eq!(events.last().unwrap().kind, "run.completed");
        assert!(output.report_ref.contains("final-report.json"));
        assert_eq!(output.report.status, ReportStatus::Completed);
        assert_eq!(output.report.evidence_refs[0].kind, "event_log");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn mock_runner_can_finish_blocked() {
        let (config, root, store) = fixture();
        let runner = MockWorkflowRunner::new(&config, store.clone());

        let output = runner
            .run_with_options(
                "planner-led",
                "blocked task",
                MockRunOptions {
                    outcome: MockRunOutcome::Blocked,
                    requested_rounds: 1,
                },
            )
            .unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        assert_eq!(events.last().unwrap().kind, "run.blocked");
        assert_eq!(output.report.status, ReportStatus::Blocked);
        assert_eq!(
            output.report.blockers[0],
            "mock run requested blocked outcome"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn mock_runner_can_finish_failed() {
        let (config, root, store) = fixture();
        let runner = MockWorkflowRunner::new(&config, store.clone());

        let output = runner
            .run_with_options(
                "planner-led",
                "failed task",
                MockRunOptions {
                    outcome: MockRunOutcome::Failed,
                    requested_rounds: 1,
                },
            )
            .unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        assert_eq!(events.last().unwrap().kind, "run.failed");
        assert_eq!(output.report.status, ReportStatus::Failed);
        assert_eq!(
            output.report.blockers[0],
            "mock run requested failed outcome"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn mock_runner_blocks_when_requested_rounds_exceed_max_rounds() {
        let (config, root, store) = fixture();
        let runner = MockWorkflowRunner::new(&config, store.clone());

        let output = runner
            .run_with_options(
                "planner-led",
                "too many rounds",
                MockRunOptions {
                    outcome: MockRunOutcome::Completed,
                    requested_rounds: 99,
                },
            )
            .unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        assert_eq!(events.last().unwrap().kind, "run.blocked");
        assert_eq!(
            output.report.blockers[0],
            "max_rounds reached before a terminal completed outcome"
        );
        assert!(
            events
                .iter()
                .filter(|event| event.kind == "round.started")
                .count()
                <= 3
        );
        let _ = fs::remove_dir_all(root);
    }

    fn fixture() -> (ProjectConfig, PathBuf, RunStore) {
        let config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("coder-workflow-{}-{}", std::process::id(), id));
        let store = RunStore::new(&root);
        (config, root, store)
    }
}
