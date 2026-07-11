use axum::extract::{Path, State};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use crate::{
    background_command_status, cancel_background_command_endpoint,
    cancel_background_subagent_endpoint, ApiError, ApiState,
};

const CLAUDE_TASK_OUTPUT_DEFAULT_TIMEOUT_MS: u64 = 30_000;
const CLAUDE_TASK_OUTPUT_MAX_TIMEOUT_MS: u64 = 600_000;
const TASK_OUTPUT_STATUS_POLL_INTERVAL_MS: u64 = 100;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ModelToolTaskOutputOptions {
    block: bool,
    timeout_ms: u64,
}

#[derive(Debug)]
pub(crate) struct ModelToolTaskStopPermissionResolution {
    pub(crate) required_permission: Option<&'static str>,
    pub(crate) payload: Value,
}

#[derive(serde::Deserialize)]
pub(crate) struct TaskIdInput {
    task_id: Option<String>,
    #[serde(default)]
    shell_id: Option<String>,
}

impl TaskIdInput {
    pub(crate) fn resolved_task_id(&self) -> Result<String, ApiError> {
        self.task_id
            .as_deref()
            .or(self.shell_id.as_deref())
            .map(str::trim)
            .filter(|task_id| !task_id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| ApiError::bad_request("Missing required parameter: task_id"))
    }
}

#[derive(Debug, Clone)]
enum ModelToolBackgroundTaskKind {
    Command { status: String, command: String },
    Subagent { status: String, agent_id: String },
}

impl ModelToolBackgroundTaskKind {
    fn task_type(&self) -> &'static str {
        match self {
            Self::Command { .. } => "local_bash",
            Self::Subagent { .. } => "local_agent",
        }
    }

    fn required_permission(&self) -> &'static str {
        match self {
            Self::Command { .. } => "run_commands",
            Self::Subagent { .. } => "child_harness_permissions",
        }
    }

    fn status(&self) -> &str {
        match self {
            Self::Command { status, .. } | Self::Subagent { status, .. } => status,
        }
    }

    fn command(&self) -> String {
        match self {
            Self::Command { command, .. } => command.clone(),
            Self::Subagent { agent_id, .. } => format!("agent:{agent_id}"),
        }
    }
}

pub(crate) fn model_tool_task_output_options(
    requested_tool_name: &str,
    input: &Value,
) -> ModelToolTaskOutputOptions {
    let block = model_tool_semantic_bool(input, &["block"])
        .unwrap_or_else(|| is_claude_task_output_alias(requested_tool_name));
    let timeout_ms = model_tool_u64(input, &["timeout", "timeout_ms", "timeoutMs"])
        .unwrap_or(CLAUDE_TASK_OUTPUT_DEFAULT_TIMEOUT_MS)
        .min(CLAUDE_TASK_OUTPUT_MAX_TIMEOUT_MS);
    ModelToolTaskOutputOptions { block, timeout_ms }
}

pub(crate) fn is_claude_task_output_alias(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "TaskOutput" | "task_output" | "AgentOutputTool" | "BashOutputTool"
    )
}

pub(crate) fn model_tool_task_stop_permission_resolution(
    state: &ApiState,
    input: &Value,
) -> ModelToolTaskStopPermissionResolution {
    let Some(task_id) = model_tool_task_id_from_input(input) else {
        return ModelToolTaskStopPermissionResolution {
            required_permission: None,
            payload: json!({
                "contract": "coder.model_tool_task_stop_resolution.v1",
                "status": "missing_task_id",
                "required_permission": Value::Null,
                "claude_sources": claude_task_stop_sources()
            }),
        };
    };

    match model_tool_background_task_kind(state, &task_id) {
        Ok(task) => ModelToolTaskStopPermissionResolution {
            required_permission: Some(task.required_permission()),
            payload: json!({
                "contract": "coder.model_tool_task_stop_resolution.v1",
                "status": "resolved",
                "task_id": task_id,
                "task_type": task.task_type(),
                "task_status": task.status(),
                "required_permission": task.required_permission(),
                "command": task.command(),
                "claude_sources": claude_task_stop_sources()
            }),
        },
        Err(error) => ModelToolTaskStopPermissionResolution {
            required_permission: None,
            payload: json!({
                "contract": "coder.model_tool_task_stop_resolution.v1",
                "status": "unresolved",
                "task_id": task_id,
                "required_permission": Value::Null,
                "error": error.message,
                "claude_sources": claude_task_stop_sources()
            }),
        },
    }
}

pub(crate) async fn model_tool_stop_background_task(
    state: &ApiState,
    input: &TaskIdInput,
) -> Result<Value, ApiError> {
    let task_id = input.resolved_task_id()?;
    let task = model_tool_background_task_kind(state, &task_id)?;
    if task.status() != "running" {
        let task_type = task.task_type();
        let required_permission = task.required_permission();
        let command = task.command();
        let task_status = task.status().to_owned();
        return Ok(task_stop_payload(
            &task_id,
            task_type,
            &command,
            false,
            &task_status,
            required_permission,
            "Task already reached a terminal status before TaskStop executed.",
        ));
    }
    let task_type = task.task_type();
    let required_permission = task.required_permission();
    let command = task.command();
    let (cancelled, task_status) = match task {
        ModelToolBackgroundTaskKind::Command { .. } => {
            let response =
                cancel_background_command_endpoint(State(state.clone()), Path(task_id.clone()))
                    .await?
                    .0;
            (response.cancelled, response.status)
        }
        ModelToolBackgroundTaskKind::Subagent { .. } => {
            let response =
                cancel_background_subagent_endpoint(State(state.clone()), Path(task_id.clone()))
                    .await?
                    .0;
            (response.cancelled, response.status)
        }
    };

    Ok(task_stop_payload(
        &task_id,
        task_type,
        &command,
        cancelled,
        &task_status,
        required_permission,
        "Successfully stopped task.",
    ))
}

fn task_stop_payload(
    task_id: &str,
    task_type: &str,
    command: &str,
    cancelled: bool,
    task_status: &str,
    required_permission: &str,
    message: &str,
) -> Value {
    json!({
        "status": "completed",
        "message": format!("{message} task: {task_id} ({command})"),
        "task_id": task_id,
        "task_type": task_type,
        "command": command,
        "cancelled": cancelled,
        "task_status": task_status,
        "required_permission": required_permission,
        "task_stop_policy": {
            "contract": "coder.model_tool_task_stop.v1",
            "claude_sources": claude_task_stop_sources(),
            "accepted_task_id_fields": ["task_id", "shell_id"],
            "running_status_required": false,
            "terminal_status_behavior": "completed_noop"
        }
    })
}

pub(crate) async fn model_tool_background_command_status(
    state: &ApiState,
    task_id: &str,
    options: ModelToolTaskOutputOptions,
) -> Result<Value, ApiError> {
    let started = Instant::now();
    loop {
        let response = background_command_status(state, task_id)?;
        let is_running = model_tool_task_status_is_running(&response.status);
        if !is_running {
            return task_output_payload(response, "success", options);
        }
        if !options.block {
            return task_output_payload(response, "not_ready", options);
        }
        if started.elapsed() >= Duration::from_millis(options.timeout_ms) {
            return task_output_payload(response, "timeout", options);
        }
        tokio::time::sleep(model_tool_task_output_poll_delay(
            started,
            options.timeout_ms,
        ))
        .await;
    }
}

pub(crate) async fn model_tool_background_any_task_status(
    state: &ApiState,
    task_id: &str,
    options: ModelToolTaskOutputOptions,
) -> Result<Value, ApiError> {
    match model_tool_background_subagent_status(state, task_id, options).await {
        Ok(response) => Ok(response),
        Err(error) if model_tool_background_subagent_not_found(&error) => {
            model_tool_background_command_status(state, task_id, options).await
        }
        Err(error) => Err(error),
    }
}

pub(crate) async fn model_tool_background_subagent_status(
    state: &ApiState,
    task_id: &str,
    options: ModelToolTaskOutputOptions,
) -> Result<Value, ApiError> {
    let started = Instant::now();
    loop {
        let response = crate::subagent_tools::background_subagent_status(state, task_id)?;
        let is_running = model_tool_task_status_is_running(&response.status);
        if !is_running {
            return task_output_payload(response, "success", options);
        }
        if !options.block {
            return task_output_payload(response, "not_ready", options);
        }
        if started.elapsed() >= Duration::from_millis(options.timeout_ms) {
            return task_output_payload(response, "timeout", options);
        }
        tokio::time::sleep(model_tool_task_output_poll_delay(
            started,
            options.timeout_ms,
        ))
        .await;
    }
}

fn model_tool_background_task_kind(
    state: &ApiState,
    task_id: &str,
) -> Result<ModelToolBackgroundTaskKind, ApiError> {
    match crate::subagent_tools::background_subagent_status(state, task_id) {
        Ok(response) => {
            return Ok(ModelToolBackgroundTaskKind::Subagent {
                status: response.status,
                agent_id: response.agent_id,
            });
        }
        Err(error) if model_tool_background_subagent_not_found(&error) => {}
        Err(error) => return Err(error),
    }

    match background_command_status(state, task_id) {
        Ok(response) => Ok(ModelToolBackgroundTaskKind::Command {
            status: response.status,
            command: response.command,
        }),
        Err(error) if model_tool_background_command_not_found(&error) => Err(ApiError::not_found(
            format!("background task not found: {task_id}"),
        )),
        Err(error) => Err(error),
    }
}

fn model_tool_background_subagent_not_found(error: &ApiError) -> bool {
    error
        .message
        .contains("background subagent task not found:")
}

fn model_tool_background_command_not_found(error: &ApiError) -> bool {
    error.message.contains("background command task not found:")
}

fn model_tool_task_output_poll_delay(started: Instant, timeout_ms: u64) -> Duration {
    let timeout = Duration::from_millis(timeout_ms);
    let remaining = timeout.saturating_sub(started.elapsed());
    remaining.min(Duration::from_millis(TASK_OUTPUT_STATUS_POLL_INTERVAL_MS))
}

fn model_tool_task_status_is_running(status: &str) -> bool {
    matches!(status, "running" | "pending" | "backgrounded")
}

fn task_output_payload<T: serde::Serialize>(
    value: T,
    retrieval_status: &'static str,
    options: ModelToolTaskOutputOptions,
) -> Result<Value, ApiError> {
    let mut value = to_value(value)?;
    if let Value::Object(payload) = &mut value {
        payload.insert("retrieval_status".to_owned(), json!(retrieval_status));
        payload.insert("block".to_owned(), json!(options.block));
        payload.insert("timeout_ms".to_owned(), json!(options.timeout_ms));
        payload.insert(
            "task_output_policy".to_owned(),
            json!({
                "contract": "coder.model_tool_task_output.v1",
                "claude_sources": [
                    "packages/builtin-tools/src/tools/TaskOutputTool/TaskOutputTool.tsx inputSchema",
                    "packages/builtin-tools/src/tools/TaskOutputTool/TaskOutputTool.tsx waitForTaskCompletion"
                ],
                "default_block_for_task_output_alias": true,
                "default_timeout_ms": CLAUDE_TASK_OUTPUT_DEFAULT_TIMEOUT_MS,
                "max_timeout_ms": CLAUDE_TASK_OUTPUT_MAX_TIMEOUT_MS,
                "poll_interval_ms": TASK_OUTPUT_STATUS_POLL_INTERVAL_MS
            }),
        );
        return Ok(value);
    }
    Ok(json!({
        "retrieval_status": retrieval_status,
        "block": options.block,
        "timeout_ms": options.timeout_ms,
        "task": value
    }))
}

fn model_tool_u64(input: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| input.get(*key).and_then(Value::as_u64))
}

fn model_tool_semantic_bool(input: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| {
        let value = input.get(*key)?;
        if let Some(value) = value.as_bool() {
            return Some(value);
        }
        let text = value.as_str()?.trim().to_ascii_lowercase();
        match text.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    })
}

fn model_tool_task_id_from_input(input: &Value) -> Option<String> {
    input
        .get("task_id")
        .or_else(|| input.get("shell_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|task_id| !task_id.is_empty())
        .map(str::to_owned)
}

fn claude_task_stop_sources() -> Vec<&'static str> {
    vec![
        "packages/builtin-tools/src/tools/TaskStopTool/TaskStopTool.ts inputSchema",
        "packages/builtin-tools/src/tools/TaskStopTool/TaskStopTool.ts validateInput",
        "packages/builtin-tools/src/tools/TaskStopTool/TaskStopTool.ts call",
        "src/tasks/stopTask.js",
    ]
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|error| ApiError::internal(error.to_string()))
}
