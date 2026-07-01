use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use coder_config::{
    validate_project_config, AgentSpec, HarnessSpec, ModelSpec,
    OpenHandsApiPaths as ConfigOpenHandsApiPaths,
    OpenHandsAuthHeaderMode as ConfigOpenHandsAuthHeaderMode, OpenHandsHarnessConfig,
    OpenHandsRunStartStrategy as ConfigOpenHandsRunStartStrategy, ProjectConfig, WorkflowEdgeSpec,
    WorkflowNodeSpec, WorkflowSpec,
};
use coder_core::{FinalReport, ReportStatus, RunId, RunRequest, RunState, RunStatus, WorkflowId};
use coder_events::CoderEvent;
use coder_harness::{
    HarnessBackend, HarnessError, HarnessRunEvent, HarnessRunRequest, HarnessRunResult,
};
use coder_openhands::{
    normalize_openhands_event, openhands_final_report, openhands_raw_event_kind, OpenHandsApiPaths,
    OpenHandsAuthHeaderMode, OpenHandsClient, OpenHandsError, OpenHandsRunStartStrategy,
    OpenHandsServerConfig,
};
use coder_store::{RepoEvidenceKind, RepoEvidenceRef, RunStore, StoreError};
use coder_tools::{
    apply_patch_file, find_files, git_diff, git_status, preview_command, preview_patch_file,
    read_file, read_file_range, run_command, search_text, CommandRunRequest,
    PatchApplyRequest as ToolPatchApplyRequest, RepoToolConfig,
};
use serde_json::{json, Value};
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
    #[error("backend not found: {0}")]
    BackendNotFound(String),
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
    let mut report = match outcome {
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
    report = report
        .with_check(format!("mock node visits: {visited_nodes}"))
        .with_evidence(
            "event_log",
            format!("eventlog://runs/{}/events.jsonl", run_id.as_str()),
        );
    report.refresh_planner_style_summary(
        Some(task),
        &[format!(
            "Visited {visited_nodes} node(s) across {rounds} round(s)."
        )],
    );
    report
}

pub struct WorkflowRunner {
    config: ProjectConfig,
    store: RunStore,
    backends: BackendRegistry,
}

impl WorkflowRunner {
    pub fn new(config: ProjectConfig, store: RunStore) -> Self {
        let backends = BackendRegistry::from_project_config(&config, store.clone());
        Self {
            config,
            store,
            backends,
        }
    }

    pub fn with_registry(
        config: ProjectConfig,
        store: RunStore,
        backends: BackendRegistry,
    ) -> Self {
        Self {
            config,
            store,
            backends,
        }
    }

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

        let run_id = RunId::new();
        let request = RunRequest {
            repo_root: options.repo_root.display().to_string(),
            task: options.task.clone(),
            workflow_id: WorkflowId::new(options.workflow_id.clone()),
        };
        let mut state = RunState::new(run_id.clone(), request.workflow_id.clone());
        state.status = RunStatus::Running;
        self.store.write_metadata(&state)?;

        let mut sequence = 1;
        self.emit(
            &run_id,
            &mut sequence,
            "run.started",
            json!({
                "workflow_id": &options.workflow_id,
                "task": &options.task,
                "repo_root": request.repo_root,
                "dry_run": options.dry_run,
                "max_rounds": workflow.max_rounds,
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

        let max_rounds_limit = options
            .max_rounds_override
            .unwrap_or(workflow.max_rounds)
            .max(1)
            .min(workflow.max_rounds);
        let mut max_rounds_reached = false;
        let terminal_status;
        let mut terminal_reason = None;
        let mut checks = Vec::new();
        let mut blockers = Vec::new();
        let mut evidence_refs = Vec::new();
        let mut changed_files = BTreeSet::new();
        let mut patch_refs = BTreeSet::new();
        let mut current_node_id = graph.start_node_id.clone();
        let mut round = 1;
        self.emit(
            &run_id,
            &mut sequence,
            "round.started",
            json!({"round": round, "start_node_id": &graph.start_node_id}),
        )?;

        loop {
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
                    "backend": harness.backend
                }),
            )?;

            let requested_backend = harness.backend.clone();
            let fallback_allowed =
                requested_backend == "openhands" && options.allow_native_fallback_for_openhands;
            let mut effective_backend = requested_backend.clone();
            let mut effective_harness = harness.clone();
            let mut backend = match self.backends.backend_for(&requested_backend) {
                Some(backend) => {
                    self.emit_backend_selected(
                        &run_id,
                        &mut sequence,
                        round,
                        node,
                        &requested_backend,
                        &requested_backend,
                        None,
                        fallback_allowed,
                    )?;
                    backend
                }
                None if fallback_allowed => {
                    self.emit_backend_blocked(
                        &run_id,
                        &mut sequence,
                        round,
                        node,
                        &requested_backend,
                        "OpenHands backend is not configured for this run.",
                        true,
                    )?;
                    effective_backend = "native-rust".to_owned();
                    effective_harness = native_fallback_harness(harness);
                    self.emit_backend_selected(
                        &run_id,
                        &mut sequence,
                        round,
                        node,
                        &effective_backend,
                        &requested_backend,
                        Some(&requested_backend),
                        true,
                    )?;
                    self.backends
                        .backend_for(&effective_backend)
                        .ok_or_else(|| WorkflowError::BackendNotFound(effective_backend.clone()))?
                }
                None => return Err(WorkflowError::BackendNotFound(requested_backend)),
            };

            let mut backend_result = match backend
                .run(build_harness_run_request(
                    &run_id,
                    &request.repo_root,
                    &options,
                    workflow,
                    node,
                    agent,
                    &effective_harness,
                    model,
                ))
                .await
            {
                Ok(result) => result,
                Err(HarnessError::Unavailable(message))
                    if requested_backend == "openhands" && fallback_allowed =>
                {
                    self.emit_backend_blocked(
                        &run_id,
                        &mut sequence,
                        round,
                        node,
                        &requested_backend,
                        &message,
                        true,
                    )?;
                    effective_backend = "native-rust".to_owned();
                    effective_harness = native_fallback_harness(harness);
                    backend = self
                        .backends
                        .backend_for(&effective_backend)
                        .ok_or_else(|| WorkflowError::BackendNotFound(effective_backend.clone()))?;
                    self.emit_backend_selected(
                        &run_id,
                        &mut sequence,
                        round,
                        node,
                        &effective_backend,
                        &requested_backend,
                        Some(&requested_backend),
                        true,
                    )?;
                    match backend
                        .run(build_harness_run_request(
                            &run_id,
                            &request.repo_root,
                            &options,
                            workflow,
                            node,
                            agent,
                            &effective_harness,
                            model,
                        ))
                        .await
                    {
                        Ok(result) => result,
                        Err(HarnessError::Unavailable(message)) => HarnessRunResult::blocked(
                            format!("backend 'native-rust' unavailable: {message}"),
                        ),
                        Err(error) => HarnessRunResult {
                            status: "failed".to_owned(),
                            report: Some(FinalReport::failed(
                                "Harness backend failed.",
                                error.to_string(),
                            )),
                            events: vec![HarnessRunEvent::new(
                                "backend.failed",
                                json!({
                                    "backend": "native-rust",
                                    "requested_backend": requested_backend,
                                    "error": error.to_string()
                                }),
                            )],
                        },
                    }
                }
                Err(HarnessError::Unavailable(message)) => {
                    self.emit_backend_blocked(
                        &run_id,
                        &mut sequence,
                        round,
                        node,
                        &requested_backend,
                        &message,
                        false,
                    )?;
                    HarnessRunResult::blocked(format!(
                        "backend '{}' unavailable: {message}",
                        requested_backend
                    ))
                }
                Err(error) => HarnessRunResult {
                    status: "failed".to_owned(),
                    report: Some(FinalReport::failed(
                        "Harness backend failed.",
                        error.to_string(),
                    )),
                    events: vec![HarnessRunEvent::new(
                        "backend.failed",
                        json!({
                            "backend": effective_backend,
                            "requested_backend": requested_backend,
                            "error": error.to_string()
                        }),
                    )],
                },
            };
            if effective_backend != requested_backend && backend_result.report.is_none() {
                backend_result.report = Some(FinalReport::completed(format!(
                    "Native fallback handled executor backend request for '{requested_backend}'."
                )));
            }

            for backend_event in backend_result.events {
                self.emit_harness_event(&run_id, &mut sequence, backend_event)?;
            }

            let raw_status = backend_result.status;
            let signal = WorkflowSignal::from_status(&raw_status);
            checks.push(format!(
                "node {} via {}: {}",
                node.id, effective_backend, raw_status
            ));
            if let Some(report) = backend_result.report {
                evidence_refs.extend(report.evidence_refs);
                blockers.extend(report.blockers);
                changed_files.extend(report.changed_files);
                patch_refs.extend(report.patch_refs);
            }

            let Some(signal) = signal else {
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
                    reason: blockers.last().map(String::as_str),
                },
            )?;

            if let Some(edge) = graph.select_edge(&node.id, signal) {
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
                if edge.to.as_str() == graph.start_node_id.as_str() {
                    if round >= max_rounds_limit {
                        max_rounds_reached = true;
                        terminal_status = RunStatus::Blocked;
                        let reason =
                            "max_rounds reached before a terminal completed outcome".to_owned();
                        terminal_reason = Some(reason.clone());
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
                    self.emit(
                        &run_id,
                        &mut sequence,
                        "round.started",
                        json!({"round": round, "start_node_id": &graph.start_node_id}),
                    )?;
                }
                current_node_id = edge.to.clone();
                continue;
            }

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
            plan_context: options.plan_context.clone(),
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

    fn emit(
        &self,
        run_id: &RunId,
        sequence: &mut u64,
        kind: &str,
        payload: Value,
    ) -> Result<(), WorkflowError> {
        let event = CoderEvent::new(run_id.clone(), *sequence, kind, payload);
        self.store.append_event(run_id, &event)?;
        *sequence += 1;
        Ok(())
    }

    fn emit_harness_event(
        &self,
        run_id: &RunId,
        sequence: &mut u64,
        backend_event: HarnessRunEvent,
    ) -> Result<(), WorkflowError> {
        let mut event = CoderEvent::new(
            run_id.clone(),
            *sequence,
            backend_event.kind,
            backend_event.payload,
        );
        for reference in backend_event.refs {
            event = event.with_ref(reference.label, reference.uri);
        }
        self.store.append_event(run_id, &event)?;
        *sequence += 1;
        Ok(())
    }

    fn emit_backend_selected(
        &self,
        run_id: &RunId,
        sequence: &mut u64,
        round: u32,
        node: &WorkflowNodeSpec,
        backend: &str,
        requested_backend: &str,
        fallback_for: Option<&str>,
        fallback_allowed: bool,
    ) -> Result<(), WorkflowError> {
        let mut payload = json!({
            "round": round,
            "node_id": node.id,
            "agent_id": node.agent,
            "harness_id": node.harness,
            "backend": backend,
            "requested_backend": requested_backend,
            "status": "selected",
            "fallback_allowed": fallback_allowed,
            "summary": backend_selection_summary(backend, fallback_for)
        });
        if let Some(fallback_for) = fallback_for {
            payload["fallback_for"] = json!(fallback_for);
        }
        self.emit(run_id, sequence, "backend.selected", payload)
    }

    fn emit_backend_blocked(
        &self,
        run_id: &RunId,
        sequence: &mut u64,
        round: u32,
        node: &WorkflowNodeSpec,
        backend: &str,
        reason: &str,
        fallback_allowed: bool,
    ) -> Result<(), WorkflowError> {
        self.emit(
            run_id,
            sequence,
            "backend.blocked",
            json!({
                "round": round,
                "node_id": node.id,
                "agent_id": node.agent,
                "harness_id": node.harness,
                "backend": backend,
                "status": "blocked",
                "fallback_allowed": fallback_allowed,
                "reason": reason,
                "summary": backend_blocked_summary(backend, reason, fallback_allowed)
            }),
        )
    }

    fn emit_node_outcome(
        &self,
        run_id: &RunId,
        sequence: &mut u64,
        outcome: NodeOutcomeEvent<'_>,
    ) -> Result<(), WorkflowError> {
        let mut payload = json!({
            "round": outcome.round,
            "node_id": outcome.node.id,
            "status": outcome.status
        });
        if let Some(reason) = outcome.reason {
            payload["reason"] = json!(reason);
        }
        self.emit(run_id, sequence, outcome.kind, payload)
    }
}

struct NodeOutcomeEvent<'a> {
    round: u32,
    node: &'a WorkflowNodeSpec,
    kind: &'a str,
    status: &'a str,
    reason: Option<&'a str>,
}

#[derive(Debug)]
struct WorkflowGraph<'a> {
    start_node_id: String,
    nodes: BTreeMap<&'a str, &'a WorkflowNodeSpec>,
    edges: Vec<&'a WorkflowEdgeSpec>,
}

impl<'a> WorkflowGraph<'a> {
    fn new(workflow: &'a WorkflowSpec) -> Result<Self, WorkflowError> {
        let start_node_id = workflow
            .nodes
            .first()
            .map(|node| node.id.clone())
            .ok_or_else(|| {
                WorkflowError::InvalidConfig("workflow_start_node_missing".to_owned())
            })?;
        let mut seen = BTreeSet::new();
        let mut nodes = BTreeMap::new();
        for node in &workflow.nodes {
            if !seen.insert(node.id.as_str()) {
                return Err(WorkflowError::InvalidConfig(format!(
                    "duplicate workflow node '{}'",
                    node.id
                )));
            }
            nodes.insert(node.id.as_str(), node);
        }
        for edge in &workflow.edges {
            if !nodes.contains_key(edge.from.as_str()) {
                return Err(WorkflowError::InvalidConfig(format!(
                    "workflow edge source '{}' does not exist",
                    edge.from
                )));
            }
            if !nodes.contains_key(edge.to.as_str()) {
                return Err(WorkflowError::InvalidConfig(format!(
                    "workflow edge target '{}' does not exist",
                    edge.to
                )));
            }
        }
        Ok(Self {
            start_node_id,
            nodes,
            edges: workflow.edges.iter().collect(),
        })
    }

    fn node(&self, node_id: &str) -> Result<&'a WorkflowNodeSpec, WorkflowError> {
        self.nodes.get(node_id).copied().ok_or_else(|| {
            WorkflowError::InvalidConfig(format!("workflow node '{node_id}' does not exist"))
        })
    }

    fn select_edge(&self, node_id: &str, signal: WorkflowSignal) -> Option<&'a WorkflowEdgeSpec> {
        self.edges
            .iter()
            .copied()
            .find(|edge| edge.from == node_id && edge.on == signal.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkflowSignal {
    Ready,
    Completed,
    Blocked,
    Failed,
    Cancelled,
    Continue,
    Finish,
}

impl WorkflowSignal {
    fn from_status(status: &str) -> Option<Self> {
        match status {
            "ready" => Some(Self::Ready),
            "completed" => Some(Self::Completed),
            "blocked" => Some(Self::Blocked),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "continue" => Some(Self::Continue),
            "finish" => Some(Self::Finish),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Continue => "continue",
            Self::Finish => "finish",
        }
    }

    fn node_event_kind(self) -> &'static str {
        match self {
            Self::Ready | Self::Completed | Self::Continue | Self::Finish => "node.completed",
            Self::Blocked => "node.blocked",
            Self::Failed => "node.failed",
            Self::Cancelled => "node.cancelled",
        }
    }

    fn terminal_status(self) -> Option<RunStatus> {
        match self {
            Self::Completed | Self::Finish => Some(RunStatus::Completed),
            Self::Blocked => Some(RunStatus::Blocked),
            Self::Failed => Some(RunStatus::Failed),
            Self::Cancelled => Some(RunStatus::Cancelled),
            Self::Ready | Self::Continue => None,
        }
    }
}

pub fn replay_run_status(events: &[CoderEvent]) -> Option<RunStatus> {
    events
        .iter()
        .rev()
        .find_map(|event| match event.kind.as_str() {
            "run.completed" => Some(RunStatus::Completed),
            "run.blocked" => Some(RunStatus::Blocked),
            "run.failed" => Some(RunStatus::Failed),
            "run.cancelled" => Some(RunStatus::Cancelled),
            _ => None,
        })
}

pub struct OpenHandsConversationPayloadInput<'a> {
    pub run_id: &'a RunId,
    pub repo_root: &'a str,
    pub workflow_id: &'a str,
    pub workflow: &'a WorkflowSpec,
    pub node: &'a WorkflowNodeSpec,
    pub agent_id: &'a str,
    pub agent: &'a AgentSpec,
    pub harness_id: &'a str,
    pub harness: &'a HarnessSpec,
    pub model: &'a ModelSpec,
    pub plan_context: Option<&'a Value>,
}

pub fn build_openhands_conversation_payload(input: OpenHandsConversationPayloadInput<'_>) -> Value {
    let model = model_reference(input.agent, input.model);
    let memory = memory_scope_summary(input.agent, input.harness);
    let permissions = permission_summary(input.harness);
    let verification = serde_json::to_value(&input.harness.verification).unwrap_or(Value::Null);
    let coder_metadata = json!({
        "contract": "coder.openhands.conversation.v1",
        "source": "coder-workflow",
        "agent_kind_source": "default_fallback",
        "run_id": input.run_id.as_str(),
        "repo_root": input.repo_root,
        "workflow_id": input.workflow_id,
        "workflow_name": &input.workflow.name,
        "node_id": &input.node.id,
        "agent_id": input.agent_id,
        "harness_id": input.harness_id,
        "selected_tools": &input.harness.tools,
        "model": model,
        "memory": memory,
        "permissions": permissions,
        "verification": verification,
        "output_contract": &input.agent.output_contract,
        "plan_context": input.plan_context.cloned().unwrap_or(Value::Null)
    });

    json!({
        "workspace": {
            "kind": "LocalWorkspace",
            "working_dir": input.repo_root
        },
        "agent": openhands_agent_payload(input.agent, input.model, input.harness),
        "metadata": {
            "source": "coder-workflow",
            "coder": coder_metadata
        },
        "coder_context": {
            "contract": "coder.openhands.context.v1",
            "run": {
                "run_id": input.run_id.as_str(),
                "repo_root": input.repo_root
            },
            "workflow": {
                "workflow_id": input.workflow_id,
                "name": &input.workflow.name,
                "max_rounds": input.workflow.max_rounds,
                "stop": &input.workflow.stop,
                "node_id": &input.node.id
            },
            "agent": {
                "agent_id": input.agent_id,
                "role": &input.agent.role,
                "model_ref": &input.agent.model,
                "output_contract": &input.agent.output_contract,
                "system_instructions": &input.agent.system
            },
            "harness": {
                "harness_id": input.harness_id,
                "backend": &input.harness.backend,
                "selected_tools": &input.harness.tools,
                "permissions": serde_json::to_value(&input.harness.permissions).unwrap_or(Value::Null),
                "memory": serde_json::to_value(&input.harness.memory).unwrap_or(Value::Null),
                "verification": serde_json::to_value(&input.harness.verification).unwrap_or(Value::Null)
            },
            "model": model_reference(input.agent, input.model),
            "memory": memory_scope_summary(input.agent, input.harness),
            "output_contract": {
                "name": &input.agent.output_contract,
                "require_evidence": input.harness.verification.require_evidence
            },
            "plan_context": input.plan_context.cloned().unwrap_or(Value::Null)
        }
    })
}

fn openhands_agent_payload(agent: &AgentSpec, model: &ModelSpec, harness: &HarnessSpec) -> Value {
    let mut llm = serde_json::Map::new();
    llm.insert(
        "model".to_owned(),
        Value::String(openhands_llm_model_name(model)),
    );
    llm.insert(
        "usage_id".to_owned(),
        Value::String(format!("coder-{}", agent.role)),
    );
    if let Some(base_url) = model
        .base_url_env
        .as_deref()
        .and_then(|name| env::var(name).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        llm.insert("base_url".to_owned(), Value::String(base_url));
    }
    if let Some(api_key) = model
        .api_key_env
        .as_deref()
        .and_then(|name| env::var(name).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        llm.insert("api_key".to_owned(), Value::String(api_key));
    }

    json!({
        "kind": "Agent",
        "llm": Value::Object(llm),
        "tools": openhands_tool_payloads(&harness.tools),
        "include_default_tools": ["FinishTool", "ThinkTool"],
        "agent_context": {
            "system_message_suffix": agent.system
        }
    })
}

fn openhands_llm_model_name(model: &ModelSpec) -> String {
    if model.provider == "openai-compatible" && !model.model.contains('/') {
        format!("openai/{}", model.model)
    } else {
        model.model.clone()
    }
}

fn openhands_tool_payloads(tools: &[String]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|tool| match tool.as_str() {
            "terminal" => Some("terminal"),
            "file_editor" => Some("file_editor"),
            "task_tracker" => Some("task_tracker"),
            _ => None,
        })
        .map(|name| json!({ "name": name, "params": {} }))
        .collect()
}

fn harness_backend_context(input: OpenHandsConversationPayloadInput<'_>) -> Value {
    let coder = json!({
        "workflow_id": input.workflow_id,
        "node_id": input.node.id,
        "agent_id": input.agent_id,
        "harness_id": input.harness_id,
        "agent": {
            "role": &input.agent.role,
            "model": &input.agent.model,
            "output_contract": &input.agent.output_contract
        },
        "harness": {
            "backend": &input.harness.backend,
            "selected_tools": &input.harness.tools,
            "permissions": serde_json::to_value(&input.harness.permissions).unwrap_or(Value::Null),
            "memory": serde_json::to_value(&input.harness.memory).unwrap_or(Value::Null),
            "verification": serde_json::to_value(&input.harness.verification).unwrap_or(Value::Null)
        },
        "model": model_reference(input.agent, input.model),
        "memory": memory_scope_summary(input.agent, input.harness),
        "permissions": permission_summary(input.harness),
        "plan_context": input.plan_context.cloned().unwrap_or(Value::Null)
    });
    if input.harness.backend == "openhands" {
        json!({
            "coder": coder,
            "openhands": {
                "create_conversation_payload": build_openhands_conversation_payload(input)
            }
        })
    } else {
        json!({ "coder": coder })
    }
}

fn build_harness_run_request(
    run_id: &RunId,
    repo_root: &str,
    options: &WorkflowRunOptions,
    workflow: &WorkflowSpec,
    node: &WorkflowNodeSpec,
    agent: &AgentSpec,
    harness: &HarnessSpec,
    model: &ModelSpec,
) -> HarnessRunRequest {
    HarnessRunRequest {
        run_id: run_id.clone(),
        workflow_id: options.workflow_id.clone(),
        node_id: node.id.clone(),
        agent_id: node.agent.clone(),
        harness_id: node.harness.clone(),
        repo_root: options.repo_root.display().to_string(),
        task: options.task.clone(),
        backend_context: harness_backend_context(OpenHandsConversationPayloadInput {
            run_id,
            repo_root,
            workflow_id: &options.workflow_id,
            workflow,
            node,
            agent_id: &node.agent,
            agent,
            harness_id: &node.harness,
            harness,
            model,
            plan_context: options.plan_context.as_ref(),
        }),
    }
}

fn native_fallback_harness(harness: &HarnessSpec) -> HarnessSpec {
    let mut fallback = harness.clone();
    fallback.backend = "native-rust".to_owned();
    fallback.openhands = None;
    fallback.tools.clear();
    fallback
}

fn backend_selection_summary(backend: &str, fallback_for: Option<&str>) -> String {
    if backend == "native-rust" && fallback_for == Some("openhands") {
        return "Executor backend: native fallback".to_owned();
    }
    format!("Executor backend: {}", backend_display_name(backend))
}

fn backend_blocked_summary(backend: &str, reason: &str, fallback_allowed: bool) -> String {
    if backend == "openhands" {
        let suffix = if fallback_allowed {
            " Native fallback is enabled."
        } else {
            ""
        };
        return format!(
            "Executor backend: blocked - OpenHands not reachable. {}{}",
            reason, suffix
        );
    }
    format!(
        "Executor backend: blocked - {}. {}",
        backend_display_name(backend),
        reason
    )
}

fn backend_display_name(backend: &str) -> &'static str {
    match backend {
        "openhands" => "OpenHands",
        "native-rust" | "native_mock" | "mock" => "native fallback",
        "planner-model" => "Planner",
        _ => "unknown",
    }
}

fn model_reference(agent: &AgentSpec, model: &ModelSpec) -> Value {
    json!({
        "profile_ref": &agent.model,
        "provider": &model.provider,
        "model": &model.model,
        "base_url_env": &model.base_url_env,
        "api_key_env": &model.api_key_env
    })
}

fn memory_scope_summary(agent: &AgentSpec, harness: &HarnessSpec) -> Value {
    json!({
        "agent": &agent.memory,
        "harness": &harness.memory,
        "note": "scope names only; memory contents are not embedded"
    })
}

fn permission_summary(harness: &HarnessSpec) -> Value {
    json!({
        "policy": &harness.permissions,
        "summary": {
            "read_files": &harness.permissions.read_files,
            "write_files": &harness.permissions.write_files,
            "run_commands": &harness.permissions.run_commands,
            "network": &harness.permissions.network,
            "secrets": &harness.permissions.secrets,
            "publish_external": &harness.permissions.publish_external,
            "git_commit": &harness.permissions.git_commit,
            "git_push": &harness.permissions.git_push,
            "deploy": &harness.permissions.deploy
        }
    })
}

#[derive(Debug, Clone)]
pub struct WorkflowRunOptions {
    pub workflow_id: String,
    pub task: String,
    pub repo_root: PathBuf,
    pub dry_run: bool,
    pub max_rounds_override: Option<u32>,
    pub plan_context: Option<Value>,
    pub allow_native_fallback_for_openhands: bool,
}

impl WorkflowRunOptions {
    pub fn new(workflow_id: impl Into<String>, task: impl Into<String>) -> Self {
        Self {
            workflow_id: workflow_id.into(),
            task: task.into(),
            repo_root: PathBuf::from("."),
            dry_run: false,
            max_rounds_override: None,
            plan_context: None,
            allow_native_fallback_for_openhands: false,
        }
    }
}

#[derive(Debug)]
pub struct WorkflowRunOutput {
    pub run_id: RunId,
    pub report: FinalReport,
    pub report_ref: String,
}

#[derive(Clone)]
pub struct BackendRegistry {
    planner_model: Arc<dyn HarnessBackend>,
    native_rust: Arc<dyn HarnessBackend>,
    native_mock: Arc<dyn HarnessBackend>,
    openhands: Option<Arc<dyn HarnessBackend>>,
}

impl BackendRegistry {
    pub fn native_only() -> Self {
        Self {
            planner_model: Arc::new(PlannerModelBackend),
            native_rust: Arc::new(NativeMockBackend::default()),
            native_mock: Arc::new(NativeMockBackend::default()),
            openhands: None,
        }
    }

    pub fn from_project_config(config: &ProjectConfig, store: RunStore) -> Self {
        let openhands = config
            .harnesses
            .values()
            .find(|harness| harness.backend == "openhands")
            .and_then(|harness| harness.openhands.as_ref())
            .map(|config| {
                Arc::new(OpenHandsHarnessBackend::new(config.clone(), store.clone()))
                    as Arc<dyn HarnessBackend>
            });
        Self {
            planner_model: Arc::new(PlannerModelBackend),
            native_rust: Arc::new(NativeRustBackend::new(store.clone())),
            native_mock: Arc::new(NativeMockBackend::default()),
            openhands,
        }
    }

    pub fn with_native_backend(mut self, backend: Arc<dyn HarnessBackend>) -> Self {
        self.native_rust = backend;
        self
    }

    pub fn with_openhands_backend(mut self, backend: Arc<dyn HarnessBackend>) -> Self {
        self.openhands = Some(backend);
        self
    }

    pub fn backend_for(&self, backend: &str) -> Option<Arc<dyn HarnessBackend>> {
        match backend {
            "planner-model" => Some(Arc::clone(&self.planner_model)),
            "native-rust" => Some(Arc::clone(&self.native_rust)),
            "native_mock" | "mock" => Some(Arc::clone(&self.native_mock)),
            "openhands" => self.openhands.as_ref().map(Arc::clone),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PlannerModelBackend;

#[async_trait]
impl HarnessBackend for PlannerModelBackend {
    async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        let plan_goal = request
            .backend_context
            .pointer("/coder/plan_context/plan_draft/goal")
            .and_then(Value::as_str)
            .or_else(|| {
                request
                    .backend_context
                    .pointer("/coder/plan_context/original_user_request")
                    .and_then(Value::as_str)
            })
            .unwrap_or("Confirmed workflow plan");
        let mut report = FinalReport::completed(
            "Planner Conversation Harness accepted the confirmed plan without side effects.",
        );
        report.checks = vec![
            "planner-model harness: read-only boundary enforced".to_owned(),
            format!("plan_context: {plan_goal}"),
        ];
        Ok(HarnessRunResult {
            status: "ready".to_owned(),
            report: Some(report),
            events: vec![
                HarnessRunEvent::new(
                    "planner.message.completed",
                    json!({
                        "backend": "planner-model",
                        "node_id": request.node_id,
                        "agent_id": request.agent_id,
                        "harness_id": request.harness_id,
                        "side_effects": "none"
                    }),
                ),
                HarnessRunEvent::new(
                    "planner.plan.updated",
                    json!({
                        "backend": "planner-model",
                        "plan_context_summary": plan_goal
                    }),
                ),
                HarnessRunEvent::new(
                    "planner.readiness.changed",
                    json!({
                        "backend": "planner-model",
                        "readiness": "ready"
                    }),
                ),
            ],
        })
    }
}

#[derive(Debug, Clone)]
pub struct NativeRustBackend {
    store: RunStore,
}

impl NativeRustBackend {
    pub fn new(store: RunStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl HarnessBackend for NativeRustBackend {
    async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        let repo_root = if request.repo_root.trim().is_empty() {
            ".".to_owned()
        } else {
            request.repo_root.clone()
        };
        let tools = native_selected_tools(&request);
        let mut events = vec![HarnessRunEvent::new(
            "backend.native_rust.started",
            json!({
                "backend": "native-rust",
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "tools": tools.iter().cloned().collect::<Vec<_>>()
            }),
        )];
        let mut evidence_refs = Vec::new();
        let mut patch_refs = Vec::new();
        let mut changed_files = BTreeSet::new();
        let mut checks = Vec::new();
        let mut blockers = Vec::new();
        let mut failures = Vec::new();
        let mut completed_tools = 0usize;

        if native_tool_enabled(&tools, "repo_find_files") {
            match find_files(&repo_root, None, &[], 50) {
                Ok(files) => {
                    let file_count = files.len();
                    let reference = write_native_repo_evidence(
                        &self.store,
                        &request.run_id,
                        RepoEvidenceKind::RepoFileList,
                        &repo_root,
                        format!("Native Rust backend found {file_count} repo file(s)."),
                        json!({
                            "evidence_kind": "repo_evidence",
                            "operation": "find_files",
                            "files": files
                        }),
                    )?;
                    evidence_refs.push(repo_evidence_ref(&reference));
                    events.push(native_tool_event(
                        "repo_find_files",
                        "completed",
                        json!({ "file_count": file_count }),
                        Some(&reference),
                    ));
                    checks.push("repo_find_files: completed".to_owned());
                    completed_tools += 1;
                }
                Err(error) => {
                    failures.push(format!("repo_find_files failed: {error}"));
                    events.push(native_tool_failure_event(
                        "repo_find_files",
                        error.to_string(),
                    ));
                }
            }
        }

        if native_tool_enabled(&tools, "repo_search_text") {
            let query = native_search_query(&request.task);
            match search_text(&repo_root, &query, &RepoToolConfig::default()) {
                Ok(matches) => {
                    let match_count = matches.len();
                    let reference = write_native_repo_evidence(
                        &self.store,
                        &request.run_id,
                        RepoEvidenceKind::RepoTextSearch,
                        &repo_root,
                        format!("Native Rust backend found {match_count} text match(es)."),
                        json!({
                            "evidence_kind": "repo_evidence",
                            "operation": "search_text",
                            "query": query,
                            "matches": matches
                        }),
                    )?;
                    evidence_refs.push(repo_evidence_ref(&reference));
                    events.push(native_tool_event(
                        "repo_search_text",
                        "completed",
                        json!({ "match_count": match_count }),
                        Some(&reference),
                    ));
                    checks.push("repo_search_text: completed".to_owned());
                    completed_tools += 1;
                }
                Err(error) => {
                    failures.push(format!("repo_search_text failed: {error}"));
                    events.push(native_tool_failure_event(
                        "repo_search_text",
                        error.to_string(),
                    ));
                }
            }
        }

        let candidate_file = native_candidate_file(&repo_root, &request.task);
        if native_tool_enabled(&tools, "repo_read_file") {
            if let Some(path) = &candidate_file {
                match read_file(&repo_root, path, &RepoToolConfig::default()) {
                    Ok(file) => {
                        let file_path = file.path.clone();
                        let reference = write_native_repo_evidence(
                            &self.store,
                            &request.run_id,
                            RepoEvidenceKind::RepoRead,
                            &repo_root,
                            format!("Native Rust backend read {file_path}."),
                            json!({
                                "evidence_kind": "repo_evidence",
                                "operation": "read_file",
                                "file": file
                            }),
                        )?;
                        evidence_refs.push(repo_evidence_ref(&reference));
                        events.push(native_tool_event(
                            "repo_read_file",
                            "completed",
                            json!({ "path": file_path }),
                            Some(&reference),
                        ));
                        checks.push(format!("repo_read_file: {file_path}"));
                        completed_tools += 1;
                    }
                    Err(error) => {
                        failures.push(format!("repo_read_file failed: {error}"));
                        events.push(native_tool_failure_event(
                            "repo_read_file",
                            error.to_string(),
                        ));
                    }
                }
            } else {
                events.push(native_tool_skipped_event(
                    "repo_read_file",
                    "no safe readable candidate file found",
                ));
            }
        }

        if native_tool_enabled(&tools, "repo_read_file_range") {
            if let Some(path) = &candidate_file {
                match read_file_range(&repo_root, path, 1, 80, 16_000) {
                    Ok(snippet) => {
                        let snippet_path = snippet.path.clone();
                        let reference = write_native_repo_evidence(
                            &self.store,
                            &request.run_id,
                            RepoEvidenceKind::RepoRead,
                            &repo_root,
                            format!(
                                "Native Rust backend read {snippet_path}:1-{}.",
                                snippet.end_line
                            ),
                            json!({
                                "evidence_kind": "repo_evidence",
                                "operation": "read_file_range",
                                "snippet": snippet
                            }),
                        )?;
                        evidence_refs.push(repo_evidence_ref(&reference));
                        events.push(native_tool_event(
                            "repo_read_file_range",
                            "completed",
                            json!({ "path": snippet_path }),
                            Some(&reference),
                        ));
                        checks.push(format!("repo_read_file_range: {snippet_path}"));
                        completed_tools += 1;
                    }
                    Err(error) => {
                        failures.push(format!("repo_read_file_range failed: {error}"));
                        events.push(native_tool_failure_event(
                            "repo_read_file_range",
                            error.to_string(),
                        ));
                    }
                }
            }
        }

        if native_tool_enabled(&tools, "git_status") {
            match git_status(&repo_root) {
                Ok(status) => {
                    let reference = write_native_repo_evidence(
                        &self.store,
                        &request.run_id,
                        RepoEvidenceKind::RepoDiff,
                        &repo_root,
                        "Native Rust backend captured git status.",
                        json!({
                            "evidence_kind": "repo_evidence",
                            "operation": "git_status",
                            "status": status
                        }),
                    )?;
                    evidence_refs.push(repo_evidence_ref(&reference));
                    events.push(native_tool_event(
                        "git_status",
                        "completed",
                        json!({}),
                        Some(&reference),
                    ));
                    checks.push("git_status: completed".to_owned());
                    completed_tools += 1;
                }
                Err(error) => {
                    events.push(native_tool_failure_event("git_status", error.to_string()));
                }
            }
        }

        if native_tool_enabled(&tools, "git_diff") {
            match git_diff(&repo_root, coder_tools::DEFAULT_MAX_GIT_OUTPUT_BYTES) {
                Ok(diff) => {
                    let reference = write_native_repo_evidence(
                        &self.store,
                        &request.run_id,
                        RepoEvidenceKind::RepoDiff,
                        &repo_root,
                        "Native Rust backend captured git diff.",
                        json!({
                            "evidence_kind": "repo_evidence",
                            "operation": "git_diff",
                            "diff": diff
                        }),
                    )?;
                    evidence_refs.push(repo_evidence_ref(&reference));
                    events.push(native_tool_event(
                        "git_diff",
                        "completed",
                        json!({}),
                        Some(&reference),
                    ));
                    checks.push("git_diff: completed".to_owned());
                    completed_tools += 1;
                }
                Err(error) => {
                    events.push(native_tool_failure_event("git_diff", error.to_string()));
                }
            }
        }

        if native_tool_enabled(&tools, "command_preview") {
            if let Some(argv) = native_command_args(&request.task) {
                match preview_command(&repo_root, ".", argv, "model", false) {
                    Ok(preview) => {
                        events.push(HarnessRunEvent::new(
                            "native.tool.completed",
                            json!({
                                "tool": "command_preview",
                                "status": "completed",
                                "command": preview.command,
                                "requires_approval": preview.requires_approval,
                                "approval_key": preview.approval_key,
                                "policy": preview.policy
                            }),
                        ));
                        checks.push("command_preview: completed".to_owned());
                        completed_tools += 1;
                    }
                    Err(error) => {
                        failures.push(format!("command_preview failed: {error}"));
                        events.push(native_tool_failure_event(
                            "command_preview",
                            error.to_string(),
                        ));
                    }
                }
            }
        }

        if native_tool_enabled(&tools, "command_run") {
            if let Some(argv) = native_command_args(&request.task) {
                match run_command(
                    &repo_root,
                    CommandRunRequest {
                        argv,
                        source: "model".to_owned(),
                        approved: false,
                        ..CommandRunRequest::default()
                    },
                ) {
                    Ok(output) => {
                        let blocked = output.blocked;
                        let requires_approval = output.requires_approval;
                        let reference = write_native_repo_evidence(
                            &self.store,
                            &request.run_id,
                            RepoEvidenceKind::RepoTest,
                            &repo_root,
                            format!("Native Rust command {}: {}.", output.status, output.command),
                            json!({
                                "evidence_kind": "command_evidence",
                                "operation": "command_run",
                                "result": output
                            }),
                        )?;
                        evidence_refs.push(repo_evidence_ref(&reference));
                        let event_kind = if blocked && requires_approval {
                            "approval.requested"
                        } else {
                            "native.tool.completed"
                        };
                        events.push(
                            HarnessRunEvent::new(
                                event_kind,
                                json!({
                                    "tool": "command_run",
                                    "approval_type": if blocked && requires_approval { "command" } else { "" },
                                    "status": if blocked { "blocked" } else { "completed" },
                                    "requires_approval": requires_approval,
                                    "evidence_ref": reference.ref_id
                                }),
                            )
                            .with_ref("command_evidence", format!("repo-evidence://{}", reference.ref_id)),
                        );
                        if blocked && requires_approval {
                            blockers.push("command_run requires approval".to_owned());
                        } else {
                            checks.push("command_run: completed".to_owned());
                            completed_tools += 1;
                        }
                    }
                    Err(error) => {
                        failures.push(format!("command_run failed: {error}"));
                        events.push(native_tool_failure_event("command_run", error.to_string()));
                    }
                }
            }
        }

        let patch_file = native_patch_file(&repo_root, &request.task);
        if native_tool_enabled(&tools, "patch_preview") {
            if let Some(path) = &patch_file {
                match preview_patch_file(&repo_root, path, coder_tools::DEFAULT_MAX_PATCH_BYTES) {
                    Ok(preview) => {
                        let touched = preview
                            .files
                            .iter()
                            .filter_map(|file| {
                                file.new_path.clone().or_else(|| file.old_path.clone())
                            })
                            .collect::<Vec<_>>();
                        for path in &touched {
                            changed_files.insert(path.clone());
                        }
                        let reference = write_native_repo_evidence(
                            &self.store,
                            &request.run_id,
                            RepoEvidenceKind::RepoDiff,
                            &repo_root,
                            format!(
                                "Native Rust backend previewed patch touching {} file(s).",
                                preview.file_count
                            ),
                            json!({
                                "evidence_kind": "repo_evidence",
                                "operation": "patch_preview",
                                "preview": preview
                            }),
                        )?;
                        patch_refs.push(format!("repo-evidence://{}", reference.ref_id));
                        evidence_refs.push(repo_evidence_ref(&reference));
                        events.push(native_tool_event(
                            "patch_preview",
                            "completed",
                            json!({ "files": touched }),
                            Some(&reference),
                        ));
                        checks.push("patch_preview: completed".to_owned());
                        completed_tools += 1;
                    }
                    Err(error) => {
                        failures.push(format!("patch_preview failed: {error}"));
                        events.push(native_tool_failure_event(
                            "patch_preview",
                            error.to_string(),
                        ));
                    }
                }
            } else {
                events.push(native_tool_skipped_event(
                    "patch_preview",
                    "no patch file found",
                ));
            }
        }

        if native_tool_enabled(&tools, "patch_apply")
            && native_task_requests_patch_apply(&request.task)
        {
            if let Some(path) = &patch_file {
                match apply_patch_file(
                    &repo_root,
                    ToolPatchApplyRequest {
                        patch_file: path.clone(),
                        max_patch_bytes: coder_tools::DEFAULT_MAX_PATCH_BYTES,
                        source: "model".to_owned(),
                        approved: false,
                    },
                ) {
                    Ok(result) => {
                        let blocked = result.requires_approval;
                        let reference = write_native_repo_evidence(
                            &self.store,
                            &request.run_id,
                            RepoEvidenceKind::RepoDiff,
                            &repo_root,
                            format!(
                                "Native Rust patch apply {}: {} file(s).",
                                result.status, result.preview.file_count
                            ),
                            json!({
                                "evidence_kind": "patch_apply",
                                "operation": "patch_apply",
                                "result": result
                            }),
                        )?;
                        patch_refs.push(format!("repo-evidence://{}", reference.ref_id));
                        evidence_refs.push(repo_evidence_ref(&reference));
                        events.push(
                            HarnessRunEvent::new(
                                if blocked {
                                    "approval.requested"
                                } else {
                                    "native.tool.completed"
                                },
                                json!({
                                    "tool": "patch_apply",
                                    "approval_type": if blocked { "patch_apply" } else { "" },
                                    "status": if blocked { "blocked" } else { "completed" },
                                    "requires_approval": blocked,
                                    "evidence_ref": reference.ref_id
                                }),
                            )
                            .with_ref(
                                "patch_evidence",
                                format!("repo-evidence://{}", reference.ref_id),
                            ),
                        );
                        if blocked {
                            blockers.push("patch_apply requires approval".to_owned());
                        } else {
                            checks.push("patch_apply: completed".to_owned());
                            completed_tools += 1;
                        }
                    }
                    Err(error) => {
                        failures.push(format!("patch_apply failed: {error}"));
                        events.push(native_tool_failure_event("patch_apply", error.to_string()));
                    }
                }
            }
        }

        let status = if !blockers.is_empty() {
            "blocked"
        } else if completed_tools == 0 && !failures.is_empty() {
            "failed"
        } else if request_agent_role(&request) == Some("planner") {
            "ready"
        } else {
            "completed"
        };
        let mut report = match status {
            "blocked" => FinalReport::blocked(
                "Native Rust backend stopped before side effects.",
                blockers.join("; "),
            ),
            "failed" => FinalReport::failed(
                "Native Rust backend could not complete requested tool work.",
                failures.join("; "),
            ),
            _ => FinalReport::completed(format!(
                "Native Rust backend completed {} tool operation(s).",
                completed_tools
            )),
        };
        report.checks = checks;
        report.evidence_refs = evidence_refs;
        report.patch_refs = patch_refs;
        report.changed_files = changed_files.into_iter().collect();
        if !failures.is_empty() && status != "failed" {
            report.next_steps = failures;
        }
        let react_events = native_react_lifecycle_events(&request, &events, status);
        if !react_events.is_empty() {
            let mut ordered_events = Vec::with_capacity(events.len() + react_events.len());
            if !events.is_empty() {
                ordered_events.push(events.remove(0));
            }
            ordered_events.extend(react_events);
            ordered_events.extend(events);
            events = ordered_events;
        }
        events.push(HarnessRunEvent::new(
            format!("backend.native_rust.{status}"),
            json!({
                "backend": "native-rust",
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "status": status,
                "completed_tools": completed_tools
            }),
        ));
        Ok(HarnessRunResult {
            status: status.to_owned(),
            report: Some(report),
            events,
        })
    }
}

fn native_react_lifecycle_events(
    request: &HarnessRunRequest,
    source_events: &[HarnessRunEvent],
    terminal_status: &str,
) -> Vec<HarnessRunEvent> {
    if request_agent_role(request) != Some("executor") {
        return Vec::new();
    }
    let action_events = source_events
        .iter()
        .filter(|event| native_event_has_tool_action(event))
        .collect::<Vec<_>>();
    let mut events = Vec::new();
    let mut previous_observation: Option<String> = None;
    for (index, event) in action_events.iter().enumerate() {
        let step = index + 1;
        let tool_name = event_payload_string(&event.payload, "tool")
            .unwrap_or_else(|| "native_tool".to_owned());
        let status = native_public_tool_status(event);
        let observation = native_observation_summary(event, &tool_name, &status);
        let next_tool = action_events
            .get(index + 1)
            .and_then(|next| event_payload_string(&next.payload, "tool"));
        let reasoning_summary = if let Some(previous) = &previous_observation {
            format!(
                "Use the previous observation to choose the next harness action: {}",
                truncate_public(previous, 180)
            )
        } else {
            format!(
                "Select the first harness action for executor task: {}",
                truncate_public(&request.task, 180)
            )
        };

        events.push(HarnessRunEvent::new(
            "executor.reasoning_summary",
            json!({
                "run_id": request.run_id.as_str(),
                "workflow_id": request.workflow_id,
                "backend": "native-rust",
                "step": step,
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "summary": reasoning_summary,
                "previous_observation": previous_observation
            }),
        ));
        events.push(HarnessRunEvent::new(
            "executor.action_selected",
            json!({
                "run_id": request.run_id.as_str(),
                "workflow_id": request.workflow_id,
                "backend": "native-rust",
                "step": step,
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "tool_name": tool_name,
                "action": "run_harness_tool",
                "permission_boundary": "harness",
                "allowed_by_harness": true,
                "status": "selected"
            }),
        ));
        if event.kind != "native.tool.skipped" {
            events.push(HarnessRunEvent::new(
                "tool.started",
                json!({
                    "run_id": request.run_id.as_str(),
                    "workflow_id": request.workflow_id,
                    "backend": "native-rust",
                    "step": step,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "tool_name": tool_name,
                    "status": "started"
                }),
            ));
        }
        events.push(copy_event_refs(
            HarnessRunEvent::new(
                "tool.completed",
                json!({
                    "run_id": request.run_id.as_str(),
                    "workflow_id": request.workflow_id,
                    "backend": "native-rust",
                    "step": step,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "tool_name": tool_name,
                    "status": status,
                    "summary": observation,
                    "evidence_ref": first_event_ref_uri(event)
                }),
            ),
            event,
        ));
        events.push(copy_event_refs(
            HarnessRunEvent::new(
                "observation.recorded",
                json!({
                    "run_id": request.run_id.as_str(),
                    "workflow_id": request.workflow_id,
                    "backend": "native-rust",
                    "step": step,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "tool_name": tool_name,
                    "summary": observation,
                    "evidence_ref": first_event_ref_uri(event)
                }),
            ),
            event,
        ));
        events.push(HarnessRunEvent::new(
            "executor.next_step",
            json!({
                "run_id": request.run_id.as_str(),
                "workflow_id": request.workflow_id,
                "backend": "native-rust",
                "step": step,
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "based_on_observation": observation,
                "next_action": if next_tool.is_some() { "continue" } else { "finalize" },
                "next_tool": next_tool
            }),
        ));
        previous_observation = Some(observation);
    }
    if !action_events.is_empty() {
        events.push(HarnessRunEvent::new(
            executor_terminal_event_kind(terminal_status),
            json!({
                "run_id": request.run_id.as_str(),
                "workflow_id": request.workflow_id,
                "backend": "native-rust",
                "step": action_events.len(),
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "status": terminal_status,
                "summary": format!(
                    "Executor {} after {} harness action(s).",
                    terminal_status,
                    action_events.len()
                )
            }),
        ));
    }
    events
}

fn native_selected_tools(request: &HarnessRunRequest) -> BTreeSet<String> {
    request
        .backend_context
        .pointer("/coder/harness/selected_tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn native_tool_enabled(tools: &BTreeSet<String>, canonical: &str) -> bool {
    if tools.is_empty() {
        return matches!(
            canonical,
            "repo_find_files" | "repo_read_file_range" | "git_status" | "git_diff"
        );
    }
    native_tool_aliases(canonical)
        .iter()
        .any(|alias| tools.contains(*alias))
}

fn native_tool_aliases(canonical: &str) -> &'static [&'static str] {
    match canonical {
        "repo_find_files" => &["repo_find_files", "find_files", "repo_files"],
        "repo_search_text" => &["repo_search_text", "repo_search", "search_text"],
        "repo_read_file" => &["repo_read_file", "read_file"],
        "repo_read_file_range" => &["repo_read_file_range", "read_file_range", "read_file"],
        "git_status" => &["git_status"],
        "git_diff" => &["git_diff"],
        "command_preview" => &["command_preview", "preview_command"],
        "command_run" => &["command_run", "run_command", "run_command_sandbox"],
        "patch_preview" => &["patch_preview", "preview_patch", "apply_patch_sandbox"],
        "patch_apply" => &["patch_apply", "apply_patch", "apply_patch_sandbox"],
        _ => &[],
    }
}

fn write_native_repo_evidence(
    store: &RunStore,
    run_id: &RunId,
    kind: RepoEvidenceKind,
    repo_root: &str,
    summary: impl Into<String>,
    payload: Value,
) -> Result<RepoEvidenceRef, HarnessError> {
    store
        .write_repo_evidence(run_id, kind, repo_root, Vec::new(), summary, payload)
        .map_err(|error| HarnessError::Failed(error.to_string()))
}

fn repo_evidence_ref(reference: &RepoEvidenceRef) -> coder_core::EvidenceRef {
    coder_core::EvidenceRef {
        kind: "repo_evidence".to_owned(),
        reference: format!("repo-evidence://{}", reference.ref_id),
    }
}

fn native_event_has_tool_action(event: &HarnessRunEvent) -> bool {
    matches!(
        event.kind.as_str(),
        "native.tool.completed"
            | "native.tool.failed"
            | "native.tool.skipped"
            | "approval.requested"
    ) && event.payload.get("tool").and_then(Value::as_str).is_some()
}

fn native_public_tool_status(event: &HarnessRunEvent) -> String {
    if event.kind == "approval.requested" {
        return "blocked".to_owned();
    }
    event_payload_string(&event.payload, "status").unwrap_or_else(|| {
        if event.kind.ends_with(".failed") {
            "failed".to_owned()
        } else {
            "completed".to_owned()
        }
    })
}

fn native_observation_summary(event: &HarnessRunEvent, tool_name: &str, status: &str) -> String {
    if let Some(error) = event_payload_string(&event.payload, "error") {
        return format!("{tool_name} {status}: {}", truncate_public(&error, 220));
    }
    if let Some(evidence_ref) = first_event_ref_uri(event) {
        return format!("{tool_name} {status}; evidence recorded at {evidence_ref}");
    }
    if let Some(reason) = event_payload_string(&event.payload, "reason") {
        return format!("{tool_name} {status}: {}", truncate_public(&reason, 220));
    }
    format!("{tool_name} {status}.")
}

fn executor_terminal_event_kind(status: &str) -> &'static str {
    match status {
        "blocked" => "executor.blocked",
        "failed" => "executor.failed",
        "cancelled" | "canceled" => "executor.failed",
        _ => "executor.completed",
    }
}

fn event_payload_string(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn first_event_ref_uri(event: &HarnessRunEvent) -> Option<String> {
    event
        .refs
        .first()
        .map(|reference| reference.uri.clone())
        .or_else(|| {
            event_payload_string(&event.payload, "evidence_ref").map(|reference| {
                if reference.contains("://") {
                    reference
                } else {
                    format!("repo-evidence://{reference}")
                }
            })
        })
}

fn copy_event_refs(mut target: HarnessRunEvent, source: &HarnessRunEvent) -> HarnessRunEvent {
    for reference in &source.refs {
        target = target.with_ref(reference.label.clone(), reference.uri.clone());
    }
    target
}

fn truncate_public(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    let mut output = trimmed.chars().take(max_chars).collect::<String>();
    if trimmed.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn native_tool_event(
    tool: &str,
    status: &str,
    mut payload: Value,
    reference: Option<&RepoEvidenceRef>,
) -> HarnessRunEvent {
    if let Some(object) = payload.as_object_mut() {
        object.insert("tool".to_owned(), Value::String(tool.to_owned()));
        object.insert("status".to_owned(), Value::String(status.to_owned()));
        if let Some(reference) = reference {
            object.insert(
                "evidence_ref".to_owned(),
                Value::String(reference.ref_id.clone()),
            );
        }
    }
    let event = HarnessRunEvent::new("native.tool.completed", payload);
    if let Some(reference) = reference {
        event.with_ref(
            "repo_evidence",
            format!("repo-evidence://{}", reference.ref_id),
        )
    } else {
        event
    }
}

fn native_tool_failure_event(tool: &str, error: String) -> HarnessRunEvent {
    HarnessRunEvent::new(
        "native.tool.failed",
        json!({
            "tool": tool,
            "status": "failed",
            "error": error
        }),
    )
}

fn native_tool_skipped_event(tool: &str, reason: &str) -> HarnessRunEvent {
    HarnessRunEvent::new(
        "native.tool.skipped",
        json!({
            "tool": tool,
            "status": "skipped",
            "reason": reason
        }),
    )
}

fn native_search_query(task: &str) -> String {
    for marker in ['"', '\''] {
        let mut parts = task.split(marker);
        let _ = parts.next();
        if let Some(quoted) = parts.next() {
            let candidate = quoted.trim();
            if !candidate.is_empty() {
                return candidate.to_owned();
            }
        }
    }
    if task.to_ascii_lowercase().contains("todo") {
        "TODO".to_owned()
    } else {
        "fn ".to_owned()
    }
}

fn native_candidate_file(repo_root: &str, task: &str) -> Option<PathBuf> {
    if let Some(path) = native_path_token(task, &[".rs", ".py", ".ts", ".tsx", ".js", ".md"]) {
        return Some(path);
    }
    for preferred in ["README.md", "readme.md", "Cargo.toml", "package.json"] {
        if read_file_range(repo_root, preferred, 1, 1, 256).is_ok() {
            return Some(PathBuf::from(preferred));
        }
    }
    find_files(repo_root, None, &[], 20)
        .ok()
        .and_then(|files| files.into_iter().next())
        .map(|file| PathBuf::from(file.path))
}

fn native_patch_file(repo_root: &str, task: &str) -> Option<PathBuf> {
    if let Some(path) = native_path_token(task, &[".patch", ".diff"]) {
        return Some(path);
    }
    find_files(
        repo_root,
        None,
        &[String::from("patch"), String::from("diff")],
        20,
    )
    .ok()
    .and_then(|files| files.into_iter().next())
    .map(|file| PathBuf::from(file.path))
}

fn native_path_token(task: &str, suffixes: &[&str]) -> Option<PathBuf> {
    task.split_whitespace()
        .map(|token| {
            token.trim_matches(|ch: char| {
                ch == '"' || ch == '\'' || ch == '`' || ch == ',' || ch == ';' || ch == '.'
            })
        })
        .find(|token| suffixes.iter().any(|suffix| token.ends_with(suffix)))
        .map(PathBuf::from)
}

fn native_command_args(task: &str) -> Option<Vec<String>> {
    let lower = task.to_ascii_lowercase();
    let marker = lower.find("command:").or_else(|| lower.find("run:"))?;
    let command_start = marker + task[marker..].find(':')? + 1;
    let args = task[command_start..]
        .split_whitespace()
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if args.is_empty() {
        None
    } else {
        Some(args)
    }
}

fn native_task_requests_patch_apply(task: &str) -> bool {
    let lower = task.to_ascii_lowercase();
    lower.contains("apply patch") || lower.contains("patch apply")
}

fn request_agent_role(request: &HarnessRunRequest) -> Option<&str> {
    request
        .backend_context
        .pointer("/coder/agent/role")
        .and_then(Value::as_str)
}

#[derive(Debug, Clone, Copy, Default)]
pub enum NativeMockOutcome {
    #[default]
    Completed,
    Blocked,
    Failed,
}

#[derive(Debug, Default)]
pub struct NativeMockBackend {
    outcome: NativeMockOutcome,
}

impl NativeMockBackend {
    pub fn new(outcome: NativeMockOutcome) -> Self {
        Self { outcome }
    }
}

#[async_trait]
impl HarnessBackend for NativeMockBackend {
    async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        let status = match self.outcome {
            NativeMockOutcome::Completed if request.agent_id == "planner" => "ready",
            NativeMockOutcome::Completed => "completed",
            NativeMockOutcome::Blocked => "blocked",
            NativeMockOutcome::Failed => "failed",
        };
        let summary = format!(
            "Native mock backend processed node '{}' for task '{}'.",
            request.node_id, request.task
        );
        let report = match self.outcome {
            NativeMockOutcome::Completed => FinalReport::completed(summary),
            NativeMockOutcome::Blocked => {
                FinalReport::blocked(summary, "native mock backend requested blocked outcome")
            }
            NativeMockOutcome::Failed => {
                FinalReport::failed(summary, "native mock backend requested failed outcome")
            }
        };
        Ok(HarnessRunResult {
            status: status.to_owned(),
            report: Some(report),
            events: vec![HarnessRunEvent::new(
                format!("backend.native_mock.{status}"),
                json!({
                    "backend": "native-rust",
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "status": status
                }),
            )],
        })
    }
}

pub struct OpenHandsHarnessBackend {
    config: OpenHandsHarnessConfig,
    store: RunStore,
}

impl OpenHandsHarnessBackend {
    pub fn new(config: OpenHandsHarnessConfig, store: RunStore) -> Self {
        Self { config, store }
    }
}

#[async_trait]
impl HarnessBackend for OpenHandsHarnessBackend {
    async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        let client = OpenHandsClient::new(openhands_server_config_from_harness(&self.config));
        let health = client
            .health()
            .await
            .map_err(|error| HarnessError::Unavailable(error.to_string()))?;
        if !health.available {
            return Err(HarnessError::Unavailable(health.detail));
        }

        let conversation = client
            .create_conversation(openhands_create_conversation_payload(&request))
            .await
            .map_err(|error| HarnessError::Failed(error.to_string()))?;
        client
            .send_user_message(&conversation.id, &request.task, Some("coder-workflow"))
            .await
            .map_err(|error| HarnessError::Failed(error.to_string()))?;
        let trigger = client
            .trigger_run(&conversation.id)
            .await
            .map_err(|error| HarnessError::Failed(error.to_string()))?;
        let lifecycle = poll_openhands_events(
            &client,
            &conversation.id,
            &request,
            &self.store,
            &self.config,
        )
        .await?;
        let websocket_url = client
            .events_websocket_url(&conversation.id)
            .map_err(|error| HarnessError::Failed(error.to_string()))?;
        let raw_refs = lifecycle
            .events
            .iter()
            .flat_map(|event| event.refs.iter())
            .filter(|reference| reference.label == "openhands.raw_event")
            .map(|reference| reference.uri.clone())
            .collect::<Vec<_>>();
        let mut report = openhands_final_report(
            &request.run_id,
            &conversation.id,
            &trigger,
            lifecycle.captured_events,
            &websocket_url,
            &raw_refs,
        );
        if lifecycle.status != "completed" {
            report.status = match lifecycle.status.as_str() {
                "blocked" => ReportStatus::Blocked,
                "failed" => ReportStatus::Failed,
                "cancelled" => ReportStatus::Cancelled,
                _ => report.status,
            };
            report.blockers.push(lifecycle.reason);
        }

        Ok(HarnessRunResult {
            status: lifecycle.status,
            report: Some(report),
            events: lifecycle.events,
        })
    }
}

struct OpenHandsLifecycleResult {
    status: String,
    reason: String,
    captured_events: usize,
    events: Vec<HarnessRunEvent>,
}

async fn poll_openhands_events(
    client: &OpenHandsClient,
    conversation_id: &str,
    request: &HarnessRunRequest,
    store: &RunStore,
    config: &OpenHandsHarnessConfig,
) -> Result<OpenHandsLifecycleResult, HarnessError> {
    let mut events = Vec::new();
    if config.prefer_websocket {
        if let Ok(websocket_url) = client.events_websocket_url(conversation_id) {
            events.push(HarnessRunEvent::new(
                "backend.openhands.websocket.preferred",
                json!({
                    "websocket_url": websocket_url,
                    "fallback": "polling"
                }),
            ));
        }
    }

    let started = Instant::now();
    let timeout = Duration::from_secs(config.max_event_poll_seconds);
    let poll_interval = Duration::from_millis(config.poll_interval_ms);
    let max_events = config.max_events.max(1);
    let fetch_limit = max_events.min(u16::MAX as usize) as u16;
    let mut seen = BTreeSet::new();
    let mut captured_events = 0;

    loop {
        let raw_events = match client.fetch_events(conversation_id, fetch_limit).await {
            Ok(raw_events) => raw_events,
            Err(error) if openhands_event_fetch_retryable(&error) => {
                events.push(HarnessRunEvent::new(
                    "backend.openhands.events_retry",
                    json!({
                        "conversation_id": conversation_id,
                        "reason": openhands_fetch_retry_reason(&error)
                    }),
                ));
                Vec::new()
            }
            Err(error) => return Err(HarnessError::Failed(error.to_string())),
        };
        for raw in raw_events {
            let key = openhands_event_key(&raw);
            if !seen.insert(key) {
                continue;
            }
            captured_events += 1;
            let terminal = openhands_terminal_status(&raw, &config.terminal_event_kinds);
            events.extend(openhands_raw_harness_events(
                request,
                store,
                raw,
                captured_events,
            )?);
            if let Some((status, reason)) = terminal {
                return Ok(OpenHandsLifecycleResult {
                    status: status.to_owned(),
                    reason,
                    captured_events,
                    events,
                });
            }
            if captured_events >= max_events {
                let reason =
                    format!("OpenHands event limit {max_events} reached before terminal status");
                events.push(HarnessRunEvent::new(
                    "backend.openhands.max_events_reached",
                    json!({
                        "max_events": max_events,
                        "reason": reason
                    }),
                ));
                return Ok(OpenHandsLifecycleResult {
                    status: "blocked".to_owned(),
                    reason,
                    captured_events,
                    events,
                });
            }
        }

        if started.elapsed() >= timeout {
            let reason = format!(
                "OpenHands did not reach a terminal status within {} second(s)",
                config.max_event_poll_seconds
            );
            events.push(HarnessRunEvent::new(
                "backend.openhands.timeout",
                json!({
                    "timeout_seconds": config.max_event_poll_seconds,
                    "captured_events": captured_events,
                    "reason": reason
                }),
            ));
            return Ok(OpenHandsLifecycleResult {
                status: "blocked".to_owned(),
                reason,
                captured_events,
                events,
            });
        }

        if !poll_interval.is_zero() {
            std::thread::sleep(poll_interval);
        }
    }
}

fn openhands_event_fetch_retryable(error: &OpenHandsError) -> bool {
    matches!(
        error,
        OpenHandsError::HttpStatus {
            method: "GET",
            status: 500..=599,
            ..
        }
    )
}

fn openhands_fetch_retry_reason(error: &OpenHandsError) -> Value {
    match error {
        OpenHandsError::HttpStatus {
            method,
            path,
            status,
            ..
        } => json!({
            "method": method,
            "path": path,
            "status": status,
            "summary": "OpenHands event fetch returned a transient server error"
        }),
        _ => json!({
            "summary": "OpenHands event fetch returned a transient error"
        }),
    }
}

fn openhands_raw_harness_events(
    request: &HarnessRunRequest,
    store: &RunStore,
    raw: Value,
    step: usize,
) -> Result<Vec<HarnessRunEvent>, HarnessError> {
    let raw_text =
        serde_json::to_string(&raw).map_err(|error| HarnessError::Failed(error.to_string()))?;
    let raw_ref = store
        .write_large_text_ref(&raw_text)
        .map_err(|error| HarnessError::Failed(error.to_string()))?
        .blob_ref;
    let normalized = normalize_openhands_event(
        request.run_id.clone(),
        0,
        raw.clone(),
        Some(raw_ref.clone()),
    );
    let mut event = HarnessRunEvent::new(normalized.kind, normalized.payload);
    for reference in normalized.refs {
        event = event.with_ref(reference.label, reference.uri);
    }
    let mut events = vec![event];
    events.extend(openhands_public_react_events(request, &raw, &raw_ref, step));
    Ok(events)
}

fn openhands_public_react_events(
    request: &HarnessRunRequest,
    raw: &Value,
    raw_ref: &str,
    step: usize,
) -> Vec<HarnessRunEvent> {
    let raw_kind = openhands_raw_event_kind(raw);
    let normalized_kind = raw_kind.to_ascii_lowercase();
    let summary =
        openhands_public_summary(raw).unwrap_or_else(|| format!("OpenHands emitted {raw_kind}."));
    let tool_name = openhands_tool_name(raw).unwrap_or_else(|| "OpenHands".to_owned());
    let mut events = Vec::new();

    if normalized_kind.contains("message")
        || normalized_kind.contains("reason")
        || normalized_kind.contains("thought")
    {
        events.push(
            HarnessRunEvent::new(
                "executor.reasoning_summary",
                json!({
                    "run_id": request.run_id.as_str(),
                    "workflow_id": request.workflow_id,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "backend": "openhands",
                    "step": step,
                    "summary": truncate_public(&summary, 500),
                    "raw_kind": raw_kind,
                    "raw_ref": raw_ref
                }),
            )
            .with_ref("openhands.raw_event", raw_ref.to_owned()),
        );
    }

    if normalized_kind.contains("action")
        || normalized_kind.contains("tool")
        || normalized_kind.contains("command")
        || raw.get("action").is_some()
    {
        events.push(
            HarnessRunEvent::new(
                "executor.action_selected",
                json!({
                    "run_id": request.run_id.as_str(),
                    "workflow_id": request.workflow_id,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "backend": "openhands",
                    "step": step,
                    "tool_name": tool_name,
                    "action": "openhands_action",
                    "permission_boundary": "harness",
                    "allowed_by_harness": true,
                    "summary": truncate_public(&summary, 500),
                    "raw_kind": raw_kind,
                    "raw_ref": raw_ref
                }),
            )
            .with_ref("openhands.raw_event", raw_ref.to_owned()),
        );
        events.push(
            HarnessRunEvent::new(
                "tool.started",
                json!({
                    "run_id": request.run_id.as_str(),
                    "workflow_id": request.workflow_id,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "backend": "openhands",
                    "step": step,
                    "tool_name": tool_name,
                    "status": "started",
                    "summary": truncate_public(&summary, 500),
                    "raw_kind": raw_kind,
                    "raw_ref": raw_ref
                }),
            )
            .with_ref("openhands.raw_event", raw_ref.to_owned()),
        );
    }

    let observation_like = normalized_kind.contains("observation")
        || normalized_kind.contains("file")
        || normalized_kind.contains("task")
        || normalized_kind.contains("workspace")
        || raw.get("observation").is_some()
        || raw.get("result").is_some();
    if observation_like {
        events.push(
            HarnessRunEvent::new(
                "tool.completed",
                json!({
                    "run_id": request.run_id.as_str(),
                    "workflow_id": request.workflow_id,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "backend": "openhands",
                    "step": step,
                    "tool_name": tool_name,
                    "status": "completed",
                    "summary": truncate_public(&summary, 500),
                    "raw_kind": raw_kind,
                    "evidence_ref": raw_ref,
                    "raw_ref": raw_ref
                }),
            )
            .with_ref("openhands.raw_event", raw_ref.to_owned()),
        );
        events.push(
            HarnessRunEvent::new(
                "observation.recorded",
                json!({
                    "run_id": request.run_id.as_str(),
                    "workflow_id": request.workflow_id,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "backend": "openhands",
                    "step": step,
                    "tool_name": tool_name,
                    "summary": truncate_public(&summary, 500),
                    "evidence_ref": raw_ref,
                    "raw_kind": raw_kind,
                    "raw_ref": raw_ref
                }),
            )
            .with_ref("openhands.raw_event", raw_ref.to_owned()),
        );
        events.push(
            HarnessRunEvent::new(
                "executor.next_step",
                json!({
                    "run_id": request.run_id.as_str(),
                    "workflow_id": request.workflow_id,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "backend": "openhands",
                    "step": step,
                    "based_on_observation": truncate_public(&summary, 500),
                    "next_action": "continue_or_finalize",
                    "evidence_ref": raw_ref,
                    "raw_kind": raw_kind,
                    "raw_ref": raw_ref
                }),
            )
            .with_ref("openhands.raw_event", raw_ref.to_owned()),
        );
    }

    if let Some(event) = openhands_command_event(
        request,
        raw,
        raw_ref,
        &raw_kind,
        &normalized_kind,
        &tool_name,
        &summary,
        step,
    ) {
        events.push(event);
    }

    if let Some(event) = openhands_file_change_event(
        request,
        raw,
        raw_ref,
        &raw_kind,
        &normalized_kind,
        &summary,
        step,
    ) {
        events.push(event);
    }

    if let Some((status, _)) = openhands_terminal_status_from_raw(raw, &raw_kind, &tool_name) {
        events.push(
            HarnessRunEvent::new(
                executor_terminal_event_kind(status),
                json!({
                    "run_id": request.run_id.as_str(),
                    "workflow_id": request.workflow_id,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "backend": "openhands",
                    "step": step,
                    "tool_name": tool_name,
                    "status": status,
                    "summary": truncate_public(&summary, 500),
                    "raw_kind": raw_kind,
                    "raw_ref": raw_ref
                }),
            )
            .with_ref("openhands.raw_event", raw_ref.to_owned()),
        );
    }
    events
}

fn openhands_public_summary(raw: &Value) -> Option<String> {
    for key in [
        "summary",
        "message",
        "content",
        "text",
        "thought",
        "observation",
        "result",
    ] {
        if let Some(value) = raw.get(key) {
            if let Some(text) = value.as_str() {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
            if value.is_object() || value.is_array() {
                return Some(format!("OpenHands provided structured {key} data."));
            }
        }
    }
    None
}

fn openhands_tool_name(raw: &Value) -> Option<String> {
    for key in ["tool_name", "tool", "command", "action"] {
        if let Some(value) = raw.get(key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn openhands_command_event(
    request: &HarnessRunRequest,
    raw: &Value,
    raw_ref: &str,
    raw_kind: &str,
    normalized_kind: &str,
    tool_name: &str,
    summary: &str,
    step: usize,
) -> Option<HarnessRunEvent> {
    let command = openhands_command_text(raw).or_else(|| {
        let normalized_tool = tool_name.to_ascii_lowercase();
        if normalized_kind.contains("command")
            || normalized_kind.contains("shell")
            || normalized_tool.contains("shell")
            || normalized_tool.contains("bash")
            || normalized_tool.contains("terminal")
        {
            Some(truncate_public(summary, 500))
        } else {
            None
        }
    })?;
    let argv = raw
        .get("argv")
        .or_else(|| raw.pointer("/args/argv"))
        .filter(|value| value.is_array())
        .cloned();
    let returncode = openhands_i64_field(raw, &["returncode", "exit_code", "exitCode", "code"]);
    let failed = returncode.is_some_and(|code| code != 0)
        || openhands_bool_field(raw, &["failed", "error", "timed_out"]).unwrap_or(false)
        || openhands_text_field(raw, &["status", "state", "outcome"])
            .as_deref()
            .map(|status| {
                matches!(
                    status.to_ascii_lowercase().as_str(),
                    "failed" | "failure" | "error" | "errored" | "timeout" | "timed_out"
                )
            })
            .unwrap_or(false);
    let completed = failed
        || returncode.is_some()
        || normalized_kind.contains("observation")
        || normalized_kind.contains("result")
        || normalized_kind.contains("completed")
        || normalized_kind.contains("complete")
        || normalized_kind.contains("done")
        || raw.get("stdout").is_some()
        || raw.get("stderr").is_some()
        || raw.get("output").is_some()
        || raw.get("result").is_some();
    let (kind, status) = if failed {
        ("command.failed", "failed")
    } else if completed {
        ("command.completed", "completed")
    } else {
        ("command.previewed", "previewed")
    };

    Some(
        HarnessRunEvent::new(
            kind,
            json!({
                "run_id": request.run_id.as_str(),
                "workflow_id": request.workflow_id,
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "backend": "openhands",
                "step": step,
                "command": command,
                "argv": argv,
                "cwd": openhands_text_field(raw, &["cwd", "working_dir", "workingDirectory"]),
                "status": status,
                "passed": !failed,
                "returncode": returncode,
                "exit_code": returncode,
                "timed_out": openhands_bool_field(raw, &["timed_out", "timeout"]).unwrap_or(false),
                "stdout_preview": openhands_text_field(
                    raw,
                    &["stdout_preview", "stdout", "output_preview", "output", "content", "message"]
                ).map(|value| truncate_public(&value, 1000)),
                "stderr_preview": openhands_text_field(raw, &["stderr_preview", "stderr"])
                    .map(|value| truncate_public(&value, 1000)),
                "duration_ms": openhands_u64_field(raw, &["duration_ms", "elapsed_ms"]),
                "summary": truncate_public(summary, 500),
                "evidence_ref": raw_ref,
                "raw_kind": raw_kind,
                "raw_ref": raw_ref
            }),
        )
        .with_ref("openhands.raw_event", raw_ref.to_owned()),
    )
}

#[allow(clippy::too_many_arguments)]
fn openhands_file_change_event(
    request: &HarnessRunRequest,
    raw: &Value,
    raw_ref: &str,
    raw_kind: &str,
    normalized_kind: &str,
    summary: &str,
    step: usize,
) -> Option<HarnessRunEvent> {
    let files = openhands_changed_files(raw, normalized_kind);
    if files.is_empty() {
        return None;
    }

    Some(
        HarnessRunEvent::new(
            "patch.applied",
            json!({
                "run_id": request.run_id.as_str(),
                "workflow_id": request.workflow_id,
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "backend": "openhands",
                "step": step,
                "status": "applied",
                "applied": true,
                "patch_file": "openhands-file-change",
                "file_count": files.len(),
                "files": files,
                "summary": truncate_public(summary, 500),
                "evidence_ref": raw_ref,
                "raw_kind": raw_kind,
                "raw_ref": raw_ref
            }),
        )
        .with_ref("openhands.raw_event", raw_ref.to_owned()),
    )
}

fn openhands_command_text(raw: &Value) -> Option<String> {
    openhands_text_field(
        raw,
        &[
            "command",
            "cmd",
            "shell_command",
            "bash_command",
            "terminal_command",
        ],
    )
    .or_else(|| {
        raw.get("argv")
            .or_else(|| raw.pointer("/args/argv"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(openhands_value_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|value| !value.trim().is_empty())
    })
}

fn openhands_changed_files(raw: &Value, normalized_kind: &str) -> Vec<Value> {
    if !openhands_file_change_like(raw, normalized_kind) {
        return Vec::new();
    }

    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    let default_status = openhands_file_status(raw, normalized_kind);

    for key in ["files", "changed_files", "touched_files"] {
        if let Some(items) = raw.get(key).and_then(Value::as_array) {
            for item in items {
                if let Some(file) =
                    openhands_changed_file_from_value(item, &default_status, normalized_kind)
                {
                    push_openhands_file(&mut files, &mut seen, file);
                }
            }
        }
    }

    for key in [
        "file",
        "path",
        "filename",
        "filepath",
        "new_path",
        "old_path",
        "target_file",
    ] {
        if let Some(value) = raw.get(key) {
            if let Some(file) =
                openhands_changed_file_from_value(value, &default_status, normalized_kind)
            {
                push_openhands_file(&mut files, &mut seen, file);
            }
        }
    }

    if let Some(diff) = openhands_text_field(raw, &["diff", "patch"]) {
        for file in openhands_changed_files_from_diff(&diff, &default_status) {
            push_openhands_file(&mut files, &mut seen, file);
        }
    }

    files
}

fn openhands_file_change_like(raw: &Value, normalized_kind: &str) -> bool {
    if [
        "patch", "edit", "write", "create", "delete", "remove", "modify",
    ]
    .iter()
    .any(|needle| normalized_kind.contains(needle))
    {
        return true;
    }

    if raw.get("files").is_some()
        || raw.get("changed_files").is_some()
        || raw.get("touched_files").is_some()
        || raw.get("diff").is_some()
        || raw.get("patch").is_some()
    {
        return true;
    }

    openhands_text_field(raw, &["status", "action", "operation"])
        .map(|value| value.to_ascii_lowercase())
        .map(|value| {
            [
                "patch", "edit", "write", "create", "delete", "remove", "modify",
            ]
            .iter()
            .any(|needle| value.contains(needle))
        })
        .unwrap_or(false)
}

fn openhands_changed_file_from_value(
    value: &Value,
    default_status: &str,
    normalized_kind: &str,
) -> Option<Value> {
    match value {
        Value::String(path) => openhands_changed_file(path, default_status, None, None),
        Value::Object(_) => {
            let path = openhands_text_field(
                value,
                &[
                    "new_path",
                    "path",
                    "old_path",
                    "file",
                    "filename",
                    "filepath",
                    "target_file",
                ],
            )?;
            let status = openhands_text_field(value, &["status", "action", "operation"])
                .unwrap_or_else(|| openhands_file_status(value, normalized_kind));
            let additions = openhands_u64_field(value, &["additions", "added"]);
            let deletions = openhands_u64_field(value, &["deletions", "deleted", "removed"]);
            openhands_changed_file(&path, &status, additions, deletions)
        }
        _ => None,
    }
}

fn openhands_changed_file(
    path: &str,
    status: &str,
    additions: Option<u64>,
    deletions: Option<u64>,
) -> Option<Value> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let mut object = serde_json::Map::new();
    object.insert("path".to_owned(), Value::String(path.to_owned()));
    object.insert("status".to_owned(), Value::String(status.to_owned()));
    if let Some(additions) = additions {
        object.insert(
            "additions".to_owned(),
            Value::Number(serde_json::Number::from(additions)),
        );
    }
    if let Some(deletions) = deletions {
        object.insert(
            "deletions".to_owned(),
            Value::Number(serde_json::Number::from(deletions)),
        );
    }
    Some(Value::Object(object))
}

fn push_openhands_file(files: &mut Vec<Value>, seen: &mut BTreeSet<String>, file: Value) {
    let Some(path) = file.get("path").and_then(Value::as_str).map(str::to_owned) else {
        return;
    };
    if seen.insert(path) {
        files.push(file);
    }
}

fn openhands_changed_files_from_diff(diff: &str, default_status: &str) -> Vec<Value> {
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    for line in diff.lines() {
        let Some(path) = line.strip_prefix("+++ b/") else {
            continue;
        };
        if path == "/dev/null" {
            continue;
        }
        if let Some(file) = openhands_changed_file(path, default_status, None, None) {
            push_openhands_file(&mut files, &mut seen, file);
        }
    }
    files
}

fn openhands_file_status(raw: &Value, normalized_kind: &str) -> String {
    openhands_text_field(raw, &["status", "action", "operation"]).unwrap_or_else(|| {
        if normalized_kind.contains("delete") || normalized_kind.contains("remove") {
            "deleted".to_owned()
        } else if normalized_kind.contains("create") || normalized_kind.contains("add") {
            "added".to_owned()
        } else {
            "modified".to_owned()
        }
    })
}

fn openhands_text_field(raw: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = raw.get(*key).and_then(openhands_value_string) {
            return Some(value);
        }
    }
    for parent in [
        "action",
        "args",
        "metadata",
        "data",
        "tool_call",
        "toolCall",
    ] {
        let Some(value) = raw.get(parent) else {
            continue;
        };
        for key in keys {
            if let Some(value) = value.get(*key).and_then(openhands_value_string) {
                return Some(value);
            }
        }
    }
    None
}

fn openhands_bool_field(raw: &Value, keys: &[&str]) -> Option<bool> {
    for key in keys {
        if let Some(value) = raw.get(*key).and_then(Value::as_bool) {
            return Some(value);
        }
    }
    None
}

fn openhands_i64_field(raw: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        let Some(value) = raw.get(*key) else {
            continue;
        };
        if let Some(value) = value.as_i64() {
            return Some(value);
        }
        if let Some(value) = value.as_str().and_then(|value| value.parse::<i64>().ok()) {
            return Some(value);
        }
    }
    None
}

fn openhands_u64_field(raw: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        let Some(value) = raw.get(*key) else {
            continue;
        };
        if let Some(value) = value.as_u64() {
            return Some(value);
        }
        if let Some(value) = value.as_str().and_then(|value| value.parse::<u64>().ok()) {
            return Some(value);
        }
    }
    None
}

fn openhands_value_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        }
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn openhands_event_key(raw: &Value) -> String {
    raw.get("id")
        .or_else(|| raw.get("event_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string(raw).unwrap_or_else(|_| "unknown".to_owned()))
}

fn openhands_terminal_status(
    raw: &Value,
    terminal_event_kinds: &[String],
) -> Option<(&'static str, String)> {
    let raw_kind = openhands_raw_event_kind(raw);
    let normalized_kind = raw_kind.to_ascii_lowercase();
    let tool_name = openhands_tool_name(raw).unwrap_or_default();
    let configured_terminal = terminal_event_kinds
        .iter()
        .any(|kind| kind.eq_ignore_ascii_case(&raw_kind));

    if let Some(terminal) = openhands_terminal_status_from_raw(raw, &raw_kind, &tool_name) {
        return Some(terminal);
    }
    if !configured_terminal {
        return None;
    }
    if let Some(status) = terminal_status_from_text(&normalized_kind) {
        return Some((
            status,
            format!("OpenHands emitted terminal event kind '{raw_kind}'"),
        ));
    }
    Some((
        "completed",
        format!("OpenHands emitted configured terminal event kind '{raw_kind}'"),
    ))
}

fn openhands_terminal_status_from_raw(
    raw: &Value,
    raw_kind: &str,
    tool_name: &str,
) -> Option<(&'static str, String)> {
    let normalized_kind = raw_kind.to_ascii_lowercase();
    let status = raw
        .get("status")
        .or_else(|| raw.get("state"))
        .or_else(|| raw.get("result"))
        .and_then(Value::as_str)
        .map(|value| value.to_ascii_lowercase());

    if let Some(status) = status.as_deref().and_then(terminal_status_from_text) {
        return Some((
            status,
            format!("OpenHands reported terminal status '{}'", status),
        ));
    }
    if let Some(status) = terminal_status_from_text(&normalized_kind) {
        return Some((
            status,
            format!("OpenHands emitted terminal event kind '{raw_kind}'"),
        ));
    }
    if openhands_finish_signal(raw, raw_kind, tool_name) {
        return Some((
            "completed",
            "OpenHands emitted finish tool event".to_owned(),
        ));
    }
    None
}

fn openhands_finish_signal(raw: &Value, raw_kind: &str, tool_name: &str) -> bool {
    if openhands_finish_signal_text(raw_kind) || openhands_finish_signal_text(tool_name) {
        return true;
    }

    [
        "/tool_name",
        "/tool",
        "/name",
        "/action/kind",
        "/action/name",
        "/action/tool_name",
        "/tool_call/kind",
        "/tool_call/name",
        "/tool_call/tool_name",
    ]
    .iter()
    .filter_map(|pointer| raw.pointer(pointer))
    .filter_map(openhands_value_string)
    .any(|value| openhands_finish_signal_text(&value))
}

fn openhands_finish_signal_text(value: &str) -> bool {
    let normalized: String = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect();
    matches!(
        normalized.as_str(),
        "finish" | "finishtool" | "finishaction" | "finishobservation"
    )
}

fn terminal_status_from_text(value: &str) -> Option<&'static str> {
    match value {
        "completed" | "complete" | "success" | "succeeded" | "done" | "finished"
        | "run.completed" => Some("completed"),
        "blocked" | "run.blocked" => Some("blocked"),
        "failed" | "failure" | "error" | "errored" | "run.failed" => Some("failed"),
        "cancelled" | "canceled" | "run.cancelled" | "run.canceled" => Some("cancelled"),
        _ => None,
    }
}

fn openhands_create_conversation_payload(request: &HarnessRunRequest) -> Value {
    let mut payload = request
        .backend_context
        .pointer("/openhands/create_conversation_payload")
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "workspace": {
                    "kind": "LocalWorkspace",
                    "working_dir": request.repo_root
                },
                "agent": {"kind": "Agent"},
                "metadata": {
                    "source": "coder-workflow",
                    "coder": {
                        "contract": "coder.openhands.conversation.v1",
                        "agent_kind_source": "default_fallback",
                        "run_id": request.run_id.as_str(),
                        "workflow_id": &request.workflow_id,
                        "node_id": &request.node_id,
                        "agent_id": &request.agent_id,
                        "harness_id": &request.harness_id
                    }
                }
            })
        });
    ensure_openhands_workspace(&mut payload, &request.repo_root);
    payload
}

fn ensure_openhands_workspace(payload: &mut Value, repo_root: &str) {
    let has_workspace = payload
        .get("workspace")
        .map(|workspace| !workspace.is_null())
        .unwrap_or(false);
    if !has_workspace {
        payload["workspace"] = json!({
            "kind": "LocalWorkspace",
            "working_dir": repo_root
        });
    }
}

struct WorkflowReportInput<'a> {
    run_id: &'a RunId,
    workflow_id: &'a str,
    request: &'a str,
    status: RunStatus,
    reason: Option<&'a str>,
    dispatched_nodes: usize,
    checks: Vec<String>,
    evidence_refs: Vec<coder_core::EvidenceRef>,
    blockers: Vec<String>,
    changed_files: Vec<String>,
    patch_refs: Vec<String>,
    plan_context: Option<Value>,
}

fn workflow_run_report(input: WorkflowReportInput<'_>) -> FinalReport {
    let report_status = match input.status {
        RunStatus::Completed => ReportStatus::Completed,
        RunStatus::Blocked => ReportStatus::Blocked,
        RunStatus::Failed | RunStatus::Queued | RunStatus::Running => ReportStatus::Failed,
        RunStatus::Cancelled => ReportStatus::Cancelled,
    };
    let mut report = FinalReport::with_status(
        report_status,
        format!(
            "Workflow '{workflow_id}' finished with status '{}' after dispatching {} node(s).",
            run_status_str(input.status),
            input.dispatched_nodes,
            workflow_id = input.workflow_id
        ),
    );
    report.checks = input.checks;
    if let Some(summary) = plan_context_summary(input.plan_context.as_ref()) {
        report.checks.push(format!("plan_context: {summary}"));
    }
    for criterion in plan_acceptance_criteria(input.plan_context.as_ref()) {
        report.checks.push(format!("acceptance: {criterion}"));
    }
    report.blockers = input.blockers;
    report.changed_files = input.changed_files;
    report.patch_refs = input.patch_refs;
    if report.blockers.is_empty() {
        if let Some(reason) = input.reason {
            report.blockers.push(reason.to_owned());
        }
    }
    let mut evidence_refs = input.evidence_refs;
    evidence_refs.push(coder_core::EvidenceRef {
        kind: "event_log".to_owned(),
        reference: format!("eventlog://runs/{}/events.jsonl", input.run_id.as_str()),
    });
    evidence_refs.sort_by(|left, right| {
        (left.kind.as_str(), left.reference.as_str())
            .cmp(&(right.kind.as_str(), right.reference.as_str()))
    });
    evidence_refs
        .dedup_by(|left, right| left.kind == right.kind && left.reference == right.reference);
    report.evidence_refs = evidence_refs;
    let mut completed = report.checks.clone();
    if completed.is_empty() && input.dispatched_nodes > 0 {
        completed.push(format!("Dispatched {} node(s).", input.dispatched_nodes));
    }
    report.refresh_planner_style_summary(Some(input.request), &completed);
    report
}

fn plan_context_summary(plan_context: Option<&Value>) -> Option<String> {
    let plan_context = plan_context?;
    let summary = plan_context
        .get("plan_draft")
        .and_then(|plan| plan.get("goal"))
        .and_then(Value::as_str)
        .or_else(|| {
            plan_context
                .get("original_user_request")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            plan_context
                .get("planner_conversation_summary")
                .and_then(Value::as_str)
        })?
        .trim();
    if summary.is_empty() {
        None
    } else {
        Some(summary.chars().take(240).collect())
    }
}

fn plan_acceptance_criteria(plan_context: Option<&Value>) -> Vec<String> {
    let Some(plan_context) = plan_context else {
        return Vec::new();
    };
    let direct = string_array(plan_context.get("acceptance_criteria"));
    if !direct.is_empty() {
        return direct;
    }
    string_array(
        plan_context
            .get("plan_draft")
            .and_then(|plan| plan.get("acceptance_criteria")),
    )
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn run_status_str(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Queued => "queued",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Blocked => "blocked",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}

fn openhands_server_config_from_harness(config: &OpenHandsHarnessConfig) -> OpenHandsServerConfig {
    OpenHandsServerConfig {
        server_url: config.server_url.clone(),
        session_api_key_env: config.session_api_key_env.clone(),
        session_api_key: config.session_api_key.clone(),
        api_paths: openhands_api_paths_from_config(&config.api_paths),
        run_start_strategy: openhands_run_strategy_from_config(config.run_start_strategy),
    }
}

fn openhands_api_paths_from_config(paths: &ConfigOpenHandsApiPaths) -> OpenHandsApiPaths {
    OpenHandsApiPaths {
        api_prefix: paths.api_prefix.clone(),
        conversations_path: paths.conversations_path.clone(),
        events_search_path: paths.events_search_path.clone(),
        run_endpoint_path: paths.run_endpoint_path.clone(),
        websocket_path_template: paths.websocket_path_template.clone(),
        auth_header: match paths.auth_header {
            ConfigOpenHandsAuthHeaderMode::AuthorizationBearer => {
                OpenHandsAuthHeaderMode::AuthorizationBearer
            }
            ConfigOpenHandsAuthHeaderMode::XSessionApiKey => {
                OpenHandsAuthHeaderMode::XSessionApiKey
            }
        },
    }
}

fn openhands_run_strategy_from_config(
    strategy: ConfigOpenHandsRunStartStrategy,
) -> OpenHandsRunStartStrategy {
    match strategy {
        ConfigOpenHandsRunStartStrategy::PostRunEndpoint => {
            OpenHandsRunStartStrategy::PostRunEndpoint
        }
        ConfigOpenHandsRunStartStrategy::PostUserEventWithRunTrue => {
            OpenHandsRunStartStrategy::PostUserEventWithRunTrue
        }
        ConfigOpenHandsRunStartStrategy::None => OpenHandsRunStartStrategy::None,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        io::{Read, Write},
        net::TcpListener,
        path::PathBuf,
        sync::{Arc, Mutex},
        thread,
        time::Duration,
    };

    use coder_config::{MemoryScope, ProjectConfig, WorkflowNodeSpec};
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

    #[tokio::test]
    async fn workflow_runner_native_rust_read_only_review_writes_evidence() {
        let (mut config, root, store) = fixture();
        let repo = temp_root();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("README.md"), "# Native review\n").unwrap();
        fs::write(
            repo.join("src").join("lib.rs"),
            "pub fn answer() -> u8 { 42 }\n",
        )
        .unwrap();
        make_single_node_terminal_workflow(&mut config);
        config.harnesses.get_mut("review-only").unwrap().tools = vec![
            "repo_find_files".to_owned(),
            "repo_read_file_range".to_owned(),
            "git_diff".to_owned(),
        ];
        let runner = WorkflowRunner::new(config, store.clone());
        let mut options = WorkflowRunOptions::new("planner-led", "review README.md for TODO");
        options.repo_root = repo.clone();

        let output = runner.run(options).await.unwrap();
        let events = store.read_events(&output.run_id).unwrap();
        let evidence = store.list_repo_evidence(&output.run_id).unwrap();

        assert_eq!(output.report.status, ReportStatus::Completed);
        assert_eq!(events.first().unwrap().kind, "run.started");
        assert_eq!(events.last().unwrap().kind, "run.completed");
        assert!(events
            .iter()
            .any(|event| event.kind == "backend.native_rust.completed"));
        assert!(events.iter().any(|event| {
            event.kind == "native.tool.completed"
                && event.payload["tool"].as_str() == Some("repo_find_files")
        }));
        assert!(evidence
            .iter()
            .any(|item| item.kind == RepoEvidenceKind::RepoFileList));
        assert!(evidence
            .iter()
            .any(|item| item.kind == RepoEvidenceKind::RepoRead));
        assert!(output
            .report
            .evidence_refs
            .iter()
            .any(|item| item.reference.starts_with("repo-evidence://")));
        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn native_react_lifecycle_records_reason_act_observe_steps() {
        let (mut config, root, store) = fixture();
        let repo = temp_root();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("README.md"),
            "# Native review\nTODO: tighten docs\n",
        )
        .unwrap();
        fs::write(
            repo.join("src").join("lib.rs"),
            "pub fn answer() -> u8 { 42 }\n",
        )
        .unwrap();
        make_single_node_terminal_workflow(&mut config);
        config.harnesses.get_mut("review-only").unwrap().tools = vec![
            "repo_find_files".to_owned(),
            "repo_read_file_range".to_owned(),
            "git_diff".to_owned(),
        ];
        let runner = WorkflowRunner::new(config, store.clone());
        let mut options = WorkflowRunOptions::new("planner-led", "review README.md for TODO");
        options.repo_root = repo.clone();

        let output = runner.run(options).await.unwrap();
        let events = store.read_events(&output.run_id).unwrap();
        let reasoning = events
            .iter()
            .filter(|event| event.kind == "executor.reasoning_summary")
            .collect::<Vec<_>>();
        let actions = events
            .iter()
            .filter(|event| event.kind == "executor.action_selected")
            .collect::<Vec<_>>();
        let observations = events
            .iter()
            .filter(|event| event.kind == "observation.recorded")
            .collect::<Vec<_>>();

        assert_eq!(output.report.status, ReportStatus::Completed);
        assert!(reasoning.len() >= 2);
        assert!(actions.len() >= 2);
        assert!(events.iter().any(|event| event.kind == "tool.started"));
        assert!(events.iter().any(|event| event.kind == "tool.completed"));
        assert!(observations.len() >= 2);
        assert!(reasoning[1]
            .payload
            .get("previous_observation")
            .and_then(Value::as_str)
            .unwrap()
            .contains("repo_find_files"));
        assert!(events.iter().any(|event| {
            event.kind == "executor.next_step"
                && event.payload["based_on_observation"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("repo_find_files")
                && event.payload["next_tool"].as_str() == Some("repo_read_file_range")
        }));
        assert!(events
            .iter()
            .any(|event| event.kind == "executor.completed"));
        for event in events.iter().filter(|event| {
            matches!(
                event.kind.as_str(),
                "executor.reasoning_summary"
                    | "executor.action_selected"
                    | "tool.started"
                    | "tool.completed"
                    | "observation.recorded"
                    | "executor.next_step"
                    | "executor.completed"
                    | "executor.blocked"
                    | "executor.failed"
            )
        }) {
            assert_eq!(event.payload["run_id"], output.run_id.as_str());
            assert_eq!(event.payload["workflow_id"], "planner-led");
            assert_eq!(event.payload["node_id"], "review");
            assert_eq!(event.payload["agent_id"], "executor");
            assert_eq!(event.payload["harness_id"], "review-only");
            assert_eq!(event.payload["backend"], "native-rust");
            assert!(event.payload["step"].as_u64().is_some());
        }
        assert!(output
            .report
            .evidence_refs
            .iter()
            .any(|item| item.reference.starts_with("repo-evidence://")));
        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_native_rust_patch_preview_records_diff_evidence() {
        let (mut config, root, store) = fixture();
        let repo = temp_root();
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
        make_single_node_terminal_workflow(&mut config);
        config.harnesses.get_mut("review-only").unwrap().tools = vec!["patch_preview".to_owned()];
        let runner = WorkflowRunner::new(config, store.clone());
        let mut options = WorkflowRunOptions::new("planner-led", "preview change.patch");
        options.repo_root = repo.clone();

        let output = runner.run(options).await.unwrap();
        let events = store.read_events(&output.run_id).unwrap();
        let evidence = store.list_repo_evidence(&output.run_id).unwrap();

        assert_eq!(output.report.status, ReportStatus::Completed);
        assert_eq!(output.report.changed_files, vec!["tracked.txt"]);
        assert_eq!(output.report.patch_refs.len(), 1);
        assert!(events.iter().any(|event| {
            event.kind == "native.tool.completed"
                && event.payload["tool"].as_str() == Some("patch_preview")
        }));
        assert!(evidence
            .iter()
            .any(|item| item.kind == RepoEvidenceKind::RepoDiff));
        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_native_rust_patch_apply_requires_approval() {
        let (mut config, root, store) = fixture();
        let repo = temp_root();
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
        make_single_node_terminal_workflow(&mut config);
        config.harnesses.get_mut("review-only").unwrap().tools = vec!["patch_apply".to_owned()];
        let runner = WorkflowRunner::new(config, store.clone());
        let mut options = WorkflowRunOptions::new("planner-led", "apply patch change.patch");
        options.repo_root = repo.clone();

        let output = runner.run(options).await.unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        assert_eq!(output.report.status, ReportStatus::Blocked);
        assert!(output.report.blockers[0].contains("requires approval"));
        assert_eq!(
            fs::read_to_string(repo.join("tracked.txt")).unwrap(),
            "base\n"
        );
        assert!(events.iter().any(|event| {
            event.kind == "approval.requested"
                && event.payload["approval_type"].as_str() == Some("patch_apply")
        }));
        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_native_rust_command_run_requires_approval() {
        let (mut config, root, store) = fixture();
        let repo = temp_root();
        fs::create_dir_all(&repo).unwrap();
        make_single_node_terminal_workflow(&mut config);
        config.harnesses.get_mut("review-only").unwrap().tools = vec!["command_run".to_owned()];
        let runner = WorkflowRunner::new(config, store.clone());
        let mut options = WorkflowRunOptions::new("planner-led", "run command: definitely-not-run");
        options.repo_root = repo.clone();

        let output = runner.run(options).await.unwrap();
        let events = store.read_events(&output.run_id).unwrap();
        let evidence = store.list_repo_evidence(&output.run_id).unwrap();

        assert_eq!(output.report.status, ReportStatus::Blocked);
        assert!(events.iter().any(|event| {
            event.kind == "approval.requested"
                && event.payload["approval_type"].as_str() == Some("command")
        }));
        assert!(events.iter().any(|event| event.kind == "executor.blocked"));
        assert!(events.iter().any(|event| {
            event.kind == "tool.completed"
                && event.payload["tool_name"].as_str() == Some("command_run")
                && event.payload["status"].as_str() == Some("blocked")
        }));
        assert!(evidence
            .iter()
            .any(|item| item.kind == RepoEvidenceKind::RepoTest));
        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_native_mock_blocked() {
        let (mut config, root, store) = fixture();
        make_single_node_terminal_workflow(&mut config);
        make_workflow_native_only(&mut config);
        let registry = BackendRegistry::native_only()
            .with_native_backend(Arc::new(NativeMockBackend::new(NativeMockOutcome::Blocked)));
        let runner = WorkflowRunner::with_registry(config, store.clone(), registry);

        let output = runner
            .run(WorkflowRunOptions::new("planner-led", "blocked task"))
            .await
            .unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        assert_eq!(output.report.status, ReportStatus::Blocked);
        assert!(output.report.blockers[0].contains("blocked outcome"));
        assert!(events.iter().any(|event| {
            event.kind == "round.completed" && event.payload["status"].as_str() == Some("blocked")
        }));
        assert_eq!(events.last().unwrap().kind, "run.blocked");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_follows_planner_executor_planner_loop() {
        let (mut config, root, store) = fixture();
        make_workflow_native_only(&mut config);
        let registry =
            BackendRegistry::native_only().with_native_backend(Arc::new(ScriptedBackend::new([
                "ready",
                "completed",
                "finish",
            ])));
        let runner = WorkflowRunner::with_registry(config, store.clone(), registry);

        let output = runner
            .run(WorkflowRunOptions::new("planner-led", "loop task"))
            .await
            .unwrap();
        let events = store.read_events(&output.run_id).unwrap();
        let transitions = events
            .iter()
            .filter(|event| event.kind == "workflow.transition.selected")
            .map(|event| {
                (
                    event.payload["from"].as_str().unwrap().to_owned(),
                    event.payload["to"].as_str().unwrap().to_owned(),
                    event.payload["on"].as_str().unwrap().to_owned(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(output.report.status, ReportStatus::Completed);
        assert_eq!(
            transitions,
            vec![
                (
                    "planner".to_owned(),
                    "executor".to_owned(),
                    "ready".to_owned()
                ),
                (
                    "executor".to_owned(),
                    "planner".to_owned(),
                    "completed".to_owned()
                )
            ]
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "round.started")
                .count(),
            2
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_completed_terminal_stop() {
        let (mut config, root, store) = fixture();
        make_single_node_terminal_workflow(&mut config);
        let runner = workflow_runner_with_script(config, store.clone(), ["completed"]);

        let output = runner
            .run(WorkflowRunOptions::new("planner-led", "terminal completed"))
            .await
            .unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        assert_eq!(output.report.status, ReportStatus::Completed);
        assert_eq!(events.last().unwrap().kind, "run.completed");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_blocked_terminal_stop() {
        let (mut config, root, store) = fixture();
        make_single_node_terminal_workflow(&mut config);
        let runner = workflow_runner_with_script(config, store.clone(), ["blocked"]);

        let output = runner
            .run(WorkflowRunOptions::new("planner-led", "terminal blocked"))
            .await
            .unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        assert_eq!(output.report.status, ReportStatus::Blocked);
        assert!(events.iter().any(|event| event.kind == "node.blocked"));
        assert_eq!(events.last().unwrap().kind, "run.blocked");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_failed_terminal_stop() {
        let (mut config, root, store) = fixture();
        make_single_node_terminal_workflow(&mut config);
        let runner = workflow_runner_with_script(config, store.clone(), ["failed"]);

        let output = runner
            .run(WorkflowRunOptions::new("planner-led", "terminal failed"))
            .await
            .unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        assert_eq!(output.report.status, ReportStatus::Failed);
        assert!(events.iter().any(|event| event.kind == "node.failed"));
        assert_eq!(events.last().unwrap().kind, "run.failed");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_cancelled_terminal_stop() {
        let (mut config, root, store) = fixture();
        make_single_node_terminal_workflow(&mut config);
        let runner = workflow_runner_with_script(config, store.clone(), ["cancelled"]);

        let output = runner
            .run(WorkflowRunOptions::new("planner-led", "terminal cancelled"))
            .await
            .unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        assert_eq!(output.report.status, ReportStatus::Cancelled);
        assert!(events.iter().any(|event| event.kind == "node.cancelled"));
        assert_eq!(events.last().unwrap().kind, "run.cancelled");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_blocks_on_no_matching_transition() {
        let (mut config, root, store) = fixture();
        make_single_node_terminal_workflow(&mut config);
        let runner = workflow_runner_with_script(config, store.clone(), ["ready"]);

        let output = runner
            .run(WorkflowRunOptions::new("planner-led", "missing edge"))
            .await
            .unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        assert_eq!(output.report.status, ReportStatus::Blocked);
        assert!(events
            .iter()
            .any(|event| event.kind == "workflow.transition.missing"));
        assert!(output
            .report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("no matching workflow transition")));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_blocks_when_openhands_unavailable() {
        let (mut config, root, store) = fixture();
        let openhands = config
            .harnesses
            .get_mut("openhands-code-edit")
            .unwrap()
            .openhands
            .as_mut()
            .unwrap();
        openhands.server_url = "http://127.0.0.1:1".to_owned();
        let runner = WorkflowRunner::new(config, store.clone());
        let mut options = WorkflowRunOptions::new("planner-led", "needs openhands");
        options.repo_root = root.clone();

        let output = runner.run(options).await.unwrap();

        assert_eq!(output.report.status, ReportStatus::Blocked);
        let events = store.read_events(&output.run_id).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "backend.selected"
                && event.payload["backend"].as_str() == Some("openhands")
                && event.payload["fallback_allowed"].as_bool() == Some(false)
        }));
        assert!(events.iter().any(|event| {
            event.kind == "backend.blocked"
                && event.payload["backend"].as_str() == Some("openhands")
                && event.payload["fallback_allowed"].as_bool() == Some(false)
        }));
        assert!(!events.iter().any(|event| {
            event.kind == "backend.selected"
                && event.payload["backend"].as_str() == Some("native-rust")
                && event.payload["fallback_for"].as_str() == Some("openhands")
        }));
        assert!(output
            .report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("backend 'openhands' unavailable")));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_uses_native_fallback_when_openhands_unavailable_and_allowed() {
        let (mut config, root, store) = fixture();
        let workflow = config.workflows.get_mut("planner-led").unwrap();
        workflow.nodes = vec![WorkflowNodeSpec {
            id: "executor".to_owned(),
            agent: "executor".to_owned(),
            harness: "openhands-code-edit".to_owned(),
        }];
        workflow.edges.clear();
        let openhands = config
            .harnesses
            .get_mut("openhands-code-edit")
            .unwrap()
            .openhands
            .as_mut()
            .unwrap();
        openhands.server_url = "http://127.0.0.1:1".to_owned();
        let runner = WorkflowRunner::new(config, store.clone());
        let mut options = WorkflowRunOptions::new("planner-led", "needs openhands");
        options.repo_root = root.clone();
        options.allow_native_fallback_for_openhands = true;

        let output = runner.run(options).await.unwrap();

        assert_eq!(output.report.status, ReportStatus::Completed);
        assert!(output
            .report
            .checks
            .iter()
            .any(|check| check == "node executor via native-rust: completed"));
        let events = store.read_events(&output.run_id).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "backend.selected"
                && event.payload["backend"].as_str() == Some("openhands")
        }));
        assert!(events.iter().any(|event| {
            event.kind == "backend.blocked"
                && event.payload["backend"].as_str() == Some("openhands")
                && event.payload["fallback_allowed"].as_bool() == Some(true)
        }));
        assert!(events.iter().any(|event| {
            event.kind == "backend.selected"
                && event.payload["backend"].as_str() == Some("native-rust")
                && event.payload["fallback_for"].as_str() == Some("openhands")
                && event.payload["summary"].as_str() == Some("Executor backend: native fallback")
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_reports_unknown_backend() {
        let (mut config, root, store) = fixture();
        config
            .harnesses
            .get_mut("openhands-code-edit")
            .unwrap()
            .backend = "mystery-backend".to_owned();
        let runner = WorkflowRunner::new(config, store);
        let mut options = WorkflowRunOptions::new("planner-led", "unknown backend");
        options.repo_root = root.clone();

        let error = runner.run(options).await.unwrap_err();

        assert!(matches!(error, WorkflowError::BackendNotFound(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_blocks_when_max_rounds_override_exceeds_spec() {
        let (mut config, root, store) = fixture();
        make_workflow_native_only(&mut config);
        config.workflows.get_mut("planner-led").unwrap().max_rounds = 2;
        let runner = WorkflowRunner::new(config, store.clone());
        let mut options = WorkflowRunOptions::new("planner-led", "too many rounds");
        options.repo_root = root.clone();
        options.max_rounds_override = Some(3);

        let output = runner.run(options).await.unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        assert_eq!(output.report.status, ReportStatus::Blocked);
        assert!(output.report.blockers[0].contains("max_rounds"));
        assert!(events
            .iter()
            .any(|event| event.kind == "workflow.max_rounds_reached"));
        assert_eq!(events.last().unwrap().kind, "run.blocked");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_event_sequence_is_monotonic() {
        let (mut config, root, store) = fixture();
        make_workflow_native_only(&mut config);
        let runner = WorkflowRunner::new(config, store.clone());

        let output = runner
            .run(WorkflowRunOptions::new("planner-led", "sequence task"))
            .await
            .unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        for (index, event) in events.iter().enumerate() {
            assert_eq!(event.sequence, index as u64 + 1);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_final_report_has_event_log_evidence() {
        let (mut config, root, store) = fixture();
        make_workflow_native_only(&mut config);
        let runner = WorkflowRunner::new(config, store);

        let output = runner
            .run(WorkflowRunOptions::new("planner-led", "evidence task"))
            .await
            .unwrap();

        assert!(output
            .report
            .evidence_refs
            .iter()
            .any(|reference| reference.kind == "event_log"));
        assert!(output.report.summary.contains("Requested: evidence task"));
        assert!(output.report.summary.contains("Done:"));
        assert!(output.report.summary.contains("Verification:"));
        assert!(output.report.summary.contains("Evidence:"));
        assert!(output.report.summary.contains("Remaining risks:"));
        assert!(output.report.summary.contains("Next steps:"));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_replays_terminal_status_from_events() {
        let (mut config, root, store) = fixture();
        make_single_node_terminal_workflow(&mut config);
        let runner = workflow_runner_with_script(config, store.clone(), ["failed"]);

        let output = runner
            .run(WorkflowRunOptions::new("planner-led", "replay task"))
            .await
            .unwrap();
        let events = store.read_events(&output.run_id).unwrap();

        assert_eq!(replay_run_status(&events), Some(RunStatus::Failed));
        let metadata = store.read_metadata(&output.run_id).unwrap().unwrap();
        assert_eq!(replay_run_status(&events), Some(metadata.status));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_rejects_invalid_edge_target_before_runtime() {
        let (mut config, root, store) = fixture();
        config
            .workflows
            .get_mut("planner-led")
            .unwrap()
            .edges
            .push(WorkflowEdgeSpec {
                from: "planner".to_owned(),
                to: "missing".to_owned(),
                on: "ready".to_owned(),
            });
        let runner = WorkflowRunner::new(config, store);

        let error = runner
            .run(WorkflowRunOptions::new("planner-led", "invalid edge"))
            .await
            .unwrap_err();

        assert!(matches!(error, WorkflowError::InvalidConfig(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_rejects_duplicate_node_ids_before_runtime() {
        let (mut config, root, store) = fixture();
        let workflow = config.workflows.get_mut("planner-led").unwrap();
        workflow.nodes.push(workflow.nodes[0].clone());
        let runner = WorkflowRunner::new(config, store);

        let error = runner
            .run(WorkflowRunOptions::new("planner-led", "duplicate node"))
            .await
            .unwrap_err();

        assert!(matches!(error, WorkflowError::InvalidConfig(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workflow_runner_rejects_missing_start_node_before_runtime() {
        let (mut config, root, store) = fixture();
        let workflow = config.workflows.get_mut("planner-led").unwrap();
        workflow.nodes.clear();
        workflow.edges.clear();
        let runner = WorkflowRunner::new(config, store);

        let error = runner
            .run(WorkflowRunOptions::new("planner-led", "missing start"))
            .await
            .unwrap_err();

        assert!(matches!(error, WorkflowError::InvalidConfig(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn openhands_payload_projects_coder_specs_without_secret_values() {
        let (config, root, _) = fixture();
        let secret_value = "actual-secret-value";
        let workflow = config.workflows.get("planner-led").unwrap();
        let node = workflow
            .nodes
            .iter()
            .find(|node| node.id == "executor")
            .unwrap();
        let agent = config.agents.get(&node.agent).unwrap();
        let harness = config.harnesses.get(&node.harness).unwrap();
        let model = config.models.get(&agent.model).unwrap();
        let run_id = RunId::from_string("run-phase2");
        let plan_context = json!({
            "original_user_request": "Update planner loop",
            "acceptance_criteria": ["planner criteria reached"],
            "plan_draft": {
                "goal": "Update planner loop",
                "affected_paths": ["crates/coder-workflow/src/lib.rs"]
            }
        });

        let payload = build_openhands_conversation_payload(OpenHandsConversationPayloadInput {
            run_id: &run_id,
            repo_root: root.to_str().unwrap(),
            workflow_id: "planner-led",
            workflow,
            node,
            agent_id: &node.agent,
            agent,
            harness_id: &node.harness,
            harness,
            model,
            plan_context: Some(&plan_context),
        });
        let payload_text = serde_json::to_string(&payload).unwrap();

        assert_eq!(payload["agent"]["kind"], "Agent");
        assert_eq!(payload["agent"]["llm"]["model"], "best");
        assert!(payload["agent"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "terminal"));
        assert_eq!(payload["workspace"]["kind"], "LocalWorkspace");
        assert_eq!(payload["workspace"]["working_dir"], root.to_str().unwrap());
        assert_eq!(
            payload["metadata"]["coder"]["agent_kind_source"],
            "default_fallback"
        );
        assert_eq!(payload["metadata"]["coder"]["run_id"], "run-phase2");
        assert_eq!(
            payload["metadata"]["coder"]["repo_root"],
            root.to_str().unwrap()
        );
        assert_eq!(payload["metadata"]["coder"]["workflow_id"], "planner-led");
        assert_eq!(payload["metadata"]["coder"]["node_id"], "executor");
        assert!(payload["metadata"]["coder"]["selected_tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool.as_str() == Some("terminal")));
        assert_eq!(
            payload["metadata"]["coder"]["model"]["profile_ref"],
            "default"
        );
        assert_eq!(
            payload["metadata"]["coder"]["model"]["api_key_env"],
            "LLM_API_KEY"
        );
        assert_eq!(
            payload["metadata"]["coder"]["permissions"]["summary"]["write_files"],
            "ask"
        );
        assert_eq!(
            payload["coder_context"]["agent"]["output_contract"],
            "execution_result"
        );
        assert_eq!(
            payload["coder_context"]["run"]["repo_root"],
            root.to_str().unwrap()
        );
        assert!(payload["coder_context"]["agent"]["system_instructions"]
            .as_str()
            .unwrap()
            .contains("coding executor"));
        assert_eq!(
            payload["coder_context"]["memory"]["note"],
            "scope names only; memory contents are not embedded"
        );
        assert_eq!(
            payload["metadata"]["coder"]["plan_context"]["acceptance_criteria"][0],
            "planner criteria reached"
        );
        assert_eq!(
            payload["coder_context"]["plan_context"]["plan_draft"]["goal"],
            "Update planner loop"
        );
        assert!(!payload_text.contains(secret_value));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn openhands_backend_prefers_context_payload_and_has_minimal_fallback() {
        let request = HarnessRunRequest {
            run_id: RunId::from_string("run-context"),
            workflow_id: "workflow".to_owned(),
            node_id: "node".to_owned(),
            agent_id: "agent".to_owned(),
            harness_id: "harness".to_owned(),
            repo_root: ".".to_owned(),
            task: "task".to_owned(),
            backend_context: json!({
                "openhands": {
                    "create_conversation_payload": {
                        "agent": {"kind": "CustomAgent"},
                        "metadata": {"coder": {"marker": "from-context"}}
                    }
                }
            }),
        };
        let fallback_request = HarnessRunRequest {
            backend_context: Value::Null,
            ..request.clone()
        };

        let payload = openhands_create_conversation_payload(&request);
        let fallback = openhands_create_conversation_payload(&fallback_request);

        assert_eq!(payload["metadata"]["coder"]["marker"], "from-context");
        assert_eq!(payload["workspace"]["kind"], "LocalWorkspace");
        assert_eq!(payload["workspace"]["working_dir"], ".");
        assert_eq!(fallback["agent"]["kind"], "Agent");
        assert_eq!(fallback["workspace"]["kind"], "LocalWorkspace");
        assert_eq!(fallback["workspace"]["working_dir"], ".");
        assert_eq!(fallback["metadata"]["coder"]["run_id"], "run-context");
        assert_eq!(
            fallback["metadata"]["coder"]["agent_kind_source"],
            "default_fallback"
        );
    }

    #[tokio::test]
    async fn openhands_backend_polls_until_terminal_and_stores_raw_refs() {
        let (server_url, requests) = spawn_openhands_server(vec![
            json_response(r#"{"status":"ok"}"#),
            json_response(r#"{"id":"conv-1"}"#),
            json_response(r#"{"accepted":true}"#),
            json_response(r#"[{"id":"raw-1","type":"message","content":"working"}]"#),
            json_response(
                r#"[{"id":"raw-1","type":"message","content":"working"},{"id":"raw-2","type":"done","status":"completed","api_key":"secret"}]"#,
            ),
        ]);
        let root = temp_root();
        let store = RunStore::new(&root);
        let backend =
            OpenHandsHarnessBackend::new(openhands_test_config(server_url, 10, 0), store.clone());
        let request = openhands_test_request("poll terminal");

        let result = backend.run(request).await.unwrap();

        assert_eq!(result.status, "completed");
        assert!(result
            .events
            .iter()
            .any(|event| event.kind == "backend.openhands.done"));
        let raw_refs = result
            .events
            .iter()
            .flat_map(|event| event.refs.iter())
            .filter(|reference| reference.label == "openhands.raw_event")
            .map(|reference| reference.uri.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(raw_refs.len(), 2);
        assert!(
            result
                .events
                .iter()
                .filter(|event| event.payload.get("raw").is_some())
                .count()
                == 0
        );
        assert!(result
            .report
            .unwrap()
            .evidence_refs
            .iter()
            .any(|reference| reference.kind == "openhands_raw_event"));
        let request_log = requests.lock().unwrap().join("\n");
        assert!(request_log.contains("GET /health "));
        assert!(request_log.contains("POST /conversations "));
        assert!(request_log.contains("POST /conversations/conv-1/events "));
        assert!(request_log.contains("GET /conversations/conv-1/events "));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn openhands_react_events_map_actions_observations_and_terminal_status() {
        let (server_url, _) = spawn_openhands_server(vec![
            json_response(r#"{"status":"ok"}"#),
            json_response(r#"{"id":"conv-1"}"#),
            json_response(r#"{"accepted":true}"#),
            json_response(
                r#"[
                    {"id":"raw-1","type":"ThoughtEvent","thought":"I should run the test suite."},
                    {"id":"raw-2","type":"ActionEvent","tool_name":"shell","command":"cargo test","cwd":".","message":"Run tests"},
                    {"id":"raw-3","type":"ObservationEvent","tool_name":"shell","command":"cargo test","content":"tests passed","returncode":0,"stdout":"ok"},
                    {"id":"raw-4","type":"FileEditObservation","files":[{"path":"src/lib.rs","status":"modified","additions":1,"deletions":0}],"content":"updated src/lib.rs"},
                    {"id":"raw-5","type":"done","status":"completed"}
                ]"#,
            ),
        ]);
        let root = temp_root();
        let store = RunStore::new(&root);
        let backend =
            OpenHandsHarnessBackend::new(openhands_test_config(server_url, 10, 0), store.clone());
        let request = openhands_test_request("react terminal");

        let result = backend.run(request).await.unwrap();

        assert_eq!(result.status, "completed");
        assert!(result
            .events
            .iter()
            .any(|event| event.kind == "executor.reasoning_summary"
                && event.payload["summary"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("test suite")));
        assert!(result
            .events
            .iter()
            .any(|event| event.kind == "executor.action_selected"
                && event.payload["tool_name"].as_str() == Some("shell")));
        assert!(result
            .events
            .iter()
            .any(|event| event.kind == "tool.started"));
        assert!(result
            .events
            .iter()
            .any(|event| event.kind == "tool.completed"));
        assert!(result
            .events
            .iter()
            .any(|event| event.kind == "observation.recorded"
                && event.payload["summary"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("tests passed")));
        assert!(result
            .events
            .iter()
            .any(|event| event.kind == "executor.next_step"));
        assert!(result.events.iter().any(|event| {
            event.kind == "command.completed"
                && event.payload["command"].as_str() == Some("cargo test")
                && event.payload["returncode"].as_i64() == Some(0)
        }));
        assert!(result.events.iter().any(|event| {
            event.kind == "patch.applied"
                && event
                    .payload
                    .get("files")
                    .and_then(Value::as_array)
                    .is_some_and(|files| {
                        files.iter().any(|file| {
                            file.get("path").and_then(Value::as_str) == Some("src/lib.rs")
                                && file.get("status").and_then(Value::as_str) == Some("modified")
                        })
                    })
        }));
        assert!(result
            .events
            .iter()
            .any(|event| event.kind == "executor.completed"));
        for event in result.events.iter().filter(|event| {
            matches!(
                event.kind.as_str(),
                "executor.reasoning_summary"
                    | "executor.action_selected"
                    | "tool.started"
                    | "tool.completed"
                    | "observation.recorded"
                    | "executor.next_step"
                    | "executor.completed"
                    | "executor.blocked"
                    | "executor.failed"
                    | "command.previewed"
                    | "command.completed"
                    | "command.failed"
                    | "patch.applied"
            )
        }) {
            assert_eq!(event.payload["run_id"], "run-openhands-test");
            assert_eq!(event.payload["workflow_id"], "workflow");
            assert_eq!(event.payload["node_id"], "executor");
            assert_eq!(event.payload["agent_id"], "executor");
            assert_eq!(event.payload["harness_id"], "openhands-code-edit");
            assert!(event.payload["step"].as_u64().is_some());
        }
        assert!(
            result
                .events
                .iter()
                .filter(|event| event.payload.get("raw").is_some())
                .count()
                == 0
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn openhands_finish_tool_event_completes_run() {
        let (server_url, _) = spawn_openhands_server(vec![
            json_response(r#"{"status":"ok"}"#),
            json_response(r#"{"id":"conv-1"}"#),
            json_response(r#"{"accepted":true}"#),
            json_response(
                r#"[
                    {"id":"raw-1","kind":"ActionEvent","tool_name":"finish","summary":"Live smoke result documented","action":{"kind":"FinishAction","message":"complete"}},
                    {"id":"raw-2","kind":"ObservationEvent","tool_name":"finish","observation":{"kind":"FinishObservation","content":"done"}}
                ]"#,
            ),
        ]);
        let root = temp_root();
        let store = RunStore::new(&root);
        let backend =
            OpenHandsHarnessBackend::new(openhands_test_config(server_url, 10, 0), store.clone());
        let request = openhands_test_request("finish tool terminal");

        let result = backend.run(request).await.unwrap();

        assert_eq!(result.status, "completed");
        assert!(result.events.iter().any(|event| {
            event.kind == "executor.completed"
                && event.payload["tool_name"].as_str() == Some("finish")
        }));
        assert_eq!(result.report.unwrap().status, ReportStatus::Completed);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn openhands_backend_blocks_on_poll_timeout() {
        let (server_url, _) = spawn_openhands_server(vec![
            json_response(r#"{"status":"ok"}"#),
            json_response(r#"{"id":"conv-1"}"#),
            json_response(r#"{"accepted":true}"#),
            json_response(r#"[]"#),
        ]);
        let root = temp_root();
        let store = RunStore::new(&root);
        let backend = OpenHandsHarnessBackend::new(openhands_test_config(server_url, 0, 0), store);
        let request = openhands_test_request("timeout");

        let result = backend.run(request).await.unwrap();

        assert_eq!(result.status, "blocked");
        assert!(result
            .events
            .iter()
            .any(|event| event.kind == "backend.openhands.timeout"));
        assert_eq!(result.report.unwrap().status, ReportStatus::Blocked);
        let _ = fs::remove_dir_all(root);
    }

    fn workflow_runner_with_script<I, S>(
        mut config: ProjectConfig,
        store: RunStore,
        statuses: I,
    ) -> WorkflowRunner
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        make_workflow_native_only(&mut config);
        let registry = BackendRegistry::native_only()
            .with_native_backend(Arc::new(ScriptedBackend::new(statuses)));
        WorkflowRunner::with_registry(config, store, registry)
    }

    struct ScriptedBackend {
        statuses: Mutex<VecDeque<String>>,
    }

    impl ScriptedBackend {
        fn new<I, S>(statuses: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: Into<String>,
        {
            Self {
                statuses: Mutex::new(statuses.into_iter().map(Into::into).collect()),
            }
        }
    }

    #[async_trait]
    impl HarnessBackend for ScriptedBackend {
        async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
            let status = self
                .statuses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| "completed".to_owned());
            let report = match status.as_str() {
                "blocked" => FinalReport::blocked("Scripted backend blocked.", "scripted blocked"),
                "failed" => FinalReport::failed("Scripted backend failed.", "scripted failed"),
                "cancelled" => {
                    FinalReport::with_status(ReportStatus::Cancelled, "Scripted backend cancelled.")
                }
                _ => FinalReport::completed("Scripted backend completed."),
            };
            Ok(HarnessRunResult {
                status: status.clone(),
                report: Some(report),
                events: vec![HarnessRunEvent::new(
                    format!("backend.scripted.{status}"),
                    json!({
                        "node_id": request.node_id,
                        "agent_id": request.agent_id,
                        "status": status
                    }),
                )],
            })
        }
    }

    fn openhands_test_config(
        server_url: String,
        max_event_poll_seconds: u64,
        poll_interval_ms: u64,
    ) -> OpenHandsHarnessConfig {
        OpenHandsHarnessConfig {
            server_url,
            session_api_key_env: None,
            session_api_key: None,
            workspace_mode: Some("local".to_owned()),
            prefer_websocket: false,
            poll_interval_ms,
            max_event_poll_seconds,
            max_events: 10,
            terminal_event_kinds: vec!["done".to_owned()],
            api_paths: ConfigOpenHandsApiPaths::default(),
            run_start_strategy: ConfigOpenHandsRunStartStrategy::PostUserEventWithRunTrue,
        }
    }

    fn openhands_test_request(task: &str) -> HarnessRunRequest {
        HarnessRunRequest {
            run_id: RunId::from_string("run-openhands-test"),
            workflow_id: "workflow".to_owned(),
            node_id: "executor".to_owned(),
            agent_id: "executor".to_owned(),
            harness_id: "openhands-code-edit".to_owned(),
            repo_root: ".".to_owned(),
            task: task.to_owned(),
            backend_context: Value::Null,
        }
    }

    fn spawn_openhands_server(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_for_thread = Arc::clone(&requests);
        let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
        thread::spawn(move || {
            while !responses.lock().unwrap().is_empty() {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let request = read_request(&mut stream);
                requests_for_thread.lock().unwrap().push(request);
                let response = responses.lock().unwrap().pop_front().unwrap();
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{address}"), requests)
    }

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0; 1024];
        loop {
            let read = stream.read(&mut chunk).unwrap_or(0);
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if request_is_complete(&buffer) {
                break;
            }
        }
        String::from_utf8_lossy(&buffer).into_owned()
    }

    fn request_is_complete(buffer: &[u8]) -> bool {
        let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let headers = String::from_utf8_lossy(&buffer[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        buffer.len() >= header_end + 4 + content_length
    }

    fn json_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn fixture() -> (ProjectConfig, PathBuf, RunStore) {
        let config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        let root = temp_root();
        let store = RunStore::new(&root);
        (config, root, store)
    }

    fn temp_root() -> PathBuf {
        static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("coder-workflow-{}-{}", std::process::id(), id))
    }

    fn make_workflow_native_only(config: &mut ProjectConfig) {
        for harness in config.harnesses.values_mut() {
            harness.backend = "native-rust".to_owned();
            harness.openhands = None;
            harness.memory.read = vec![MemoryScope::Workflow, MemoryScope::Run];
            harness.memory.write = vec![MemoryScope::Run];
        }
    }

    fn make_single_node_terminal_workflow(config: &mut ProjectConfig) {
        let workflow = config.workflows.get_mut("planner-led").unwrap();
        workflow.nodes = vec![WorkflowNodeSpec {
            id: "review".to_owned(),
            agent: "executor".to_owned(),
            harness: "review-only".to_owned(),
        }];
        workflow.edges.clear();
    }
}
