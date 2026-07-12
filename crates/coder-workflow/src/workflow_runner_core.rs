use std::collections::BTreeSet;

use coder_config::{resolve_workflow_cost_policy, validate_project_config};
use coder_core::{RunRequest, RunState, RunStatus, WorkflowId};
use serde_json::{json, Value};

use crate::{
    workflow_backend_execution::WorkflowBackendRunInput,
    workflow_compaction_events::ContextCompactionEventInput,
    workflow_context_projection::agent_runtime_event_summary,
    workflow_control::{
        concise_join, repair_task_from_feedback, workflow_feedback_value,
        workflow_planner_task_from_feedback, WorkflowSignal,
    },
    workflow_events::NodeOutcomeEvent,
    workflow_graph::{
        should_repair_with_executor, should_route_feedback_to_workflow_planner, WorkflowGraph,
    },
    workflow_reports::{run_status_str, workflow_run_report, WorkflowReportInput},
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
        let workflow = self
            .config
            .workflows
            .get(&options.workflow_id)
            .ok_or_else(|| WorkflowError::WorkflowNotFound(options.workflow_id.clone()))?;
        let cost_policy = resolve_workflow_cost_policy(&self.config, &options.workflow_id)
            .ok_or_else(|| {
                WorkflowError::InvalidConfig(format!(
                    "workflow '{}' has no model-backed node for cost policy resolution",
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
        let git_head = git_head(&request.repo_root);
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
                "workflow_id": &options.workflow_id,
                "task": &options.task,
                "repo_root": request.repo_root,
                "git_head": git_head,
                "dry_run": options.dry_run,
                "max_rounds": workflow.max_rounds,
                "token_budget": token_budget,
                "cost_policy": {
                    "token_budget": cost_policy.token_budget,
                    "budget_source": cost_policy.token_budget_source,
                    "model_id": cost_policy.model_id,
                    "provider": cost_policy.provider,
                    "model": cost_policy.model,
                    "default_max_turns": cost_policy.default_max_turns,
                    "max_rounds": cost_policy.max_rounds
                },
                "config_ref": config_ref,
                "plan_context": options.plan_context.clone()
            }),
        )?;

        let graph = WorkflowGraph::new(workflow)?;
        self.emit(
            &run_id,
            &mut sequence,
            "workflow.started",
            json!({
                "workflow_id": &options.workflow_id,
                "start_node_id": &graph.start_node_id,
                "max_rounds": workflow.max_rounds
            }),
        )?;

        let mut compaction_circuit_state =
            self.store.read_compaction_circuit_state(run_id.as_str())?;
        let max_rounds_limit = options
            .max_rounds_override
            .unwrap_or(workflow.max_rounds)
            .max(1)
            .min(workflow.max_rounds);
        let mut max_rounds_reached = false;
        let mut terminal_status;
        let mut terminal_reason = None;
        let mut checks = Vec::new();
        let mut blockers = Vec::new();
        let mut evidence_refs = Vec::new();
        let mut changed_files = BTreeSet::new();
        let mut patch_refs = BTreeSet::new();
        let mut current_node_id = graph.start_node_id.clone();
        let mut current_task = options.task.clone();
        let mut loop_feedback: Option<Value> = None;
        let mut planner_improvement_history: Vec<Vec<String>> = Vec::new();
        let mut executor_evidence_this_round = false;
        let mut executor_evidence_summary = String::new();
        let mut round = 1;
        let mut visited_this_round = BTreeSet::new();
        let mut control = options.control.clone();
        self.emit(
            &run_id,
            &mut sequence,
            "round.started",
            json!({"round": round, "start_node_id": &graph.start_node_id}),
        )?;

        loop {
            if wait_until_workflow_can_run(&mut control).await {
                terminal_status = RunStatus::Cancelled;
                terminal_reason = Some("cancelled by user".to_owned());
                self.emit(
                    &run_id,
                    &mut sequence,
                    "round.completed",
                    json!({"round": round, "status": "cancelled"}),
                )?;
                break;
            }
            visited_this_round.insert(current_node_id.clone());
            let node = graph.node(&current_node_id)?;
            let harness = self.config.harnesses.get(&node.harness).ok_or_else(|| {
                WorkflowError::InvalidConfig(format!(
                    "missing harness '{}' for node '{}'",
                    node.harness, node.id
                ))
            })?;
            let agent = self.config.agents.get(&node.agent).ok_or_else(|| {
                WorkflowError::InvalidConfig(format!(
                    "missing agent '{}' for node '{}'",
                    node.agent, node.id
                ))
            })?;
            let model = self.config.models.get(&agent.model).ok_or_else(|| {
                WorkflowError::InvalidConfig(format!(
                    "missing model '{}' for agent '{}'",
                    agent.model, node.agent
                ))
            })?;
            self.emit(
                &run_id,
                &mut sequence,
                "node.started",
                json!({
                    "round": round,
                    "node_id": node.id,
                    "agent": node.agent,
                    "harness": node.harness,
                    "backend": harness.backend,
                    "runtime": agent_runtime_event_summary(model, &agent.runtime)
                }),
            )?;

            compaction_circuit_state = self.record_context_compaction_circuit_outcome(
                &run_id,
                &mut sequence,
                ContextCompactionEventInput {
                    round,
                    node,
                    agent,
                    model,
                    plan_context: options.plan_context.as_ref(),
                    current_state: compaction_circuit_state.as_ref(),
                },
            )?;
            let backend_request = WorkflowBackendRunInput {
                run_id: &run_id,
                sequence: &mut sequence,
                round,
                repo_root: &request.repo_root,
                current_task: &current_task,
                workflow_id: &options.workflow_id,
                node,
                agent,
                harness,
                model,
                plan_context: options.plan_context.as_ref(),
                loop_feedback: loop_feedback.as_ref(),
                max_rounds: max_rounds_limit,
                token_budget,
                executor_evidence_this_round,
                executor_evidence_summary: &executor_evidence_summary,
                previous_planner_improvements: &planner_improvement_history,
                compaction_circuit_state: compaction_circuit_state.as_ref(),
            };
            let backend_output = if let Some(receiver) = control.as_mut() {
                tokio::select! {
                    biased;
                    _ = wait_for_workflow_cancellation(receiver) => None,
                    output = self.run_node_backend(backend_request) => Some(output),
                }
            } else {
                Some(self.run_node_backend(backend_request).await)
            };
            sequence = self.store.event_count(&run_id)? as u64 + 1;
            let Some(backend_output) = backend_output else {
                terminal_status = RunStatus::Cancelled;
                terminal_reason = Some("cancelled by user".to_owned());
                self.emit_node_outcome(
                    &run_id,
                    &mut sequence,
                    NodeOutcomeEvent {
                        round,
                        node,
                        kind: "node.cancelled",
                        status: "cancelled",
                        reason: terminal_reason.as_deref(),
                    },
                )?;
                self.emit(
                    &run_id,
                    &mut sequence,
                    "round.completed",
                    json!({"round": round, "status": "cancelled"}),
                )?;
                break;
            };
            let backend_output = backend_output?;
            let effective_backend = backend_output.effective_backend;
            let backend_result = backend_output.result;

            let node_event_ref_count = backend_result
                .events
                .iter()
                .map(|event| event.refs.len())
                .sum::<usize>();
            let mut event_changed_files = BTreeSet::new();
            let planner_improvements = backend_result
                .events
                .iter()
                .find(|event| event.kind == "planner.workflow_decision")
                .and_then(|event| event.payload.get("improvements"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .take(3)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .filter(|items| !items.is_empty());
            for backend_event in &backend_result.events {
                collect_harness_event_change_metadata(
                    backend_event,
                    &options.repo_root,
                    &mut event_changed_files,
                );
            }
            for backend_event in backend_result.events {
                self.emit_harness_event(&run_id, &mut sequence, backend_event)?;
            }

            let raw_status = backend_result.status.clone();
            let signal = WorkflowSignal::from_status(&raw_status);
            let mut node_checks = vec![format!(
                "node {} via {}: {}",
                node.id, effective_backend, raw_status
            )];
            let mut node_blockers = Vec::new();
            let mut node_evidence_refs = Vec::new();
            let mut node_changed_files = BTreeSet::new();
            let mut node_patch_refs = Vec::new();
            if let Some(report) = backend_result.report {
                node_checks.extend(report.checks);
                node_evidence_refs.extend(report.evidence_refs);
                node_blockers.extend(report.blockers);
                node_changed_files.extend(report.changed_files);
                node_patch_refs.extend(report.patch_refs);
            }
            node_changed_files.extend(event_changed_files);
            let node_has_evidence = node_event_ref_count > 0
                || !node_evidence_refs.is_empty()
                || !node_changed_files.is_empty()
                || !node_patch_refs.is_empty();
            if node.id == "executor" || agent.role == "executor" {
                if node_has_evidence {
                    executor_evidence_this_round = true;
                }
                let evidence_items = node_changed_files
                    .iter()
                    .map(|item| format!("changed: {item}"))
                    .chain(node_blockers.iter().map(|item| format!("blocker: {item}")))
                    .chain(node_checks.iter().map(|item| format!("check: {item}")))
                    .collect::<Vec<_>>();
                executor_evidence_summary = concise_join(&evidence_items, 1000);
            }
            if let Some(improvements) = planner_improvements {
                planner_improvement_history.push(improvements);
                if planner_improvement_history.len() > 3 {
                    planner_improvement_history.remove(0);
                }
            }

            let Some(signal) = signal else {
                checks.extend(node_checks);
                evidence_refs.extend(node_evidence_refs);
                changed_files.extend(node_changed_files);
                patch_refs.extend(node_patch_refs);
                blockers.extend(node_blockers);
                terminal_status = RunStatus::Failed;
                let reason = format!(
                    "node '{}' returned unsupported transition status '{}'",
                    node.id, raw_status
                );
                terminal_reason = Some(reason.clone());
                self.emit_node_outcome(
                    &run_id,
                    &mut sequence,
                    NodeOutcomeEvent {
                        round,
                        node,
                        kind: "node.failed",
                        status: &raw_status,
                        reason: Some(&reason),
                    },
                )?;
                self.emit(
                    &run_id,
                    &mut sequence,
                    "round.completed",
                    json!({"round": round, "status": run_status_str(terminal_status)}),
                )?;
                break;
            };

            self.emit_node_outcome(
                &run_id,
                &mut sequence,
                NodeOutcomeEvent {
                    round,
                    node,
                    kind: signal.node_event_kind(),
                    status: signal.as_str(),
                    reason: node_blockers.last().map(String::as_str),
                },
            )?;

            let selected_edge = graph
                .select_edge(&node.id, signal)
                .filter(|_| signal != WorkflowSignal::Blocked || node_has_evidence);

            checks.extend(node_checks.clone());
            evidence_refs.extend(node_evidence_refs);
            changed_files.extend(node_changed_files);
            patch_refs.extend(node_patch_refs);

            if let Some(edge) = selected_edge {
                let next_loop_feedback =
                    workflow_feedback_value(node, signal, &node_checks, &node_blockers);
                if matches!(signal, WorkflowSignal::Blocked | WorkflowSignal::Failed)
                    && !node_blockers.is_empty()
                {
                    checks.push(format!(
                        "recoverable {} from node {}: {}",
                        signal.as_str(),
                        node.id,
                        concise_join(&node_blockers, 240)
                    ));
                }
                self.emit(
                    &run_id,
                    &mut sequence,
                    "workflow.transition.selected",
                    json!({
                        "round": round,
                        "from": &edge.from,
                        "to": &edge.to,
                        "on": &edge.on
                    }),
                )?;
                let starts_new_round = visited_this_round.contains(&edge.to);
                if starts_new_round {
                    if round >= max_rounds_limit {
                        max_rounds_reached = true;
                        terminal_status = RunStatus::Blocked;
                        let reason =
                            "max_rounds reached before a terminal completed outcome".to_owned();
                        terminal_reason = Some(reason.clone());
                        blockers.extend(node_blockers);
                        blockers.push(reason.clone());
                        self.emit(
                            &run_id,
                            &mut sequence,
                            "round.completed",
                            json!({
                                "round": round,
                                "status": "blocked",
                                "reason": reason
                            }),
                        )?;
                        self.emit(
                            &run_id,
                            &mut sequence,
                            "workflow.max_rounds_reached",
                            json!({
                                "round": round,
                                "max_rounds": max_rounds_limit,
                                "next_node_id": &edge.to
                            }),
                        )?;
                        break;
                    }
                    self.emit(
                        &run_id,
                        &mut sequence,
                        "round.completed",
                        json!({"round": round, "status": "completed"}),
                    )?;
                    round += 1;
                    executor_evidence_this_round = false;
                    executor_evidence_summary.clear();
                    self.emit(
                        &run_id,
                        &mut sequence,
                        "round.started",
                        json!({"round": round, "start_node_id": &edge.to}),
                    )?;
                    visited_this_round.clear();
                }
                loop_feedback = Some(next_loop_feedback);
                if should_route_feedback_to_workflow_planner(&graph, edge.to.as_str()) {
                    current_task = workflow_planner_task_from_feedback(
                        &options.task,
                        node,
                        signal,
                        &node_checks,
                        &node_blockers,
                    );
                }
                if should_repair_with_executor(&graph, edge.to.as_str(), signal) {
                    current_task = repair_task_from_feedback(
                        &options.task,
                        node,
                        signal,
                        &node_checks,
                        &node_blockers,
                    );
                }
                current_node_id = edge.to.clone();
                continue;
            }

            blockers.extend(node_blockers);
            if let Some(status) = signal.terminal_status() {
                terminal_status = status;
                self.emit(
                    &run_id,
                    &mut sequence,
                    "round.completed",
                    json!({"round": round, "status": run_status_str(terminal_status)}),
                )?;
                break;
            }

            terminal_status = RunStatus::Blocked;
            let reason = format!(
                "node '{}' returned '{}' but no matching workflow transition exists",
                node.id,
                signal.as_str()
            );
            terminal_reason = Some(reason.clone());
            blockers.push(reason.clone());
            self.emit(
                &run_id,
                &mut sequence,
                "workflow.transition.missing",
                json!({
                    "round": round,
                    "node_id": node.id,
                    "on": signal.as_str(),
                    "reason": reason
                }),
            )?;
            self.emit(
                &run_id,
                &mut sequence,
                "round.completed",
                json!({"round": round, "status": "blocked"}),
            )?;
            break;
        }

        if workflow_control_is_cancelled(control.as_ref()) {
            terminal_status = RunStatus::Cancelled;
            terminal_reason = Some("cancelled by user".to_owned());
        }
        sequence = self.store.event_count(&run_id)? as u64 + 1;

        let report = workflow_run_report(WorkflowReportInput {
            run_id: &run_id,
            workflow_id: &options.workflow_id,
            request: &options.task,
            status: terminal_status,
            reason: terminal_reason.as_deref(),
            dispatched_nodes: checks.len(),
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
        let terminal_event = match terminal_status {
            RunStatus::Completed => "run.completed",
            RunStatus::Blocked => "run.blocked",
            RunStatus::Failed => "run.failed",
            RunStatus::Cancelled => "run.cancelled",
            RunStatus::Queued | RunStatus::Running => "run.failed",
        };
        self.emit(
            &run_id,
            &mut sequence,
            terminal_event,
            json!({
                "status": run_status_str(terminal_status),
                "report_ref": report_ref.clone(),
                "max_rounds_reached": max_rounds_reached
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
}

async fn wait_until_workflow_can_run(
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

async fn wait_for_workflow_cancellation(
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

fn workflow_control_is_cancelled(
    control: Option<&tokio::sync::watch::Receiver<WorkflowRunControl>>,
) -> bool {
    control.is_some_and(|receiver| *receiver.borrow() == WorkflowRunControl::Cancelled)
}
