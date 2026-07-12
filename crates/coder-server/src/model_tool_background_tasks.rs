use serde_json::{json, Value};
use std::time::{Duration, Instant};

use crate::{background_commands::background_command_status_since, ApiError, ApiState};

const TASK_OUTPUT_DEFAULT_TIMEOUT_MS: u64 = 30_000;
const TASK_OUTPUT_MAX_TIMEOUT_MS: u64 = 600_000;
const COMMAND_OUTPUT_DEFAULT_TIMEOUT_MS: u64 = 5_000;
const COMMAND_OUTPUT_MAX_TIMEOUT_MS: u64 = 300_000;
const TASK_OUTPUT_STATUS_POLL_INTERVAL_MS: u64 = 100;

#[derive(Debug, Clone, Copy)]
pub(crate) struct BackgroundWaitOptions {
    block: bool,
    timeout_ms: u64,
}

#[derive(serde::Deserialize)]
pub(crate) struct TaskIdInput {
    task_id: String,
    #[serde(default)]
    cursor: Option<u64>,
}

impl TaskIdInput {
    pub(crate) fn resolved_task_id(&self) -> Result<String, ApiError> {
        let task_id = self.task_id.trim();
        if task_id.is_empty() {
            return Err(ApiError::bad_request("Missing required parameter: task_id"));
        }
        Ok(task_id.to_owned())
    }

    pub(crate) fn cursor(&self) -> Option<u64> {
        self.cursor
    }
}

pub(crate) fn background_wait_options(input: &Value) -> BackgroundWaitOptions {
    resolved_background_wait_options(
        input,
        false,
        TASK_OUTPUT_DEFAULT_TIMEOUT_MS,
        TASK_OUTPUT_MAX_TIMEOUT_MS,
    )
}

pub(crate) fn command_background_wait_options(input: &Value) -> BackgroundWaitOptions {
    resolved_background_wait_options(
        input,
        true,
        COMMAND_OUTPUT_DEFAULT_TIMEOUT_MS,
        COMMAND_OUTPUT_MAX_TIMEOUT_MS,
    )
}

fn resolved_background_wait_options(
    input: &Value,
    default_block: bool,
    default_timeout_ms: u64,
    max_timeout_ms: u64,
) -> BackgroundWaitOptions {
    let block = model_tool_semantic_bool(input, &["block"]).unwrap_or(default_block);
    let timeout_ms = model_tool_u64(input, &["timeout", "timeout_ms", "timeoutMs"])
        .unwrap_or(default_timeout_ms)
        .min(max_timeout_ms);
    BackgroundWaitOptions { block, timeout_ms }
}

pub(crate) async fn model_tool_background_command_status(
    state: &ApiState,
    task_id: &str,
    cursor: Option<u64>,
    options: BackgroundWaitOptions,
) -> Result<Value, ApiError> {
    let started = Instant::now();
    loop {
        let response = background_command_status_since(state, task_id, cursor)?;
        let is_running = model_tool_task_status_is_running(&response.status);
        if !is_running {
            return background_status_payload(response, "success", options);
        }
        if !options.block {
            return background_status_payload(response, "not_ready", options);
        }
        if started.elapsed() >= Duration::from_millis(options.timeout_ms) {
            return background_status_payload(response, "timeout", options);
        }
        tokio::time::sleep(background_wait_poll_delay(started, options.timeout_ms)).await;
    }
}

pub(crate) async fn model_tool_background_subagent_status(
    state: &ApiState,
    task_id: &str,
    options: BackgroundWaitOptions,
) -> Result<Value, ApiError> {
    let started = Instant::now();
    loop {
        let response = crate::subagent_tools::background_subagent_status(state, task_id)?;
        let is_running = model_tool_task_status_is_running(&response.status);
        if !is_running {
            return background_status_payload(response, "success", options);
        }
        if !options.block {
            return background_status_payload(response, "not_ready", options);
        }
        if started.elapsed() >= Duration::from_millis(options.timeout_ms) {
            return background_status_payload(response, "timeout", options);
        }
        tokio::time::sleep(background_wait_poll_delay(started, options.timeout_ms)).await;
    }
}

fn background_wait_poll_delay(started: Instant, timeout_ms: u64) -> Duration {
    let timeout = Duration::from_millis(timeout_ms);
    let remaining = timeout.saturating_sub(started.elapsed());
    remaining.min(Duration::from_millis(TASK_OUTPUT_STATUS_POLL_INTERVAL_MS))
}

fn model_tool_task_status_is_running(status: &str) -> bool {
    matches!(status, "running" | "pending" | "backgrounded")
}

fn background_status_payload<T: serde::Serialize>(
    value: T,
    retrieval_status: &'static str,
    options: BackgroundWaitOptions,
) -> Result<Value, ApiError> {
    let mut value = to_value(value)?;
    if let Value::Object(payload) = &mut value {
        payload.insert("retrieval_status".to_owned(), json!(retrieval_status));
        payload.insert("block".to_owned(), json!(options.block));
        payload.insert("timeout_ms".to_owned(), json!(options.timeout_ms));
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

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|error| ApiError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_wait_defaults_match_codex_empty_poll_bounds() {
        let defaults = command_background_wait_options(&json!({}));
        assert!(defaults.block);
        assert_eq!(defaults.timeout_ms, 5_000);

        let clamped = command_background_wait_options(&json!({"timeout": 999_999}));
        assert_eq!(clamped.timeout_ms, 300_000);
    }

    #[test]
    fn background_wait_allows_explicit_non_blocking_status_read() {
        let options = command_background_wait_options(&json!({"block": false, "timeout": 25}));
        assert!(!options.block);
        assert_eq!(options.timeout_ms, 25);
    }
}
