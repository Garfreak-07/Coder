use std::{
    collections::{BTreeMap, BTreeSet},
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
    OpenHandsAuthHeaderMode, OpenHandsClient, OpenHandsRunStartStrategy, OpenHandsServerConfig,
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
                "max_rounds": workflow.max_rounds
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
            let backend = self
                .backends
                .backend_for(&harness.backend)
                .ok_or_else(|| WorkflowError::BackendNotFound(harness.backend.clone()))?;

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

            let backend_result = match backend
                .run(HarnessRunRequest {
                    run_id: run_id.clone(),
                    workflow_id: options.workflow_id.clone(),
                    node_id: node.id.clone(),
                    agent_id: node.agent.clone(),
                    harness_id: node.harness.clone(),
                    repo_root: options.repo_root.display().to_string(),
                    task: options.task.clone(),
                    backend_context: harness_backend_context(OpenHandsConversationPayloadInput {
                        run_id: &run_id,
                        workflow_id: &options.workflow_id,
                        workflow,
                        node,
                        agent_id: &node.agent,
                        agent,
                        harness_id: &node.harness,
                        harness,
                        model,
                    }),
                })
                .await
            {
                Ok(result) => result,
                Err(HarnessError::Unavailable(message)) => HarnessRunResult::blocked(format!(
                    "backend '{}' unavailable: {message}",
                    harness.backend
                )),
                Err(error) => HarnessRunResult {
                    status: "failed".to_owned(),
                    report: Some(FinalReport::failed(
                        "Harness backend failed.",
                        error.to_string(),
                    )),
                    events: vec![HarnessRunEvent::new(
                        "backend.failed",
                        json!({
                            "backend": harness.backend,
                            "error": error.to_string()
                        }),
                    )],
                },
            };

            for backend_event in backend_result.events {
                self.emit_harness_event(&run_id, &mut sequence, backend_event)?;
            }

            let raw_status = backend_result.status;
            let signal = WorkflowSignal::from_status(&raw_status);
            checks.push(format!(
                "node {} via {}: {}",
                node.id, harness.backend, raw_status
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
    pub workflow_id: &'a str,
    pub workflow: &'a WorkflowSpec,
    pub node: &'a WorkflowNodeSpec,
    pub agent_id: &'a str,
    pub agent: &'a AgentSpec,
    pub harness_id: &'a str,
    pub harness: &'a HarnessSpec,
    pub model: &'a ModelSpec,
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
        "output_contract": &input.agent.output_contract
    });

    json!({
        "agent": {
            "kind": "CodeActAgent"
        },
        "metadata": {
            "source": "coder-workflow",
            "coder": coder_metadata
        },
        "coder_context": {
            "contract": "coder.openhands.context.v1",
            "run": {
                "run_id": input.run_id.as_str()
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
            }
        }
    })
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
        "permissions": permission_summary(input.harness)
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
}

impl WorkflowRunOptions {
    pub fn new(workflow_id: impl Into<String>, task: impl Into<String>) -> Self {
        Self {
            workflow_id: workflow_id.into(),
            task: task.into(),
            repo_root: PathBuf::from("."),
            dry_run: false,
            max_rounds_override: None,
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
    native_rust: Arc<dyn HarnessBackend>,
    native_mock: Arc<dyn HarnessBackend>,
    openhands: Option<Arc<dyn HarnessBackend>>,
}

impl BackendRegistry {
    pub fn native_only() -> Self {
        Self {
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
            "native-rust" => Some(Arc::clone(&self.native_rust)),
            "native_mock" | "mock" => Some(Arc::clone(&self.native_mock)),
            "openhands" => self.openhands.as_ref().map(Arc::clone),
            _ => None,
        }
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
            &request.run_id,
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
    run_id: &RunId,
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
        let raw_events = client
            .fetch_events(conversation_id, fetch_limit)
            .await
            .map_err(|error| HarnessError::Failed(error.to_string()))?;
        for raw in raw_events {
            let key = openhands_event_key(&raw);
            if !seen.insert(key) {
                continue;
            }
            captured_events += 1;
            let terminal = openhands_terminal_status(&raw, &config.terminal_event_kinds);
            events.push(openhands_raw_harness_event(run_id, store, raw)?);
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

fn openhands_raw_harness_event(
    run_id: &RunId,
    store: &RunStore,
    raw: Value,
) -> Result<HarnessRunEvent, HarnessError> {
    let raw_text =
        serde_json::to_string(&raw).map_err(|error| HarnessError::Failed(error.to_string()))?;
    let raw_ref = store
        .write_large_text_ref(&raw_text)
        .map_err(|error| HarnessError::Failed(error.to_string()))?
        .blob_ref;
    let normalized = normalize_openhands_event(run_id.clone(), 0, raw, Some(raw_ref));
    let mut event = HarnessRunEvent::new(normalized.kind, normalized.payload);
    for reference in normalized.refs {
        event = event.with_ref(reference.label, reference.uri);
    }
    Ok(event)
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
    let configured_terminal = terminal_event_kinds
        .iter()
        .any(|kind| kind.eq_ignore_ascii_case(&raw_kind));
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
    request
        .backend_context
        .pointer("/openhands/create_conversation_payload")
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "agent": {"kind": "CodeActAgent"},
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
        })
}

struct WorkflowReportInput<'a> {
    run_id: &'a RunId,
    workflow_id: &'a str,
    status: RunStatus,
    reason: Option<&'a str>,
    dispatched_nodes: usize,
    checks: Vec<String>,
    evidence_refs: Vec<coder_core::EvidenceRef>,
    blockers: Vec<String>,
    changed_files: Vec<String>,
    patch_refs: Vec<String>,
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
    report
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

    use coder_config::{ProjectConfig, WorkflowNodeSpec};
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

        let output = runner
            .run(WorkflowRunOptions::new("planner-led", "needs openhands"))
            .await
            .unwrap();

        assert_eq!(output.report.status, ReportStatus::Blocked);
        assert!(output
            .report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("backend 'openhands' unavailable")));
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

        let error = runner
            .run(WorkflowRunOptions::new("planner-led", "unknown backend"))
            .await
            .unwrap_err();

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

        let payload = build_openhands_conversation_payload(OpenHandsConversationPayloadInput {
            run_id: &run_id,
            workflow_id: "planner-led",
            workflow,
            node,
            agent_id: &node.agent,
            agent,
            harness_id: &node.harness,
            harness,
            model,
        });
        let payload_text = serde_json::to_string(&payload).unwrap();

        assert_eq!(payload["agent"]["kind"], "CodeActAgent");
        assert_eq!(
            payload["metadata"]["coder"]["agent_kind_source"],
            "default_fallback"
        );
        assert_eq!(payload["metadata"]["coder"]["run_id"], "run-phase2");
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
        assert!(payload["coder_context"]["agent"]["system_instructions"]
            .as_str()
            .unwrap()
            .contains("coding executor"));
        assert_eq!(
            payload["coder_context"]["memory"]["note"],
            "scope names only; memory contents are not embedded"
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
        assert_eq!(fallback["agent"]["kind"], "CodeActAgent");
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
            .collect::<Vec<_>>();
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
