use axum::{extract::State, Json};
use coder_config::ProjectConfig;
use coder_core::RunId;
use coder_workflow::TurnContext;
use serde_json::{json, Value};

use crate::model_tool_hook_runtime::{append_model_tool_event_checked, ModelToolEventWriteError};
use crate::model_tool_input::{
    model_tool_bool, model_tool_object, model_tool_string, model_tool_u32, to_value,
};
use crate::model_tool_permissions::{
    model_tool_context_run_id, read_run_project_config_snapshot,
    DEFAULT_MODEL_TOOL_PERMISSION_HARNESS_ID,
};
use crate::model_tool_run_context::latest_run_context;
use crate::skill_model_tool::{
    load_model_skill, model_tool_skill_name, LoadedModelSkill, SkillExecutionPolicy,
};
use crate::{
    apply_provider_settings_to_project_config, ApiError, ApiState, ModelToolExecuteRequest,
    SubagentRunToolRequest, INVOKED_SKILL_CONTRACT, INVOKED_SKILL_EVENT_KIND,
    POST_COMPACT_MAX_CHARS_PER_SKILL,
};

pub(crate) async fn execute_skill_model_tool(
    state: &ApiState,
    request: &ModelToolExecuteRequest,
    host_context: &TurnContext,
) -> Result<Value, ApiError> {
    let skill_name = model_tool_skill_name(&request.input)
        .ok_or_else(|| ApiError::bad_request("Skill input requires skill or command name"))?;
    let run_id = model_tool_context_run_id(&request.input, host_context).ok_or_else(|| {
        ApiError::bad_request("Skill invocation requires run_id so it can survive compaction")
    })?;
    let run_id = RunId::from_string(run_id);
    if !crate::stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }

    let loaded_skill = load_model_skill(state, &skill_name, run_id.as_str(), &request.tool_use_id)?
        .ok_or_else(|| ApiError::bad_request(format!("Unknown skill: {skill_name}")))?;
    let execution_policy = loaded_skill.execution_policy.clone();
    if execution_policy.disable_model_invocation {
        return Err(ApiError::bad_request(format!(
            "Skill {} cannot be used with SkillTool because disable-model-invocation is true",
            loaded_skill.id
        )));
    }
    let (recorded_content, content_truncated) =
        crate::truncate_text_to_chars(&loaded_skill.content, POST_COMPACT_MAX_CHARS_PER_SKILL);
    let content_estimated_tokens = crate::estimate_text_tokens(&recorded_content);
    let parent_agent_id = model_tool_skill_parent_agent_id(request, host_context);
    let execution_context = SkillModelToolExecutionContext {
        state,
        request,
        host_context,
        run_id: &run_id,
        loaded_skill: &loaded_skill,
        execution_policy: &execution_policy,
        recorded_content: &recorded_content,
        content_truncated,
        content_estimated_tokens,
        parent_agent_id: parent_agent_id.as_deref(),
        requested_skill: &skill_name,
    };
    let sequence = record_invoked_model_skill(&execution_context)?;

    if execution_policy.context == "fork" {
        return execute_forked_skill_model_tool(&execution_context, sequence).await;
    }

    Ok(json!({
        "contract": "coder.skill_tool_result.v1",
        "source": "coder-server",
        "status": "completed",
        "execution_context": "inline",
        "skill_name": loaded_skill.id.clone(),
        "display_name": loaded_skill.display_name.clone(),
        "skill_path": loaded_skill.skill_path,
        "skill_origin": loaded_skill.origin,
        "base_dir": loaded_skill.base_dir,
        "frontmatter": loaded_skill.frontmatter,
        "execution_policy": execution_policy,
        "content": recorded_content,
        "content_truncated": content_truncated,
        "content_estimated_tokens": content_estimated_tokens,
        "agent_id": parent_agent_id,
        "event_kind": INVOKED_SKILL_EVENT_KIND,
        "event_sequence": sequence
    }))
}

fn model_tool_skill_parent_agent_id(
    request: &ModelToolExecuteRequest,
    host_context: &TurnContext,
) -> Option<String> {
    host_context
        .agent_id
        .as_deref()
        .or_else(|| {
            request
                .input
                .get("agent_id")
                .or_else(|| request.input.get("agentId"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

struct SkillModelToolExecutionContext<'a> {
    state: &'a ApiState,
    request: &'a ModelToolExecuteRequest,
    host_context: &'a TurnContext,
    run_id: &'a RunId,
    loaded_skill: &'a LoadedModelSkill,
    execution_policy: &'a SkillExecutionPolicy,
    recorded_content: &'a str,
    content_truncated: bool,
    content_estimated_tokens: u32,
    parent_agent_id: Option<&'a str>,
    requested_skill: &'a str,
}

fn record_invoked_model_skill(
    context: &SkillModelToolExecutionContext<'_>,
) -> Result<u64, ApiError> {
    let payload = json!({
        "contract": INVOKED_SKILL_CONTRACT,
        "source": "coder-server",
        "skill_name": context.loaded_skill.id.clone(),
        "display_name": context.loaded_skill.display_name.clone(),
        "skill_path": context.loaded_skill.skill_path.clone(),
        "skill_origin": context.loaded_skill.origin,
        "base_dir": context.loaded_skill.base_dir.clone(),
        "frontmatter": context.loaded_skill.frontmatter.clone(),
        "execution_policy": context.execution_policy.clone(),
        "execution_context": if context.execution_policy.context == "fork" { "fork" } else { "inline" },
        "content": context.recorded_content,
        "content_truncated": context.content_truncated,
        "content_estimated_tokens": context.content_estimated_tokens,
        "agent_id": context.parent_agent_id,
        "model_tool": {
            "tool_use_id": context.request.tool_use_id,
            "tool_name": context.request.tool_name,
            "requested_skill": context.requested_skill
        }
    });
    append_model_tool_event_checked(
        &context.state.store,
        context.run_id,
        INVOKED_SKILL_EVENT_KIND,
        payload,
    )
    .map_err(|error| match error {
        ModelToolEventWriteError::LockPoisoned => {
            ApiError::internal("model tool event lock poisoned")
        }
        ModelToolEventWriteError::Store(error) => ApiError::from(error),
    })
}

async fn execute_forked_skill_model_tool(
    context: &SkillModelToolExecutionContext<'_>,
    event_sequence: u64,
) -> Result<Value, ApiError> {
    let subagent_request = skill_fork_subagent_run_request(
        context.state,
        context.request,
        context.host_context,
        context.run_id,
        context.loaded_skill,
        context.execution_policy,
        event_sequence,
    )?;
    let response = crate::subagent_tools::run_subagent_endpoint(
        State(context.state.clone()),
        Json(subagent_request),
    )
    .await?;
    let subagent = to_value(response.0)?;
    let status = subagent
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
        .to_owned();
    Ok(json!({
        "contract": "coder.skill_tool_result.v1",
        "source": "coder-server",
        "status": status,
        "execution_context": "fork",
        "skill_name": context.loaded_skill.id.clone(),
        "display_name": context.loaded_skill.display_name.clone(),
        "skill_path": context.loaded_skill.skill_path.clone(),
        "skill_origin": context.loaded_skill.origin,
        "base_dir": context.loaded_skill.base_dir.clone(),
        "frontmatter": context.loaded_skill.frontmatter.clone(),
        "execution_policy": context.execution_policy.clone(),
        "content_recorded_in_event": true,
        "content_truncated": context.content_truncated,
        "content_estimated_tokens": context.content_estimated_tokens,
        "agent_id": context.parent_agent_id,
        "event_kind": INVOKED_SKILL_EVENT_KIND,
        "event_sequence": event_sequence,
        "subagent": subagent,
        "metadata_ref": subagent.get("metadata_ref").cloned().unwrap_or(Value::Null),
        "transcript_ref": subagent.get("transcript_ref").cloned().unwrap_or(Value::Null)
    }))
}

fn skill_fork_subagent_run_request(
    state: &ApiState,
    request: &ModelToolExecuteRequest,
    host_context: &TurnContext,
    run_id: &RunId,
    loaded_skill: &LoadedModelSkill,
    execution_policy: &SkillExecutionPolicy,
    event_sequence: u64,
) -> Result<SubagentRunToolRequest, ApiError> {
    let input = &request.input;
    let run_context = latest_run_context(&state.store, run_id.as_str()).unwrap_or_default();
    let mut config = read_run_project_config_snapshot(&state.store, run_id.as_str())
        .unwrap_or_else(crate::default_project_config);
    let provider_settings = state.provider_settings.lock().unwrap().clone();
    apply_provider_settings_to_project_config(&mut config, &provider_settings);
    let workflow_id = model_tool_string(input, &["workflow_id"])
        .or(run_context.task_profile_id)
        .unwrap_or_else(|| "model-tool".to_owned());
    let node_id = model_tool_string(input, &["node_id"])
        .or(run_context.node_id)
        .unwrap_or_else(|| "skill-tool".to_owned());
    let parent_harness_id = host_context
        .harness_id
        .as_ref()
        .cloned()
        .or(run_context.harness_id)
        .or_else(|| model_tool_string(input, &["parent_harness_id", "harness_id"]))
        .unwrap_or_else(|| DEFAULT_MODEL_TOOL_PERMISSION_HARNESS_ID.to_owned());
    let parent_agent_id = host_context
        .agent_id
        .as_ref()
        .cloned()
        .or(run_context.agent_id)
        .or_else(|| model_tool_string(input, &["parent_agent_id"]))
        .unwrap_or_else(|| "model-tool".to_owned());
    let repo_root = model_tool_string(input, &["repo_root", "cwd"]).or(run_context.repo_root);
    let backend_context = model_tool_object(input, "backend_context").unwrap_or_else(|| json!({}));
    let subagent_name = resolve_skill_fork_profile(execution_policy, &config, &parent_agent_id)?;
    let run_in_background = model_tool_bool(input, &["run_in_background", "runInBackground"]);
    Ok(SubagentRunToolRequest {
        config,
        workflow_id,
        node_id,
        parent_agent_id,
        parent_harness_id,
        repo_root,
        task: skill_fork_task(input, loaded_skill),
        run_id: Some(run_id.as_str().to_owned()),
        agent_id: model_tool_string(input, &["child_agent_id", "childAgentId"]),
        subagent_name: Some(subagent_name),
        is_built_in: loaded_skill.origin == "builtin",
        invoking_request_id: Some(request.tool_use_id.clone()),
        invocation_kind: Some("spawn".to_owned()),
        parent_query_depth: model_tool_u32(input, &["parent_query_depth"]).unwrap_or_default(),
        parent_sequence: Some(event_sequence),
        run_in_background,
        model_override: execution_policy.model.clone(),
        effort_override: execution_policy.effort.clone(),
        backend_context,
    })
}

fn resolve_skill_fork_profile(
    execution_policy: &SkillExecutionPolicy,
    config: &ProjectConfig,
    parent_agent_id: &str,
) -> Result<String, ApiError> {
    let requested_agent = execution_policy
        .agent
        .as_deref()
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .map(str::to_owned);
    if let Some(requested_agent) = requested_agent {
        return config
            .task_profiles
            .contains_key(&requested_agent)
            .then_some(requested_agent.clone())
            .ok_or_else(|| {
                ApiError::bad_request(format!(
                    "Skill fork requests unknown task profile '{requested_agent}'"
                ))
            });
    }
    if config.task_profiles.contains_key(parent_agent_id) {
        return Ok(parent_agent_id.to_owned());
    }
    let mut profiles = config.task_profiles.keys().cloned();
    if let Some(only_profile) = profiles.next() {
        if profiles.next().is_none() {
            return Ok(only_profile);
        }
    }
    Err(ApiError::bad_request(
        "Skill fork must specify a task profile when the parent profile is unavailable and the config does not define exactly one task profile",
    ))
}

fn skill_fork_task(input: &Value, loaded_skill: &LoadedModelSkill) -> String {
    let args = model_tool_string(
        input,
        &[
            "args",
            "argument",
            "arguments",
            "prompt",
            "user_prompt",
            "task",
        ],
    );
    let args_section = args
        .as_deref()
        .map(|args| format!("\n\nInvocation arguments:\n{args}"))
        .unwrap_or_default();
    format!(
        "Run the following SkillTool skill in an isolated forked subagent. Follow the skill instructions and return a concise result to the parent.\n\nSkill: {}\n\n{}{}",
        loaded_skill.display_name, loaded_skill.content, args_section
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_fork_profile_resolution_uses_the_only_profile_when_parent_is_external() {
        let policy = SkillExecutionPolicy::default();
        let config = crate::default_project_config();
        let resolution = resolve_skill_fork_profile(&policy, &config, "model-tool").unwrap();

        assert_eq!(resolution, "code");
    }

    #[test]
    fn skill_fork_profile_resolution_inherits_configured_parent() {
        let policy = SkillExecutionPolicy::default();
        let config = crate::default_project_config();
        let resolution = resolve_skill_fork_profile(&policy, &config, "code").unwrap();

        assert_eq!(resolution, "code");
    }

    #[test]
    fn skill_fork_profile_resolution_rejects_unknown_requested_profile() {
        let policy = SkillExecutionPolicy {
            agent: Some("reviewer".to_owned()),
            ..SkillExecutionPolicy::default()
        };
        let config = crate::default_project_config();
        let error = resolve_skill_fork_profile(&policy, &config, "model-tool").unwrap_err();

        assert!(error.message.contains("unknown task profile 'reviewer'"));
    }

    #[test]
    fn skill_fork_profile_resolution_accepts_matching_config_profile() {
        let policy = SkillExecutionPolicy {
            agent: Some("code".to_owned()),
            ..SkillExecutionPolicy::default()
        };
        let config = crate::default_project_config();
        let resolution = resolve_skill_fork_profile(&policy, &config, "model-tool").unwrap();

        assert_eq!(resolution, "code");
    }
}
