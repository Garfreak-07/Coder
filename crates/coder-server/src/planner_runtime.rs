use coder_config::{
    validate_project_config, HarnessSpec as ConfigHarnessSpec, MemoryScope as ConfigMemoryScope,
    PermissionDecision as ConfigPermissionDecision, ProjectConfig, ValidationLevel,
    ValidationReport,
};

use crate::{ApiError, PlannerRuntimeContext};

pub(crate) fn resolve_planner_runtime(
    config: &ProjectConfig,
    workflow_id: &str,
    planner_agent_id: Option<&str>,
) -> Result<PlannerRuntimeContext, ApiError> {
    let validation = validate_project_config(config);
    if validation
        .issues
        .iter()
        .any(|issue| issue.level == ValidationLevel::Error)
    {
        return Err(ApiError::bad_request(format!(
            "Planner workflow config is invalid: {}",
            validation_issue_summary(&validation)
        )));
    }
    let workflow = config
        .workflows
        .get(workflow_id)
        .ok_or_else(|| ApiError::bad_request(format!("workflow '{workflow_id}' was not found")))?;
    let binding = resolve_planner_binding(config, workflow_id, planner_agent_id)?;
    let agent = config.agents.get(&binding.agent_id).ok_or_else(|| {
        ApiError::bad_request(format!(
            "planner binding '{}' references missing agent '{}'",
            binding.node_id, binding.agent_id
        ))
    })?;
    if agent.role != "planner" {
        return Err(ApiError::bad_request(format!(
            "planner binding '{}' must reference an agent with role 'planner'",
            binding.node_id
        )));
    }
    if agent.output_contract != "planner_conversation" {
        return Err(ApiError::bad_request(format!(
            "Planner Chat requires planner agent '{}' to use output_contract 'planner_conversation'",
            binding.agent_id
        )));
    }
    let harness = config.harnesses.get(&binding.harness_id).ok_or_else(|| {
        ApiError::bad_request(format!(
            "planner binding '{}' references missing harness '{}'",
            binding.node_id, binding.harness_id
        ))
    })?;
    ensure_planner_conversation_harness(&binding.harness_id, harness)?;
    let model = config.models.get(&agent.model).ok_or_else(|| {
        ApiError::bad_request(format!(
            "planner agent '{}' references missing model '{}'",
            binding.agent_id, agent.model
        ))
    })?;
    Ok(PlannerRuntimeContext {
        workflow_id: workflow_id.to_owned(),
        workflow_name: workflow.name.clone(),
        node_id: binding.node_id,
        agent_id: binding.agent_id,
        harness_id: binding.harness_id,
        agent: agent.clone(),
        harness: harness.clone(),
        model: model.clone(),
    })
}

struct PlannerRuntimeBinding {
    node_id: String,
    agent_id: String,
    harness_id: String,
}

fn resolve_planner_binding(
    config: &ProjectConfig,
    workflow_id: &str,
    planner_agent_id: Option<&str>,
) -> Result<PlannerRuntimeBinding, ApiError> {
    let binding = config
        .surface_bindings
        .planner_chat
        .as_ref()
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "workflow '{workflow_id}' requires an explicit surface_bindings.planner_chat agent and harness"
            ))
        })?;
    if let Some(requested_agent_id) = planner_agent_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|value| *value != binding.agent)
    {
        return Err(ApiError::bad_request(format!(
            "Planner Chat requested agent '{requested_agent_id}', but surface_bindings.planner_chat selects '{}'",
            binding.agent
        )));
    }
    Ok(PlannerRuntimeBinding {
        node_id: binding.agent.clone(),
        agent_id: binding.agent.clone(),
        harness_id: binding.harness.clone(),
    })
}

fn ensure_planner_conversation_harness(
    harness_id: &str,
    harness: &ConfigHarnessSpec,
) -> Result<(), ApiError> {
    if harness.backend != "planner-model" {
        return Err(ApiError::bad_request(format!(
            "Planner Chat requires planner harness '{harness_id}' to use backend 'planner-model'"
        )));
    }
    ensure_permission(
        harness_id,
        "read_files",
        harness.permissions.read_files,
        ConfigPermissionDecision::Allow,
    )?;
    for (permission, decision) in [
        ("write_files", harness.permissions.write_files),
        ("run_commands", harness.permissions.run_commands),
        (
            "child_harness_permissions",
            harness.permissions.child_harness_permissions,
        ),
        ("network", harness.permissions.network),
        ("secrets", harness.permissions.secrets),
        ("publish_external", harness.permissions.publish_external),
        ("git_commit", harness.permissions.git_commit),
        ("git_push", harness.permissions.git_push),
        ("deploy", harness.permissions.deploy),
    ] {
        ensure_permission(
            harness_id,
            permission,
            decision,
            ConfigPermissionDecision::Deny,
        )?;
    }
    if harness
        .memory
        .write
        .iter()
        .any(|scope| *scope != ConfigMemoryScope::Run)
    {
        return Err(ApiError::bad_request(format!(
            "Planner Conversation Harness '{harness_id}' may only write run memory"
        )));
    }
    Ok(())
}

fn ensure_permission(
    harness_id: &str,
    permission: &str,
    actual: ConfigPermissionDecision,
    expected: ConfigPermissionDecision,
) -> Result<(), ApiError> {
    if actual == expected {
        return Ok(());
    }
    Err(ApiError::bad_request(format!(
        "Planner Conversation Harness '{harness_id}' must set {permission} to {:?}",
        expected
    )))
}

pub(crate) fn validation_issue_summary(report: &ValidationReport) -> String {
    report
        .issues
        .iter()
        .filter(|issue| issue.level == ValidationLevel::Error)
        .take(3)
        .map(|issue| format!("{} at {}", issue.code, issue.target))
        .collect::<Vec<_>>()
        .join("; ")
}
