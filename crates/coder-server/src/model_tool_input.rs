use coder_core::RunId;
use coder_workflow::{ModelToolHostContext, ModelToolUseBlock};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::{ApiError, ModelToolExecuteRequest, ModelToolUseRequestBlock};

pub(crate) fn canonical_model_tool_name(tool_name: &str) -> &'static str {
    match tool_name {
        "repo_find_files" | "find_files" | "repo_files" | "search_files" => "repo_find_files",
        "repo_search_text" | "repo_search" | "search_text" => "repo_search_text",
        "repo_read_file" | "read_file" => "repo_read_file",
        "repo_read_file_range" | "read_file_range" => "repo_read_file_range",
        "git_status" => "git_status",
        "git_diff" | "inspect_git_diff" => "git_diff",
        "command_run" | "run_command" | "run_command_sandbox" => "command_run",
        "command_background" | "bash_background" => "command_background",
        "read_command_output" => "read_command_output",
        "cancel_command_background" => "cancel_command_background",
        "patch_preview" | "preview_patch" | "propose_patch" => "patch_preview",
        "patch_apply" | "apply_patch" | "apply_patch_sandbox" => "patch_apply",
        "agent_subagent" | "Agent" | "agent" | "Task" | "task" | "subagent" => "agent_subagent",
        "Skill" | "skill" | "SkillTool" | "skill_tool" => "skill",
        "read_subagent_status"
        | "TaskOutput"
        | "task_output"
        | "AgentOutputTool"
        | "BashOutputTool" => "read_subagent_status",
        "cancel_subagent_background" => "cancel_subagent_background",
        "TaskStop" | "task_stop" | "KillShell" | "kill_shell" => "task_stop",
        "sleep" | "Sleep" | "sleep_tool" | "SleepTool" => "sleep",
        "write_text_file" | "write_file" | "file_write" => "write_text_file",
        "finish" | "final" | "final_report" => "finish",
        _ => "unknown",
    }
}

pub(crate) fn apply_model_tool_defaults(request: &mut ModelToolExecuteRequest) -> Value {
    if canonical_model_tool_name(&request.tool_name) != "command_run" {
        return json!({});
    }
    let Some(input) = request.input.as_object_mut() else {
        return json!({});
    };

    let mut defaults = serde_json::Map::new();
    if !input.contains_key("background_on_timeout") {
        input.insert("background_on_timeout".to_owned(), Value::Bool(true));
        defaults.insert("background_on_timeout".to_owned(), Value::Bool(true));
    }
    if !input.contains_key("foreground_timeout_seconds") {
        input.insert(
            "foreground_timeout_seconds".to_owned(),
            json!(coder_tools::DEFAULT_COMMAND_TIMEOUT_SECONDS),
        );
        defaults.insert(
            "foreground_timeout_seconds".to_owned(),
            json!(coder_tools::DEFAULT_COMMAND_TIMEOUT_SECONDS),
        );
    }
    Value::Object(defaults)
}

pub(crate) fn apply_model_tool_policy_approval_defaults(
    canonical_tool_name: &str,
    permission_decision_status: &str,
    input: &mut Value,
) -> Value {
    if !matches!(
        permission_decision_status,
        "allowed_by_policy" | "allowed_by_skill_context_modifier"
    ) || !model_tool_uses_underlying_approval(canonical_tool_name)
    {
        return json!({});
    }
    let Some(input) = input.as_object_mut() else {
        return json!({});
    };
    if input.get("approved").and_then(Value::as_bool).is_some() {
        return json!({});
    }
    input.insert("approved".to_owned(), Value::Bool(true));
    json!({
        "approved": true,
        "reason": match permission_decision_status {
            "allowed_by_skill_context_modifier" => "active_skill_context_modifier_allowed_tool",
            _ => "active_permission_policy_allowed_tool"
        }
    })
}

fn model_tool_uses_underlying_approval(canonical_tool_name: &str) -> bool {
    matches!(
        canonical_tool_name,
        "command_run" | "command_background" | "patch_apply"
    )
}

pub(crate) fn apply_model_tool_request_context(
    request: &mut ModelToolExecuteRequest,
) -> ModelToolHostContext {
    if let Some(run_id) = request
        .run_id
        .as_deref()
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
    {
        if let Some(input) = request.input.as_object_mut() {
            input
                .entry("run_id".to_owned())
                .or_insert_with(|| Value::String(run_id.to_owned()));
        }
    }
    ModelToolHostContext {
        run_id: request
            .run_id
            .as_deref()
            .map(str::trim)
            .filter(|run_id| !run_id.is_empty())
            .map(str::to_owned),
        harness_id: request
            .harness_id
            .as_deref()
            .map(str::trim)
            .filter(|harness_id| !harness_id.is_empty())
            .map(str::to_owned),
        agent_id: request
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
            .filter(|agent_id| !agent_id.is_empty())
            .map(str::to_owned),
        skill_context_modifiers: request.skill_context_modifiers.clone(),
        current_model: request
            .current_model
            .as_deref()
            .or_else(|| {
                request
                    .input
                    .get("current_model")
                    .or_else(|| request.input.get("currentModel"))
                    .or_else(|| request.input.get("mainLoopModel"))
                    .and_then(Value::as_str)
            })
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_owned),
        current_effort: request.current_effort.clone().or_else(|| {
            request
                .input
                .get("current_effort")
                .or_else(|| request.input.get("currentEffort"))
                .or_else(|| request.input.get("effortValue"))
                .filter(|value| !value.is_null())
                .cloned()
        }),
        ..ModelToolHostContext::default()
    }
}

pub(crate) fn model_tool_string(input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        input
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

pub(crate) fn model_tool_bool(input: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| input.get(*key).and_then(Value::as_bool))
}

pub(crate) fn model_tool_u32(input: &Value, keys: &[&str]) -> Option<u32> {
    keys.iter()
        .find_map(|key| input.get(*key).and_then(Value::as_u64))
        .and_then(|value| u32::try_from(value).ok())
}

pub(crate) fn model_tool_u64(input: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| input.get(*key).and_then(Value::as_u64))
}

pub(crate) fn model_tool_object(input: &Value, key: &str) -> Option<Value> {
    let value = input.get(key)?.clone();
    if value.is_object() {
        Some(value)
    } else {
        None
    }
}

pub(crate) fn model_tool_use_blocks(
    blocks: Vec<ModelToolUseRequestBlock>,
) -> Result<Vec<ModelToolUseBlock>, ApiError> {
    blocks
        .into_iter()
        .map(|block| {
            if block.id.trim().is_empty() {
                return Err(ApiError::bad_request("tool_use id is required"));
            }
            if block.name.trim().is_empty() {
                return Err(ApiError::bad_request("tool_use name is required"));
            }
            Ok(ModelToolUseBlock::new(block.id, block.name, block.input))
        })
        .collect()
}

pub(crate) fn parse_input<T: DeserializeOwned>(input: &Value) -> Result<T, ApiError> {
    serde_json::from_value(input.clone())
        .map_err(|error| ApiError::bad_request(format!("invalid tool input: {error}")))
}

pub(crate) fn to_value<T: serde::Serialize>(value: T) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|error| ApiError::internal(error.to_string()))
}

pub(crate) fn run_id_from_model_tool_input(input: &Value) -> Option<RunId> {
    input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
        .map(|run_id| RunId::from_string(run_id.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(tool_name: &str, input: Value) -> ModelToolExecuteRequest {
        ModelToolExecuteRequest {
            tool_use_id: "toolu_1".to_owned(),
            tool_name: tool_name.to_owned(),
            run_id: None,
            harness_id: None,
            agent_id: None,
            current_model: None,
            current_effort: None,
            skill_context_modifiers: Vec::new(),
            input,
        }
    }

    #[test]
    fn model_tool_input_normalizes_claude_tool_aliases() {
        assert_eq!(canonical_model_tool_name("Agent"), "agent_subagent");
        assert_eq!(
            canonical_model_tool_name("BashOutputTool"),
            "read_subagent_status"
        );
        assert_eq!(canonical_model_tool_name("KillShell"), "task_stop");
        assert_eq!(canonical_model_tool_name("SleepTool"), "sleep");
        assert_eq!(
            canonical_model_tool_name("bash_background"),
            "command_background"
        );
        assert_eq!(canonical_model_tool_name("write_file"), "write_text_file");
        assert_eq!(canonical_model_tool_name("final_report"), "finish");
    }

    #[test]
    fn model_tool_input_applies_command_background_defaults_only_for_command_run() {
        let mut command = request("command_run", json!({}));
        let defaults = apply_model_tool_defaults(&mut command);
        assert_eq!(defaults["background_on_timeout"], json!(true));
        assert_eq!(
            command.input["foreground_timeout_seconds"],
            json!(coder_tools::DEFAULT_COMMAND_TIMEOUT_SECONDS)
        );

        let mut read = request("repo_read_file", json!({}));
        assert_eq!(apply_model_tool_defaults(&mut read), json!({}));
        assert!(read.input.get("background_on_timeout").is_none());
    }

    #[test]
    fn model_tool_input_projects_host_context_and_input_run_id() {
        let mut request = request(
            "repo_read_file",
            json!({
                "agentId": "agent-1",
                "currentModel": "deepseek-v4-flash",
                "effortValue": "low"
            }),
        );
        request.run_id = Some(" run-1 ".to_owned());
        request.harness_id = Some(" harness-1 ".to_owned());

        let context = apply_model_tool_request_context(&mut request);

        assert_eq!(context.run_id.as_deref(), Some("run-1"));
        assert_eq!(context.harness_id.as_deref(), Some("harness-1"));
        assert_eq!(context.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(context.current_model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(context.current_effort, Some(json!("low")));
        assert_eq!(request.input["run_id"], json!("run-1"));
    }
}
