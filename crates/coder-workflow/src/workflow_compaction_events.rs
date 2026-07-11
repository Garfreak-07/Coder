use coder_config::{AgentSpec, WorkflowNodeSpec};
use coder_core::RunId;
use coder_store::CompactionCircuitState;
use serde_json::{json, Value};

use crate::{
    context_compaction::{compact_plan_context_with_circuit, ContextCompactionCircuitOutcome},
    WorkflowError, WorkflowRunner,
};

pub(super) struct ContextCompactionEventInput<'a> {
    pub(super) round: u32,
    pub(super) node: &'a WorkflowNodeSpec,
    pub(super) agent: &'a AgentSpec,
    pub(super) plan_context: Option<&'a Value>,
    pub(super) current_state: Option<&'a CompactionCircuitState>,
}

impl WorkflowRunner {
    pub(super) fn record_context_compaction_circuit_outcome(
        &self,
        run_id: &RunId,
        sequence: &mut u64,
        input: ContextCompactionEventInput<'_>,
    ) -> Result<Option<CompactionCircuitState>, WorkflowError> {
        let output = compact_plan_context_with_circuit(
            input.plan_context,
            &input.agent.runtime,
            input.current_state,
        );
        let Some(outcome) = output.circuit_outcome else {
            return Ok(input.current_state.cloned());
        };
        let updated = self.store.record_compaction_circuit_outcome(
            run_id.as_str(),
            input.agent.runtime.max_consecutive_compaction_failures,
            outcome.success(),
        )?;
        self.emit(
            run_id,
            sequence,
            "context.compaction.outcome",
            json!({
                "round": input.round,
                "node_id": input.node.id,
                "agent": input.node.agent,
                "status": output.report["status"].clone(),
                "applied": output.report["applied"].clone(),
                "success": outcome.success(),
                "outcome": context_compaction_outcome_name(outcome),
                "original_estimated_tokens": output.report["original_estimated_tokens"].clone(),
                "compacted_estimated_tokens": output.report["compacted_estimated_tokens"].clone(),
                "consecutive_failures": updated.consecutive_failures,
                "max_consecutive_failures": updated.max_consecutive_failures,
                "circuit_breaker_open": updated.circuit_breaker_open,
                "circuit_scope_id": updated.scope_id
            }),
        )?;
        Ok(Some(updated))
    }
}

fn context_compaction_outcome_name(outcome: ContextCompactionCircuitOutcome) -> &'static str {
    match outcome {
        ContextCompactionCircuitOutcome::Success => "success",
        ContextCompactionCircuitOutcome::Failure => "failure",
    }
}
