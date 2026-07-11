use coder_config::{AgentSpec, HarnessSpec, ModelSpec, WorkflowNodeSpec};
use coder_core::{FinalReport, RunId};
use coder_harness::{HarnessError, HarnessRunEvent, HarnessRunResult};
use coder_store::CompactionCircuitState;
use serde_json::{json, Value};

use crate::{
    workflow_events::{BackendBlockedEvent, BackendEventContext, BackendSelectionEvent},
    workflow_harness_request::{build_harness_run_request, HarnessRunRequestInput},
    workflow_verification::{enforce_harness_verification, VerificationEventContext},
    WorkflowError, WorkflowRunner,
};

pub(super) struct WorkflowBackendRunInput<'a> {
    pub(super) run_id: &'a RunId,
    pub(super) sequence: &'a mut u64,
    pub(super) round: u32,
    pub(super) repo_root: &'a str,
    pub(super) current_task: &'a str,
    pub(super) workflow_id: &'a str,
    pub(super) node: &'a WorkflowNodeSpec,
    pub(super) agent: &'a AgentSpec,
    pub(super) harness: &'a HarnessSpec,
    pub(super) model: &'a ModelSpec,
    pub(super) plan_context: Option<&'a Value>,
    pub(super) loop_feedback: Option<&'a Value>,
    pub(super) max_rounds: u32,
    pub(super) token_budget: Option<u64>,
    pub(super) executor_evidence_this_round: bool,
    pub(super) executor_evidence_summary: &'a str,
    pub(super) previous_planner_improvements: &'a [Vec<String>],
    pub(super) compaction_circuit_state: Option<&'a CompactionCircuitState>,
}

pub(super) struct WorkflowBackendRunOutput {
    pub(super) effective_backend: String,
    pub(super) result: HarnessRunResult,
}

impl WorkflowRunner {
    pub(super) async fn run_node_backend(
        &self,
        input: WorkflowBackendRunInput<'_>,
    ) -> Result<WorkflowBackendRunOutput, WorkflowError> {
        let requested_backend = input.harness.backend.clone();
        let effective_backend = requested_backend.clone();
        let backend = match self.backends.backend_for(&requested_backend) {
            Some(backend) => {
                self.emit_backend_selected(
                    BackendEventContext {
                        run_id: input.run_id,
                        sequence: input.sequence,
                        round: input.round,
                        node: input.node,
                    },
                    BackendSelectionEvent {
                        backend: &requested_backend,
                        requested_backend: &requested_backend,
                        fallback_for: None,
                        fallback_allowed: false,
                    },
                )?;
                backend
            }
            None => return Err(WorkflowError::BackendNotFound(requested_backend)),
        };

        let mut result = match backend
            .run(build_harness_run_request(HarnessRunRequestInput {
                run_id: input.run_id,
                repo_root: input.repo_root,
                task: input.current_task,
                workflow_id: input.workflow_id,
                node: input.node,
                agent: input.agent,
                harness: input.harness,
                model: input.model,
                plan_context: input.plan_context,
                loop_feedback: input.loop_feedback,
                round: input.round,
                max_rounds: input.max_rounds,
                token_budget: input.token_budget,
                executor_evidence_this_round: input.executor_evidence_this_round,
                executor_evidence_summary: input.executor_evidence_summary,
                previous_planner_improvements: input.previous_planner_improvements,
                compaction_circuit_state: input.compaction_circuit_state,
            }))
            .await
        {
            Ok(result) => result,
            Err(HarnessError::Unavailable(message)) => {
                self.emit_backend_blocked(
                    BackendEventContext {
                        run_id: input.run_id,
                        sequence: input.sequence,
                        round: input.round,
                        node: input.node,
                    },
                    BackendBlockedEvent {
                        backend: &requested_backend,
                        reason: &message,
                        fallback_allowed: false,
                    },
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

        enforce_harness_verification(
            &mut result,
            VerificationEventContext {
                run_id: input.run_id,
                workflow_id: input.workflow_id,
                round: input.round,
                node: input.node,
                backend: &effective_backend,
                harness: input.harness,
            },
        );

        Ok(WorkflowBackendRunOutput {
            effective_backend,
            result,
        })
    }
}
