use coder_config::{HookCommandSpec, HookEvent};
use coder_core::RunId;
use coder_store::RunStore;
use serde_json::{json, Value};
use std::{
    env,
    path::{Path, PathBuf},
    time::Instant,
};

use crate::model_tool_hook_output::{
    parse_model_tool_hook_output, ModelToolHookEffects, MODEL_TOOL_HOOK_OUTPUT_LIMIT_BYTES,
};
use crate::model_tool_hook_runtime::{append_model_tool_event, ModelToolHookExecution};

pub(crate) const CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS: u64 = 10 * 60;
pub(crate) const CLAUDE_DEFAULT_HOOK_SHELL: &str = "bash";
pub(crate) const CLAUDE_GIT_BASH_PATH_ENV: &str = "CLAUDE_CODE_GIT_BASH_PATH";
pub(crate) const ASYNC_REWAKE_NOTIFICATION_EVENT_KIND: &str =
    "model_tool.async_rewake.notification";
pub(crate) const ASYNC_HOOK_RESPONSE_EVENT_KIND: &str = "model_tool.async_hook.response";
pub(crate) const ASYNC_HOOK_RESPONSE_CONTRACT: &str = "coder.model_tool_async_hook_response.v1";

pub(crate) struct ParsedAsyncHookResponse {
    pub(crate) kind: &'static str,
    pub(crate) response: Value,
}

pub(crate) fn parse_async_hook_response_output(output: &str) -> Option<ParsedAsyncHookResponse> {
    if output.trim().is_empty() {
        return None;
    }
    for line in output.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if parsed.get("async").is_none() {
            return Some(ParsedAsyncHookResponse {
                kind: "hook_json",
                response: parsed,
            });
        }
    }
    Some(ParsedAsyncHookResponse {
        kind: "plain_text",
        response: json!({}),
    })
}

pub(crate) fn shell_command_hook_argv(command: &str, shell: Option<&str>) -> Vec<String> {
    match shell
        .unwrap_or(CLAUDE_DEFAULT_HOOK_SHELL)
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "bash" => vec![
            default_bash_hook_executable(),
            "-lc".to_owned(),
            command.to_owned(),
        ],
        "sh" => vec!["sh".to_owned(), "-lc".to_owned(), command.to_owned()],
        "powershell" => {
            let executable = if cfg!(windows) {
                "powershell.exe"
            } else {
                "pwsh"
            };
            vec![
                executable.to_owned(),
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                command.to_owned(),
            ]
        }
        "pwsh" => vec![
            "pwsh".to_owned(),
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            command.to_owned(),
        ],
        _ => vec![
            default_bash_hook_executable(),
            "-lc".to_owned(),
            command.to_owned(),
        ],
    }
}

pub(crate) fn execute_command_model_tool_hook(
    store: RunStore,
    hook: &HookCommandSpec,
    event: HookEvent,
    requested_tool_name: &str,
    hook_input: &Value,
    tool_input: &Value,
) -> ModelToolHookExecution {
    let HookCommandSpec::Command {
        command,
        shell,
        timeout,
        run_async,
        async_rewake,
        ..
    } = hook
    else {
        return ModelToolHookExecution {
            payload: json!({
                "type": command_hook_kind(hook),
                "outcome": "unsupported"
            }),
            blocking_error: None,
            effects: ModelToolHookEffects::default(),
        };
    };
    let Some(repo_root) = tool_input.get("repo_root").and_then(Value::as_str) else {
        return ModelToolHookExecution {
            payload: json!({
                "type": "command",
                "command": command,
                "outcome": "skipped_missing_repo_root"
            }),
            blocking_error: None,
            effects: ModelToolHookEffects::default(),
        };
    };
    let cwd = tool_input.get("cwd").and_then(Value::as_str).unwrap_or(".");
    let argv = shell_command_hook_argv(command, shell.as_deref());
    let timeout_seconds = timeout.unwrap_or(CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS);
    if *run_async || *async_rewake {
        return background_command_model_tool_hook(BackgroundCommandHookRequest {
            store,
            repo_root,
            cwd,
            argv,
            command,
            event,
            requested_tool_name,
            hook_input,
            timeout_seconds,
            run_async: *run_async,
            async_rewake: *async_rewake,
        });
    }
    let started = Instant::now();
    let output = coder_tools::run_command(
        repo_root,
        coder_tools::CommandRunRequest {
            cwd: cwd.into(),
            argv,
            timeout_seconds,
            max_output_bytes: MODEL_TOOL_HOOK_OUTPUT_LIMIT_BYTES,
            source: "hook".to_owned(),
            sandbox: false,
            approved: true,
            stdin: Some(format!("{hook_input}\n")),
        },
    );
    match output {
        Ok(output) => {
            let parsed_output = parse_model_tool_hook_output(&output.output, event, command);
            let blocking_error = if let Some(error) = parsed_output.blocking_error.clone() {
                Some(error)
            } else if output.returncode == Some(2) {
                Some(format!(
                    "[{}]: {}",
                    command,
                    if output.output.trim().is_empty() {
                        "No stderr output"
                    } else {
                        output.output.trim()
                    }
                ))
            } else {
                None
            };
            let outcome = if blocking_error.is_some() {
                "blocking"
            } else if output.passed {
                "success"
            } else {
                "non_blocking_error"
            };
            let hook_output_kind = parsed_output.kind;
            let hook_json_output = parsed_output.json_output.clone();
            let hook_output_validation_error = parsed_output.validation_error.clone();
            let effects = parsed_output.effects;
            ModelToolHookExecution {
                payload: json!({
                    "type": "command",
                    "command": command,
                    "hook_event": command_hook_event_name(event),
                    "tool_name": requested_tool_name,
                    "outcome": outcome,
                    "status": output.status,
                    "returncode": output.returncode,
                    "timed_out": output.timed_out,
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "timeout_seconds": timeout_seconds,
                    "default_timeout_seconds": CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS,
                    "stdin_protocol": "claude.hook_input.v1",
                    "hook_output_kind": hook_output_kind,
                    "hook_json_output": hook_json_output,
                    "hook_output_validation_error": hook_output_validation_error,
                    "permission_behavior": effects.permission_behavior,
                    "permission_decision_reason": effects.permission_decision_reason.clone(),
                    "updated_input": effects.updated_input.clone(),
                    "additional_context": effects.additional_context.clone(),
                    "updated_tool_output": effects.updated_tool_output.clone(),
                    "prevent_continuation": effects.prevent_continuation,
                    "stop_reason": effects.stop_reason.clone(),
                    "output_preview": output.output,
                    "output_truncated": output.output_truncated
                }),
                blocking_error,
                effects,
            }
        }
        Err(error) => ModelToolHookExecution {
            payload: json!({
                "type": "command",
                "command": command,
                "hook_event": command_hook_event_name(event),
                "tool_name": requested_tool_name,
                "outcome": "execution_error",
                "error": error.to_string()
            }),
            blocking_error: None,
            effects: ModelToolHookEffects::default(),
        },
    }
}

struct BackgroundCommandHookRequest<'a> {
    store: RunStore,
    repo_root: &'a str,
    cwd: &'a str,
    argv: Vec<String>,
    command: &'a str,
    event: HookEvent,
    requested_tool_name: &'a str,
    hook_input: &'a Value,
    timeout_seconds: u64,
    run_async: bool,
    async_rewake: bool,
}

fn background_command_model_tool_hook(
    request: BackgroundCommandHookRequest<'_>,
) -> ModelToolHookExecution {
    let BackgroundCommandHookRequest {
        store,
        repo_root,
        cwd,
        argv,
        command,
        event,
        requested_tool_name,
        hook_input,
        timeout_seconds,
        run_async,
        async_rewake,
    } = request;
    let repo_root = repo_root.to_owned();
    let cwd = cwd.to_owned();
    let run_id = command_hook_run_id(hook_input).map(RunId::from_string);
    let tool_use_id = hook_input
        .get("tool_use_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let agent_id = hook_input
        .get("agent_id")
        .or_else(|| hook_input.get("agentId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|agent_id| !agent_id.is_empty())
        .map(str::to_owned);
    let hook_event_name_value = command_hook_event_name(event).to_owned();
    let async_hook_id = format!("async_hook_{}", uuid::Uuid::new_v4());
    let hook_input = format!("{hook_input}\n");
    let task_timeout_seconds = timeout_seconds;
    let command_for_task = command.to_owned();
    let command_for_payload = command.to_owned();
    let requested_tool_name_for_task = requested_tool_name.to_owned();
    record_background_hook_event(
        &store,
        run_id.as_ref(),
        "model_tool.async_hook.started",
        json!({
            "contract": "coder.model_tool_async_hook.v1",
            "source": "coder-server",
            "async_hook_id": async_hook_id.clone(),
            "hook_event": hook_event_name_value.clone(),
            "tool_name": requested_tool_name,
            "tool_use_id": tool_use_id.clone(),
            "agent_id": agent_id.clone(),
            "agentId": agent_id.clone(),
            "command": command,
            "async": run_async,
            "async_rewake": async_rewake,
            "timeout_seconds": timeout_seconds,
            "default_timeout_seconds": CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS,
            "stdin_protocol": "claude.hook_input.v1"
        }),
    );
    let store_for_task = store.clone();
    let run_id_for_task = run_id.clone();
    let async_hook_id_for_task = async_hook_id.clone();
    let hook_event_name_for_task = hook_event_name_value.clone();
    let tool_use_id_for_task = tool_use_id.clone();
    let agent_id_for_task = agent_id.clone();
    std::mem::drop(tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let output = coder_tools::run_command(
            repo_root,
            coder_tools::CommandRunRequest {
                cwd: cwd.into(),
                argv,
                timeout_seconds: task_timeout_seconds,
                max_output_bytes: MODEL_TOOL_HOOK_OUTPUT_LIMIT_BYTES,
                source: "hook".to_owned(),
                sandbox: false,
                approved: true,
                stdin: Some(hook_input),
            },
        );
        match output {
            Ok(output) => {
                let outcome = if output.passed { "success" } else { "error" };
                let output_preview = output.output.clone();
                let async_response = if async_rewake {
                    None
                } else {
                    parse_async_hook_response_output(&output.output)
                };
                record_background_hook_event(
                    &store_for_task,
                    run_id_for_task.as_ref(),
                    "model_tool.async_hook.completed",
                    json!({
                        "contract": "coder.model_tool_async_hook.v1",
                        "source": "coder-server",
                        "async_hook_id": async_hook_id_for_task.clone(),
                        "hook_event": hook_event_name_for_task.clone(),
                        "tool_name": requested_tool_name_for_task.clone(),
                        "tool_use_id": tool_use_id_for_task.clone(),
                        "agent_id": agent_id_for_task.clone(),
                        "agentId": agent_id_for_task.clone(),
                        "command": command_for_task.clone(),
                        "outcome": outcome,
                        "status": output.status,
                        "returncode": output.returncode,
                        "timed_out": output.timed_out,
                        "duration_ms": started.elapsed().as_millis() as u64,
                        "timeout_seconds": task_timeout_seconds,
                        "output_preview": output_preview,
                        "output_truncated": output.output_truncated,
                        "rewake_requested": async_rewake,
                        "rewake_notification_recorded": async_rewake && output.returncode == Some(2),
                        "async_response_recorded": async_response.is_some()
                    }),
                );
                if let Some(async_response) = async_response {
                    record_background_hook_event(
                        &store_for_task,
                        run_id_for_task.as_ref(),
                        ASYNC_HOOK_RESPONSE_EVENT_KIND,
                        json!({
                            "contract": ASYNC_HOOK_RESPONSE_CONTRACT,
                            "source": "coder-server",
                            "async_hook_id": async_hook_id_for_task.clone(),
                            "processId": async_hook_id_for_task.clone(),
                            "process_id": async_hook_id_for_task.clone(),
                            "hookName": command_for_task.clone(),
                            "hookEvent": hook_event_name_for_task.clone(),
                            "toolName": requested_tool_name_for_task.clone(),
                            "hook_event": hook_event_name_for_task.clone(),
                            "tool_name": requested_tool_name_for_task.clone(),
                            "tool_use_id": tool_use_id_for_task.clone(),
                            "agent_id": agent_id_for_task.clone(),
                            "agentId": agent_id_for_task.clone(),
                            "command": command_for_task.clone(),
                            "response": async_response.response,
                            "stdout": output.output.clone(),
                            "stderr": "",
                            "exitCode": output.returncode,
                            "status": output.status,
                            "outcome": outcome,
                            "hook_output_kind": async_response.kind,
                            "output_channel": "coder_tools_merged_output",
                            "delivery_status": "recorded_not_delivered"
                        }),
                    );
                }
                if async_rewake && output.returncode == Some(2) {
                    record_background_hook_event(
                        &store_for_task,
                        run_id_for_task.as_ref(),
                        ASYNC_REWAKE_NOTIFICATION_EVENT_KIND,
                        json!({
                            "contract": "coder.model_tool_async_rewake.v1",
                            "source": "coder-server",
                            "async_hook_id": async_hook_id_for_task.clone(),
                            "mode": "task-notification",
                            "delivery_status": "recorded_not_delivered",
                            "priority": "later",
                            "agent_id": agent_id_for_task.clone(),
                            "agentId": agent_id_for_task.clone(),
                            "hook_event": hook_event_name_for_task.clone(),
                            "tool_name": requested_tool_name_for_task.clone(),
                            "tool_use_id": tool_use_id_for_task.clone(),
                            "command": command_for_task.clone(),
                            "message": format!(
                                "Stop hook blocking error from command \"{}\": {}",
                                command_for_task,
                                if output.output.trim().is_empty() {
                                    "No stderr output"
                                } else {
                                    output.output.trim()
                                }
                            )
                        }),
                    );
                }
            }
            Err(error) => {
                record_background_hook_event(
                    &store_for_task,
                    run_id_for_task.as_ref(),
                    "model_tool.async_hook.failed",
                    json!({
                        "contract": "coder.model_tool_async_hook.v1",
                        "source": "coder-server",
                        "async_hook_id": async_hook_id_for_task,
                        "hook_event": hook_event_name_for_task,
                        "tool_name": requested_tool_name_for_task,
                        "tool_use_id": tool_use_id_for_task,
                        "command": command_for_task,
                        "outcome": "execution_error",
                        "error": error.to_string(),
                        "duration_ms": started.elapsed().as_millis() as u64,
                        "timeout_seconds": task_timeout_seconds,
                        "rewake_requested": async_rewake
                    }),
                );
            }
        }
    }));

    ModelToolHookExecution {
        payload: json!({
            "type": "command",
            "command": command_for_payload,
            "hook_event": command_hook_event_name(event),
            "tool_name": requested_tool_name,
            "outcome": "backgrounded",
            "async_hook_id": async_hook_id,
            "async": run_async,
            "async_rewake": async_rewake,
            "rewake_supported": async_rewake,
            "rewake_delivery": if async_rewake { "recorded_on_exit_code_2_pending_model_turn_delivery" } else { "not_requested" },
            "stdin_protocol": "claude.hook_input.v1",
            "timeout_seconds": timeout_seconds,
            "default_timeout_seconds": CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS
        }),
        blocking_error: None,
        effects: ModelToolHookEffects::default(),
    }
}

fn record_background_hook_event(
    store: &RunStore,
    run_id: Option<&RunId>,
    kind: &'static str,
    payload: Value,
) {
    let Some(run_id) = run_id else {
        return;
    };
    let _ = append_model_tool_event(store, run_id, kind, payload);
}

fn command_hook_run_id(hook_input: &Value) -> Option<String> {
    hook_input
        .get("run_id")
        .or_else(|| hook_input.get("session_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
        .map(str::to_owned)
}

fn command_hook_event_name(event: HookEvent) -> &'static str {
    match event {
        HookEvent::PreToolUse => "PreToolUse",
        HookEvent::PostToolUse => "PostToolUse",
        HookEvent::PostToolUseFailure => "PostToolUseFailure",
    }
}

fn command_hook_kind(hook: &HookCommandSpec) -> &'static str {
    match hook {
        HookCommandSpec::Command { .. } => "command",
        HookCommandSpec::Prompt { .. } => "prompt",
        HookCommandSpec::Agent { .. } => "agent",
        HookCommandSpec::Webhook { .. } => "webhook",
    }
}

fn default_bash_hook_executable() -> String {
    if cfg!(windows) {
        windows_git_bash_path().unwrap_or_else(|| "bash".to_owned())
    } else {
        "bash".to_owned()
    }
}

fn windows_git_bash_path() -> Option<String> {
    if let Some(path) = env::var_os(CLAUDE_GIT_BASH_PATH_ENV)
        .and_then(|path| path.into_string().ok())
        .map(|path| path.trim().to_owned())
        .filter(|path| !path.is_empty())
        .filter(|path| path_exists(path))
    {
        return Some(path);
    }

    find_git_executable_on_windows()
        .and_then(|git_path| git_bash_path_from_git_executable(&git_path))
        .filter(|path| path_exists(path))
        .map(|path| path.display().to_string())
}

fn find_git_executable_on_windows() -> Option<PathBuf> {
    for path in [
        r"C:\Program Files\Git\cmd\git.exe",
        r"C:\Program Files (x86)\Git\cmd\git.exe",
    ] {
        let path = PathBuf::from(path);
        if path_exists(&path) {
            return Some(path);
        }
    }

    let cwd = env::current_dir()
        .ok()
        .and_then(|path| path.canonicalize().ok());
    let path_var = env::var_os("PATH")?;
    env::split_paths(&path_var)
        .map(|directory| directory.join("git.exe"))
        .find(|candidate| {
            if !path_exists(candidate) {
                return false;
            }
            let Some(cwd) = cwd.as_ref() else {
                return true;
            };
            let candidate_dir = candidate
                .parent()
                .unwrap_or(candidate)
                .canonicalize()
                .unwrap_or_else(|_| candidate.parent().unwrap_or(candidate).to_path_buf());
            candidate_dir != *cwd && !candidate_dir.starts_with(cwd)
        })
}

pub(crate) fn git_bash_path_from_git_executable(git_path: &Path) -> Option<PathBuf> {
    Some(git_path.parent()?.parent()?.join("bin").join("bash.exe"))
}

fn path_exists(path: impl AsRef<Path>) -> bool {
    path.as_ref().is_file()
}

#[cfg(test)]
pub(crate) fn windows_path_to_posix_path(windows_path: &str) -> String {
    if windows_path.starts_with(r"\\") {
        return windows_path.replace('\\', "/");
    }
    let bytes = windows_path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
    {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        return format!("/{drive}{}", windows_path[2..].replace('\\', "/"));
    }
    windows_path.replace('\\', "/")
}
