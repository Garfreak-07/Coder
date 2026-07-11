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
    let binding = resolve_planner_binding(config, workflow, workflow_id, planner_agent_id)?;
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
    workflow: &coder_config::WorkflowSpec,
    workflow_id: &str,
    planner_agent_id: Option<&str>,
) -> Result<PlannerRuntimeBinding, ApiError> {
    if let Some(planner_agent_id) = planner_agent_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if config.agents.contains_key(planner_agent_id) {
            return Ok(PlannerRuntimeBinding {
                node_id: planner_agent_id.to_owned(),
                agent_id: planner_agent_id.to_owned(),
                harness_id: resolve_planner_harness_id(config)?,
            });
        }
        if let Some(node) = workflow
            .nodes
            .iter()
            .find(|node| node.agent == planner_agent_id || node.id == planner_agent_id)
        {
            return Ok(PlannerRuntimeBinding {
                node_id: node.id.clone(),
                agent_id: node.agent.clone(),
                harness_id: node.harness.clone(),
            });
        }
        return Err(ApiError::bad_request(format!(
            "workflow '{workflow_id}' has no planner node or planner agent for '{planner_agent_id}'"
        )));
    }

    if config
        .agents
        .get("planner")
        .filter(|agent| agent.role == "planner" && agent.output_contract == "planner_conversation")
        .is_some()
    {
        return Ok(PlannerRuntimeBinding {
            node_id: "planner".to_owned(),
            agent_id: "planner".to_owned(),
            harness_id: resolve_planner_harness_id(config)?,
        });
    }

    if let Some(node) = workflow.nodes.iter().find(|node| {
        config
            .agents
            .get(&node.agent)
            .map(|agent| agent.role == "planner" && agent.output_contract == "planner_conversation")
            .unwrap_or(false)
    }) {
        return Ok(PlannerRuntimeBinding {
            node_id: node.id.clone(),
            agent_id: node.agent.clone(),
            harness_id: node.harness.clone(),
        });
    }

    let agent_id = config
        .agents
        .get("planner")
        .filter(|agent| agent.role == "planner" && agent.output_contract == "planner_conversation")
        .map(|_| "planner".to_owned())
        .or_else(|| {
            config
                .agents
                .iter()
                .find(|(_, agent)| {
                    agent.role == "planner" && agent.output_contract == "planner_conversation"
                })
                .map(|(agent_id, _)| agent_id.clone())
        })
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "workflow '{workflow_id}' has no planner node and config has no planner agent"
            ))
        })?;

    Ok(PlannerRuntimeBinding {
        node_id: agent_id.clone(),
        agent_id,
        harness_id: resolve_planner_harness_id(config)?,
    })
}

fn resolve_planner_harness_id(config: &ProjectConfig) -> Result<String, ApiError> {
    if config.harnesses.contains_key("planner-conversation") {
        return Ok("planner-conversation".to_owned());
    }
    if let Some((harness_id, _)) = config
        .harnesses
        .iter()
        .find(|(_, harness)| harness.backend == "planner-model")
    {
        return Ok(harness_id.clone());
    }
    Err(ApiError::bad_request(
        "Planner Chat requires a planner-model harness such as 'planner-conversation'",
    ))
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
