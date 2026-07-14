use std::collections::BTreeSet;

use coder_config::{resolve_task_cost_policy, validate_project_config};
use coder_core::{RunRequest, RunState, RunStatus, WorkflowId};
use serde_json::json;

use crate::{
    workflow_backend_execution::WorkflowBackendRunInput,
    workflow_compaction_events::ContextCompactionEventInput,
    workflow_context_projection::agent_runtime_event_summary,
    workflow_events::NodeOutcomeEvent,
    workflow_reports::{run_status_str, task_run_report, TaskReportInput},
    workflow_run_types::{git_head, WorkflowRunControl, WorkflowRunOptions, WorkflowRunOutput},
    workflow_verification::collect_harness_event_change_metadata,
    WorkflowError, WorkflowRunner,
};

impl WorkflowRunner {
    pub async fn run(
        &self,
        options: WorkflowRunOptions,
    ) -> Result<WorkflowRunOutput, WorkflowError> {
        let validation = validate_project_config(&self.config);
        if !validation.is_pass() {
            return Err(WorkflowError::InvalidConfig(validation.status));
        }
        if options.task.trim().is_empty() {
            return Err(WorkflowError::InvalidConfig("task_empty".to_owned()));
        }
        let profile = self
            .config
            .task_profiles
            .get(&options.workflow_id)
            .ok_or_else(|| WorkflowError::WorkflowNotFound(options.workflow_id.clone()))?;
        let harness = self.config.harnesses.get(&profile.harness).ok_or_else(|| {
            WorkflowError::InvalidConfig(format!(
                "missing harness '{}' for task profile '{}'",
                profile.harness, options.workflow_id
            ))
        })?;
        let model = self.config.models.get(&profile.model).ok_or_else(|| {
            WorkflowError::InvalidConfig(format!(
                "missing model '{}' for task profile '{}'",
                profile.model, options.workflow_id
            ))
        })?;
        let cost_policy =
            resolve_task_cost_policy(&self.config, &options.workflow_id).ok_or_else(|| {
                WorkflowError::InvalidConfig(format!(
                    "task profile '{}' has no model",
                    options.workflow_id
                ))
            })?;
        let token_budget = Some(cost_policy.token_budget);

        let run_id = options.run_id.clone().unwrap_or_default();
        let request = RunRequest {
            repo_root: options.repo_root.display().to_string(),
            task: options.task.clone(),
            workflow_id: WorkflowId::new(options.workflow_id.clone()),
        };
        let mut state = RunState::new(run_id.clone(), request.workflow_id.clone());
        state.status = RunStatus::Running;
        self.store.write_metadata(&state)?;
        let config_ref = self
            .store
            .write_run_config_snapshot(&run_id, &self.config)?;

        let mut sequence = 1;
        self.emit(
            &run_id,
            &mut sequence,
            "run.started",
            json!({
                "task_profile_id": &options.workflow_id,
                "task": &options.task,
                "repo_root": request.repo_root,
                "git_head": git_head(&request.repo_root),
                "dry_run": options.dry_run,
                "token_budget": token_budget,
                "cost_policy": {
                    "token_budget": cost_policy.token_budget,
                    "budget_source": cost_policy.token_budget_source,
                    "model_id": cost_policy.model_id,
                    "provider": cost_policy.provider,
                    "model": cost_policy.model,
                    "default_max_turns": cost_policy.default_max_turns
                },
                "config_ref": config_ref,
                "task_context": options.task_context.clone()
            }),
        )?;
        self.emit(
            &run_id,
            &mut sequence,
            "task.started",
            json!({
                "task_profile_id": &options.workflow_id,
                "agent_id": &options.workflow_id,
                "harness_id": &profile.harness
            }),
        )?;

        let mut control = options.control.clone();
        if wait_until_task_can_run(&mut control).await {
            return self.finish_cancelled_run(
                state,
                &run_id,
                sequence,
                &options,
                "cancelled before the code task runtime started",
            );
        }

        self.emit(
            &run_id,
            &mut sequence,
            "agent.started",
            json!({
                "agent_id": options.workflow_id,
                "harness_id": profile.harness,
                "backend": harness.backend,
                "runtime": agent_runtime_event_summary(model, &profile.runtime)
            }),
        )?;
        let compaction_circuit_state = self.store.read_compaction_circuit_state(run_id.as_str())?;
        let compaction_circuit_state = self.record_context_compaction_circuit_outcome(
            &run_id,
            &mut sequence,
            ContextCompactionEventInput {
                round: 1,
                task_profile_id: &options.workflow_id,
                profile,
                model,
                task_context: options.task_context.as_ref(),
                current_state: compaction_circuit_state.as_ref(),
            },
        )?;
        let backend_request = WorkflowBackendRunInput {
            run_id: &run_id,
            sequence: &mut sequence,
            round: 1,
            repo_root: &request.repo_root,
            current_task: &options.task,
            workflow_id: &options.workflow_id,
            profile,
            harness,
            model,
            task_context: options.task_context.as_ref(),
            token_budget,
            compaction_circuit_state: compaction_circuit_state.as_ref(),
        };
        let backend_output = if let Some(receiver) = control.as_mut() {
            tokio::select! {
                biased;
                _ = wait_for_task_cancellation(receiver) => None,
                output = self.run_node_backend(backend_request) => Some(output),
            }
        } else {
            Some(self.run_node_backend(backend_request).await)
        };
        sequence = self.store.event_count(&run_id)? as u64 + 1;
        let Some(backend_output) = backend_output else {
            return self.finish_cancelled_run(
                state,
                &run_id,
                sequence,
                &options,
                "cancelled while the code task runtime was running",
            );
        };
        let backend_output = backend_output?;
        let effective_backend = backend_output.effective_backend;
        let backend_result = backend_output.result;

        let mut event_changed_files = BTreeSet::new();
        let event_ref_count = backend_result
            .events
            .iter()
            .map(|event| event.refs.len())
            .sum::<usize>();
        for event in &backend_result.events {
            collect_harness_event_change_metadata(
                event,
                &options.repo_root,
                &mut event_changed_files,
            );
        }
        for event in backend_result.events {
            self.emit_harness_event(&run_id, &mut sequence, event)?;
        }

        let raw_status = backend_result.status;
        let mut checks = vec![format!(
            "task profile {} via {}: {}",
            options.workflow_id, effective_backend, raw_status
        )];
        let mut blockers = Vec::new();
        let mut evidence_refs = Vec::new();
        let mut changed_files = event_changed_files;
        let mut patch_refs = BTreeSet::new();
        if let Some(report) = backend_result.report {
            checks.extend(report.checks);
            blockers.extend(report.blockers);
            evidence_refs.extend(report.evidence_refs);
            changed_files.extend(report.changed_files);
            patch_refs.extend(report.patch_refs);
        }
        let has_evidence = event_ref_count > 0
            || !evidence_refs.is_empty()
            || !changed_files.is_empty()
            || !patch_refs.is_empty();
        checks.push(format!("task evidence present: {has_evidence}"));

        let (mut terminal_status, mut terminal_reason, node_event_kind) = match raw_status.as_str()
        {
            "completed" | "finish" => (RunStatus::Completed, None, "node.completed"),
            "blocked" => (
                RunStatus::Blocked,
                blockers.first().cloned(),
                "node.blocked",
            ),
            "failed" => (RunStatus::Failed, blockers.first().cloned(), "node.failed"),
            "cancelled" => (
                RunStatus::Cancelled,
                Some("cancelled by user".to_owned()),
                "node.cancelled",
            ),
            unsupported => {
                let reason = format!(
                    "code task runtime returned unsupported terminal status '{unsupported}'"
                );
                blockers.push(reason.clone());
                (RunStatus::Failed, Some(reason), "node.failed")
            }
        };
        if task_control_is_cancelled(control.as_ref()) {
            terminal_status = RunStatus::Cancelled;
            terminal_reason = Some("cancelled by user".to_owned());
        }
        self.emit_node_outcome(
            &run_id,
            &mut sequence,
            NodeOutcomeEvent {
                round: 1,
                task_profile_id: &options.workflow_id,
                kind: node_event_kind,
                status: run_status_str(terminal_status),
                reason: terminal_reason.as_deref(),
            },
        )?;

        let report = task_run_report(TaskReportInput {
            run_id: &run_id,
            task_profile_id: &options.workflow_id,
            request: &options.task,
            status: terminal_status,
            reason: terminal_reason.as_deref(),
            agent_runs: 1,
            checks,
            evidence_refs,
            blockers,
            changed_files: changed_files.into_iter().collect(),
            patch_refs: patch_refs.into_iter().collect(),
        });
        let report_ref = self.store.write_report(&run_id, &report)?;
        self.emit(
            &run_id,
            &mut sequence,
            "report.created",
            json!({"report_ref": report_ref.clone()}),
        )?;
        self.emit(
            &run_id,
            &mut sequence,
            terminal_event(terminal_status),
            json!({
                "status": run_status_str(terminal_status),
                "report_ref": report_ref.clone()
            }),
        )?;
        state.status = terminal_status;
        state.updated_at = time::OffsetDateTime::now_utc();
        self.store.write_metadata(&state)?;
        Ok(WorkflowRunOutput {
            run_id,
            report,
            report_ref,
        })
    }

    fn finish_cancelled_run(
        &self,
        mut state: RunState,
        run_id: &coder_core::RunId,
        mut sequence: u64,
        options: &WorkflowRunOptions,
        reason: &str,
    ) -> Result<WorkflowRunOutput, WorkflowError> {
        let report = task_run_report(TaskReportInput {
            run_id,
            task_profile_id: &options.workflow_id,
            request: &options.task,
            status: RunStatus::Cancelled,
            reason: Some(reason),
            agent_runs: 0,
            checks: Vec::new(),
            evidence_refs: Vec::new(),
            blockers: Vec::new(),
            changed_files: Vec::new(),
            patch_refs: Vec::new(),
        });
        let report_ref = self.store.write_report(run_id, &report)?;
        self.emit(
            run_id,
            &mut sequence,
            "report.created",
            json!({"report_ref": report_ref.clone()}),
        )?;
        self.emit(
            run_id,
            &mut sequence,
            "run.cancelled",
            json!({"status": "cancelled", "report_ref": report_ref.clone()}),
        )?;
        state.status = RunStatus::Cancelled;
        state.updated_at = time::OffsetDateTime::now_utc();
        self.store.write_metadata(&state)?;
        Ok(WorkflowRunOutput {
            run_id: run_id.clone(),
            report,
            report_ref,
        })
    }
}

fn terminal_event(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Completed => "run.completed",
        RunStatus::Blocked => "run.blocked",
        RunStatus::Failed | RunStatus::Queued | RunStatus::Running => "run.failed",
        RunStatus::Cancelled => "run.cancelled",
    }
}

async fn wait_until_task_can_run(
    control: &mut Option<tokio::sync::watch::Receiver<WorkflowRunControl>>,
) -> bool {
    let Some(receiver) = control.as_mut() else {
        return false;
    };
    loop {
        match *receiver.borrow() {
            WorkflowRunControl::Running => return false,
            WorkflowRunControl::Cancelled => return true,
            WorkflowRunControl::Paused => {}
        }
        if receiver.changed().await.is_err() {
            return false;
        }
    }
}

async fn wait_for_task_cancellation(
    receiver: &mut tokio::sync::watch::Receiver<WorkflowRunControl>,
) {
    loop {
        if *receiver.borrow() == WorkflowRunControl::Cancelled {
            return;
        }
        if receiver.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

fn task_control_is_cancelled(
    control: Option<&tokio::sync::watch::Receiver<WorkflowRunControl>>,
) -> bool {
    control.is_some_and(|receiver| *receiver.borrow() == WorkflowRunControl::Cancelled)
}
