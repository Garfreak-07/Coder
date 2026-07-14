use coder_config::{
    resolve_agent_runtime_policy, resolve_task_tools, HarnessSpec, ModelSpec, TaskProfile,
};
use coder_core::RunId;
use coder_harness::HarnessRunRequest;
use coder_store::CompactionCircuitState;
use serde_json::{json, Value};

use crate::{
    context_compaction::compact_task_context_with_circuit,
    workflow_context_projection::{model_reference, permission_summary},
};

pub(crate) struct HarnessRunRequestInput<'a> {
    pub(crate) run_id: &'a RunId,
    pub(crate) repo_root: &'a str,
    pub(crate) task: &'a str,
    pub(crate) workflow_id: &'a str,
    pub(crate) profile: &'a TaskProfile,
    pub(crate) harness: &'a HarnessSpec,
    pub(crate) model: &'a ModelSpec,
    pub(crate) task_context: Option<&'a Value>,
    pub(crate) token_budget: Option<u64>,
    pub(crate) compaction_circuit_state: Option<&'a CompactionCircuitState>,
}

pub(crate) fn build_harness_run_request(input: HarnessRunRequestInput<'_>) -> HarnessRunRequest {
    HarnessRunRequest {
        run_id: input.run_id.clone(),
        workflow_id: input.workflow_id.to_owned(),
        node_id: input.workflow_id.to_owned(),
        agent_id: input.workflow_id.to_owned(),
        harness_id: input.profile.harness.clone(),
        repo_root: input.repo_root.to_owned(),
        task: input.task.to_owned(),
        backend_context: harness_backend_context(&input, input.task_context),
    }
}

fn harness_backend_context(
    input: &HarnessRunRequestInput<'_>,
    task_context: Option<&Value>,
) -> Value {
    let task_tools = resolve_task_tools(input.profile, input.harness);
    let selected_tools = &task_tools.selected_tools;
    let resolved_runtime = resolve_agent_runtime_policy(input.model, &input.profile.runtime);
    let compacted_task_context = compact_task_context_with_circuit(
        task_context,
        &resolved_runtime,
        input.compaction_circuit_state,
    );
    let task_context = compacted_task_context
        .task_context
        .clone()
        .unwrap_or(Value::Null);
    let coder = json!({
        "workflow_id": input.workflow_id,
        "node_id": input.workflow_id,
        "agent_id": input.workflow_id,
        "harness_id": input.profile.harness,
        "agent": {
            "system": &input.profile.instructions,
            "runtime": serde_json::to_value(&input.profile.runtime).unwrap_or(Value::Null)
        },
        "harness": {
            "selected_tools": selected_tools,
            "permissions": serde_json::to_value(&input.harness.permissions).unwrap_or(Value::Null),
            "verification": serde_json::to_value(&input.harness.verification).unwrap_or(Value::Null)
        },
        "model": model_reference(input.model),
        "permissions": permission_summary(input.harness),
        "context_compaction": compacted_task_context.report,
        "task_context": task_context,
        "task": {
            "token_budget": input.token_budget,
        }
    });
    json!({ "coder": coder })
}
