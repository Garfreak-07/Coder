use axum::{
    extract::{Path, State},
    Json,
};
use coder_core::RunId;
use coder_harness::McpToolCallRequest;
use coder_workflow::TurnContext;
use serde_json::{json, Value};
use std::time::Duration;

use crate::background_commands::write_background_command_stdin;
use crate::mcp_runtime::invoke_mcp_tool_request;
use crate::model_tool_background_tasks::{
    background_wait_options, command_background_wait_options, model_tool_background_command_status,
    model_tool_background_subagent_status, TaskIdInput,
};
use crate::model_tool_builtin_operations::{
    execute_apply_patch, execute_edit_text_file, execute_finish, execute_write_text_file,
};
use crate::model_tool_input::{
    canonical_model_tool_name, model_tool_bool, model_tool_object, model_tool_string,
    model_tool_u32, model_tool_u64, parse_input, to_value,
};
use crate::model_tool_permissions::{
    model_tool_context_run_id, read_run_project_config_snapshot,
    DEFAULT_MODEL_TOOL_PERMISSION_HARNESS_ID,
};
use crate::model_tool_run_context::latest_run_context;
use crate::model_tool_skill_execution::execute_skill_model_tool;
use crate::{
    apply_patch_endpoint, apply_provider_settings_to_project_config,
    cancel_background_command_endpoint, cancel_background_subagent_endpoint, git_diff_endpoint,
    git_status_endpoint, preview_patch_endpoint, repo_find_files_endpoint, repo_read_file_endpoint,
    repo_read_file_range_endpoint, repo_search_text_endpoint, run_command_endpoint,
    start_background_command_endpoint, ApiError, ApiState, CommandBackgroundStartRequest,
    CommandRunToolRequest, GitDiffRequest, GitStatusRequest, ModelToolExecuteRequest,
    PatchApplyToolRequest, PatchPreviewRequest, RepoFindFilesRequest, RepoReadFileRangeRequest,
    RepoReadFileRequest, RepoSearchTextRequest, SubagentRunToolRequest,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelMcpToolRoute {
    pub(crate) server_id: String,
    pub(crate) tool_name: String,
}

pub(crate) async fn execute_model_tool_request(
    state: ApiState,
    request: ModelToolExecuteRequest,
    host_context: &TurnContext,
) -> Result<Value, ApiError> {
    execute_model_tool_request_with_route(state, request, host_context, None).await
}

pub(crate) async fn execute_model_tool_request_with_route(
    state: ApiState,
    request: ModelToolExecuteRequest,
    host_context: &TurnContext,
    mcp_route: Option<&ModelMcpToolRoute>,
) -> Result<Value, ApiError> {
    if let Some(route) = mcp_route {
        let response = invoke_mcp_tool_request(
            &state,
            McpToolCallRequest {
                server_id: route.server_id.clone(),
                tool_name: route.tool_name.clone(),
                args: request.input,
                run_id: host_context
                    .run_id
                    .as_ref()
                    .map(|run_id| RunId::from_string(run_id.clone())),
                approved: host_context.start_work_authorized,
            },
        )
        .await?;
        return to_value(response);
    }

    let tool_name = canonical_model_tool_name(&request.tool_name);
    match tool_name {
        "repo_find_files" => {
            let response = repo_find_files_endpoint(
                State(state),
                Json(parse_input::<RepoFindFilesRequest>(&request.input)?),
            )
            .await?;
            to_value(response.0)
        }
        "repo_search_text" => {
            let response = repo_search_text_endpoint(
                State(state),
                Json(parse_input::<RepoSearchTextRequest>(&request.input)?),
            )
            .await?;
            to_value(response.0)
        }
        "repo_read_file" => {
            let response = repo_read_file_endpoint(
                State(state),
                Json(parse_input::<RepoReadFileRequest>(&request.input)?),
            )
            .await?;
            to_value(response.0)
        }
        "repo_read_file_range" => {
            let response = repo_read_file_range_endpoint(
                State(state),
                Json(parse_input::<RepoReadFileRangeRequest>(&request.input)?),
            )
            .await?;
            to_value(response.0)
        }
        "git_status" => {
            let response = git_status_endpoint(
                State(state),
                Json(parse_input::<GitStatusRequest>(&request.input)?),
            )
            .await?;
            to_value(response.0)
        }
        "git_diff" => {
            let response = git_diff_endpoint(
                State(state),
                Json(parse_input::<GitDiffRequest>(&request.input)?),
            )
            .await?;
            to_value(response.0)
        }
        "command_run" => {
            let response = run_command_endpoint(
                State(state),
                Json(parse_input::<CommandRunToolRequest>(&request.input)?),
            )
            .await?;
            to_value(response.0)
        }
        "command_background" => {
            let response = start_background_command_endpoint(
                State(state),
                Json(parse_input::<CommandBackgroundStartRequest>(
                    &request.input,
                )?),
            )
            .await?;
            to_value(response.0)
        }
        "read_command_output" => {
            let input = parse_input::<TaskIdInput>(&request.input)?;
            let task_id = input.resolved_task_id()?;
            let options = command_background_wait_options(&request.input);
            model_tool_background_command_status(&state, &task_id, input.cursor(), options).await
        }
        "write_stdin" => {
            let input = parse_input::<TaskIdInput>(&request.input)?;
            let task_id = input.resolved_task_id()?;
            let text = request
                .input
                .get("input")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let close_stdin = request
                .input
                .get("close_stdin")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let write = write_background_command_stdin(&state, &task_id, text, close_stdin)?;
            let options = command_background_wait_options(&request.input);
            let mut payload =
                model_tool_background_command_status(&state, &task_id, input.cursor(), options)
                    .await?;
            if let Value::Object(payload) = &mut payload {
                payload.insert("stdin".to_owned(), to_value(write)?);
            }
            Ok(payload)
        }
        "cancel_command_background" => {
            let input = parse_input::<TaskIdInput>(&request.input)?;
            let task_id = input.resolved_task_id()?;
            let response = cancel_background_command_endpoint(State(state), Path(task_id))
                .await?
                .0;
            to_value(response)
        }
        "patch_preview" => {
            let response =
                preview_patch_endpoint(Json(parse_input::<PatchPreviewRequest>(&request.input)?))
                    .await?;
            to_value(response.0)
        }
        "patch_apply" => {
            let response = apply_patch_endpoint(
                State(state),
                Json(parse_input::<PatchApplyToolRequest>(&request.input)?),
            )
            .await?;
            to_value(response.0)
        }
        "apply_patch" => execute_apply_patch(&state, &request.input, host_context),
        "agent_subagent" => {
            let subagent_request = model_tool_subagent_run_request(&state, &request, host_context)?;
            let response =
                crate::subagent_tools::run_subagent_endpoint(State(state), Json(subagent_request))
                    .await?;
            to_value(response.0)
        }
        "read_subagent_status" => {
            let input = parse_input::<TaskIdInput>(&request.input)?;
            let task_id = input.resolved_task_id()?;
            let options = background_wait_options(&request.input);
            model_tool_background_subagent_status(&state, &task_id, options).await
        }
        "cancel_subagent_background" => {
            let input = parse_input::<TaskIdInput>(&request.input)?;
            let task_id = input.resolved_task_id()?;
            let response = cancel_background_subagent_endpoint(State(state), Path(task_id))
                .await?
                .0;
            to_value(response)
        }
        "skill" => execute_skill_model_tool(&state, &request, host_context).await,
        "sleep" => {
            let response = execute_sleep_model_tool(&request.input).await;
            to_value(response)
        }
        "write_text_file" => execute_write_text_file(&state, &request.input, host_context),
        "edit_text_file" => execute_edit_text_file(&state, &request.input, host_context),
        "finish" => execute_finish(&request.input),
        _ => Err(ApiError::bad_request(format!(
            "No such tool available: {}",
            request.tool_name
        ))),
    }
}

async fn execute_sleep_model_tool(input: &Value) -> Value {
    let duration_ms = sleep_duration_ms(input);
    if duration_ms > 0 {
        tokio::time::sleep(Duration::from_millis(duration_ms)).await;
    }
    json!({
        "contract": "coder.sleep_tool.v1",
        "source": "coder-server",
        "status": "completed",
        "duration_ms": duration_ms
    })
}

fn sleep_duration_ms(input: &Value) -> u64 {
    let duration_ms = input
        .get("duration_ms")
        .or_else(|| input.get("durationMs"))
        .or_else(|| input.get("milliseconds"))
        .and_then(Value::as_u64)
        .or_else(|| {
            input
                .get("seconds")
                .and_then(Value::as_f64)
                .map(|seconds| (seconds.max(0.0) * 1000.0) as u64)
        })
        .unwrap_or(0);
    duration_ms.min(120_000)
}

fn model_tool_subagent_run_request(
    state: &ApiState,
    request: &ModelToolExecuteRequest,
    host_context: &TurnContext,
) -> Result<SubagentRunToolRequest, ApiError> {
    let input = &request.input;
    let run_id = model_tool_context_run_id(input, host_context);
    let run_context = run_id
        .as_deref()
        .and_then(|run_id| latest_run_context(&state.store, run_id))
        .unwrap_or_default();
    let inherited_plan_context = run_context.plan_context.clone();
    let mut config = run_id
        .as_deref()
        .and_then(|run_id| read_run_project_config_snapshot(&state.store, run_id))
        .unwrap_or_else(crate::default_project_config);
    let provider_settings = state.provider_settings.lock().unwrap().clone();
    apply_provider_settings_to_project_config(&mut config, &provider_settings);
    let workflow_id = model_tool_string(input, &["workflow_id"])
        .or(run_context.workflow_id)
        .unwrap_or_else(|| "model-tool".to_owned());
    let node_id = model_tool_string(input, &["node_id"])
        .or(run_context.node_id)
        .unwrap_or_else(|| "model-tool".to_owned());
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
    let task = model_tool_string(input, &["task", "prompt"])
        .ok_or_else(|| ApiError::bad_request("agent_subagent input requires task or prompt"))?;
    let subagent_name = model_tool_string(
        input,
        &["subagent_name", "subagent_type", "name", "description"],
    );
    let backend_context = model_tool_object(input, "backend_context")
        .unwrap_or_else(|| inherited_model_tool_backend_context(inherited_plan_context));
    let parent_query_depth = model_tool_u32(input, &["parent_query_depth"])
        .or_else(|| {
            [
                "/coder/subagent/context/query_tracking/depth",
                "/coder_subagent/context/query_tracking/depth",
            ]
            .iter()
            .find_map(|pointer| backend_context.pointer(pointer))
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
        })
        .unwrap_or_default();
    let run_in_background = model_tool_bool(input, &["run_in_background", "runInBackground"])
        .or_else(|| {
            input
                .get("fork")
                .and_then(Value::as_bool)
                .filter(|fork| *fork)
        });
    Ok(SubagentRunToolRequest {
        config,
        workflow_id,
        node_id,
        parent_agent_id,
        parent_harness_id,
        repo_root,
        task,
        run_id,
        agent_id: model_tool_string(input, &["child_agent_id", "agent_id", "agentId"]),
        subagent_name,
        is_built_in: model_tool_bool(input, &["is_built_in", "isBuiltIn"]).unwrap_or(false),
        invoking_request_id: model_tool_string(input, &["invoking_request_id", "request_id"])
            .or_else(|| Some(request.tool_use_id.clone())),
        invocation_kind: model_tool_string(input, &["invocation_kind"])
            .or_else(|| Some("spawn".to_owned())),
        parent_query_depth,
        parent_sequence: model_tool_u64(input, &["parent_sequence"]),
        run_in_background,
        model_override: model_tool_string(input, &["model"]),
        effort_override: input.get("effort").cloned(),
        backend_context,
    })
}

fn inherited_model_tool_backend_context(plan_context: Option<Value>) -> Value {
    let Some(plan_context) = plan_context else {
        return json!({});
    };
    json!({
        "coder": {
            "plan_context": plan_context,
            "backend_context_source": "run_started_plan_context"
        }
    })
}
