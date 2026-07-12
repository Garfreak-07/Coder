use coder_config::{
    resolve_agent_runtime_policy, resolve_agent_tools, AgentSpec, HarnessSpec, ModelSpec,
    WorkflowNodeSpec,
};
use coder_core::RunId;
use coder_harness::HarnessRunRequest;
use coder_store::CompactionCircuitState;
use serde_json::{json, Value};

use crate::{
    context_compaction::compact_plan_context_with_circuit,
    workflow_context_projection::{model_reference, permission_summary},
};

pub(crate) struct HarnessRunRequestInput<'a> {
    pub(crate) run_id: &'a RunId,
    pub(crate) repo_root: &'a str,
    pub(crate) task: &'a str,
    pub(crate) workflow_id: &'a str,
    pub(crate) node: &'a WorkflowNodeSpec,
    pub(crate) agent: &'a AgentSpec,
    pub(crate) harness: &'a HarnessSpec,
    pub(crate) model: &'a ModelSpec,
    pub(crate) plan_context: Option<&'a Value>,
    pub(crate) loop_feedback: Option<&'a Value>,
    pub(crate) round: u32,
    pub(crate) max_rounds: u32,
    pub(crate) token_budget: Option<u64>,
    pub(crate) executor_evidence_this_round: bool,
    pub(crate) executor_evidence_summary: &'a str,
    pub(crate) previous_planner_improvements: &'a [Vec<String>],
    pub(crate) compaction_circuit_state: Option<&'a CompactionCircuitState>,
}

pub(crate) fn build_harness_run_request(input: HarnessRunRequestInput<'_>) -> HarnessRunRequest {
    let dynamic_plan_context = dynamic_plan_context(input.plan_context, input.loop_feedback);
    let plan_context = dynamic_plan_context.as_ref().or(input.plan_context);
    HarnessRunRequest {
        run_id: input.run_id.clone(),
        workflow_id: input.workflow_id.to_owned(),
        node_id: input.node.id.clone(),
        agent_id: input.node.agent.clone(),
        harness_id: input.node.harness.clone(),
        repo_root: input.repo_root.to_owned(),
        task: input.task.to_owned(),
        backend_context: harness_backend_context(&input, plan_context),
    }
}

fn harness_backend_context(
    input: &HarnessRunRequestInput<'_>,
    plan_context: Option<&Value>,
) -> Value {
    let agent_tools = resolve_agent_tools(input.agent, input.harness);
    let selected_tools = &agent_tools.selected_tools;
    let resolved_runtime = resolve_agent_runtime_policy(input.model, &input.agent.runtime);
    let compacted_plan_context = compact_plan_context_with_circuit(
        plan_context,
        &resolved_runtime,
        input.compaction_circuit_state,
    );
    let plan_context = compacted_plan_context
        .plan_context
        .clone()
        .unwrap_or(Value::Null);
    let coder = json!({
        "workflow_id": input.workflow_id,
        "node_id": input.node.id,
        "agent_id": input.node.agent,
        "harness_id": input.node.harness,
        "agent": {
            "role": &input.agent.role,
            "system": &input.agent.system,
            "output_contract": &input.agent.output_contract,
            "runtime": serde_json::to_value(&input.agent.runtime).unwrap_or(Value::Null)
        },
        "harness": {
            "selected_tools": selected_tools,
            "permissions": serde_json::to_value(&input.harness.permissions).unwrap_or(Value::Null),
            "memory": serde_json::to_value(&input.harness.memory).unwrap_or(Value::Null),
            "verification": serde_json::to_value(&input.harness.verification).unwrap_or(Value::Null)
        },
        "model": model_reference(input.model),
        "permissions": permission_summary(input.harness),
        "context_compaction": compacted_plan_context.report,
        "plan_context": plan_context,
        "workflow_loop": {
            "round": input.round,
            "max_rounds": input.max_rounds,
            "token_budget": input.token_budget,
            "remaining_rounds": input.max_rounds.saturating_sub(input.round),
            "final_round": input.round >= input.max_rounds,
            "executor_evidence_this_round": input.executor_evidence_this_round,
            "executor_evidence_summary": input.executor_evidence_summary,
            "previous_improvements": input.previous_planner_improvements
        }
    });
    json!({ "coder": coder })
}

fn dynamic_plan_context(base: Option<&Value>, loop_feedback: Option<&Value>) -> Option<Value> {
    let feedback = loop_feedback?;
    let mut context = base.cloned().unwrap_or_else(|| json!({}));
    context["workflow_feedback"] = feedback.clone();
    Some(context)
}
