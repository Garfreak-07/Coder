use coder_config::{HookCommandSpec, HookEvent, ModelSpec};
use coder_core::RunId;
use coder_workflow::ModelToolHostContext;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use crate::model_tool_dispatch::execute_model_tool_request;
use crate::model_tool_hook_output::{bounded_hook_output_preview, ModelToolHookEffects};
use crate::model_tool_hook_phase::{hook_command_kind, hook_event_name, ModelToolHookContext};
use crate::model_tool_hook_runtime::ModelToolHookExecution;
use crate::model_tool_input::{apply_model_tool_request_context, canonical_model_tool_name};
use crate::model_tool_prompt_hooks::{
    prompt_hook_model_name, prompt_hook_model_spec, prompt_hook_prompt_with_arguments,
    PROMPT_HOOK_MAX_OUTPUT_TOKENS,
};
use crate::model_tool_response::{model_tool_error_response, model_tool_success_response};
use crate::model_tool_result_storage::maybe_persist_large_model_tool_result;
use crate::provider_runtime::{
    model_provider_base_url, model_provider_for_settings, provider_api_key,
    provider_chat_completions_endpoint, provider_http_client_builder, provider_proxy_url_for_url,
    redact_provider_error,
};
use crate::run_token_budget::{
    check_existing_run_token_budget, provider_token_usage, record_existing_run_token_usage,
};
use crate::{ApiError, ApiState, ModelToolExecuteRequest, ProviderSettings};

const CLAUDE_AGENT_HOOK_EXECUTION_TIMEOUT_SECONDS: u64 = 60;
const CLAUDE_AGENT_HOOK_MAX_TURNS: usize = 50;
const CLAUDE_SYNTHETIC_OUTPUT_TOOL_NAME: &str = "StructuredOutput";

struct AgentHookReportContext<'a> {
    prompt: &'a str,
    event: HookEvent,
    requested_tool_name: &'a str,
    timeout_seconds: u64,
    started: Instant,
    provider: &'a str,
    model_name: &'a str,
    model_source: &'static str,
    hook_agent_id: &'a str,
}

pub(crate) async fn execute_agent_model_tool_hook(
    state: &ApiState,
    hook: &HookCommandSpec,
    event: HookEvent,
    requested_tool_name: &str,
    hook_input: &Value,
    host_context: &ModelToolHostContext,
    context: &ModelToolHookContext,
) -> ModelToolHookExecution {
    let HookCommandSpec::Agent {
        prompt,
        timeout,
        model,
        ..
    } = hook
    else {
        return ModelToolHookExecution {
            payload: json!({
                "type": hook_command_kind(hook),
                "outcome": "unsupported"
            }),
            blocking_error: None,
            effects: ModelToolHookEffects::default(),
        };
    };
    let started = Instant::now();
    let timeout_seconds = timeout.unwrap_or(CLAUDE_AGENT_HOOK_EXECUTION_TIMEOUT_SECONDS);
    let processed_prompt = prompt_hook_prompt_with_arguments(prompt, hook_input);
    let (prompt_preview, prompt_truncated) = bounded_hook_output_preview(&processed_prompt);
    let hook_agent_id = format!("hook-agent-{}", uuid::Uuid::new_v4());
    let provider_settings = state.provider_settings.lock().unwrap().clone();
    let (model_spec, model_source) =
        agent_hook_model_spec(context, &provider_settings, model.as_deref());
    let provider = model_provider_for_settings(&provider_settings, &model_spec);
    let model_name = prompt_hook_model_name(&provider_settings, &model_spec);
    let report_context = AgentHookReportContext {
        prompt,
        event,
        requested_tool_name,
        timeout_seconds,
        started,
        provider: &provider,
        model_name: &model_name,
        model_source,
        hook_agent_id: &hook_agent_id,
    };
    let base_url = match model_provider_base_url(&provider_settings, &provider, &model_spec) {
        Some(base_url) => base_url,
        None => {
            return agent_hook_non_blocking_error(
                &report_context,
                "provider_base_url_missing",
                "Agent hook provider base URL is not configured.",
                None,
                0,
                0,
            )
            .with_processed_prompt_preview(prompt_preview, prompt_truncated);
        }
    };
    let api_key = match provider_api_key(
        &provider_settings,
        &provider,
        model_spec.api_key_env.as_deref(),
    ) {
        Some((api_key, _source)) => api_key,
        None => {
            return agent_hook_non_blocking_error(
                &report_context,
                "provider_api_key_missing",
                "Agent hook provider API key is not configured.",
                None,
                0,
                0,
            )
            .with_processed_prompt_preview(prompt_preview, prompt_truncated);
        }
    };
    if provider_settings.mock_mode {
        return agent_hook_non_blocking_error(
            &report_context,
            "provider_mock_mode",
            "Agent hook live evaluation is skipped while provider mock mode is enabled.",
            None,
            0,
            0,
        )
        .with_processed_prompt_preview(prompt_preview, prompt_truncated);
    }

    let url = provider_chat_completions_endpoint(&base_url);
    let proxy_url = provider_proxy_url_for_url(&provider_settings, &provider, Some(&url));
    let client =
        match provider_http_client_builder(&url, proxy_url.as_deref()).and_then(|builder| {
            builder
                .timeout(Duration::from_secs(timeout_seconds))
                .build()
                .map_err(|error| error.to_string())
        }) {
            Ok(client) => client,
            Err(error) => {
                let error = redact_provider_error(
                    &error,
                    &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
                );
                return agent_hook_non_blocking_error(
                    &report_context,
                    "client_build_error",
                    &error,
                    None,
                    0,
                    0,
                )
                .with_processed_prompt_preview(prompt_preview, prompt_truncated);
            }
        };

    let mut messages = vec![
        json!({
            "role": "system",
            "content": agent_hook_system_prompt(hook_input)
        }),
        json!({
            "role": "user",
            "content": processed_prompt
        }),
    ];
    let mut assistant_turns = 0usize;
    let mut tool_call_count = 0usize;
    let run_id = host_context.run_id.as_deref().map(RunId::from_string);

    loop {
        if run_id
            .as_ref()
            .and_then(|run_id| check_existing_run_token_budget(state, run_id))
            .is_some_and(|budget| budget.exhausted())
        {
            let blocking_error =
                "Agent hook was not evaluated because the workflow token budget was exhausted."
                    .to_owned();
            return ModelToolHookExecution {
                payload: json!({
                    "type": "agent",
                    "outcome": "blocking",
                    "error_kind": "workflow_token_budget_exhausted",
                    "blocking_error": blocking_error
                }),
                blocking_error: Some(blocking_error),
                effects: ModelToolHookEffects::default(),
            };
        }
        let Some(remaining_timeout) = remaining_agent_hook_timeout(started, timeout_seconds) else {
            return agent_hook_cancelled(
                &report_context,
                assistant_turns,
                tool_call_count,
                "timeout",
                "Agent hook timed out before producing structured output.",
            )
            .with_processed_prompt_preview(prompt_preview, prompt_truncated);
        };
        let request_body = agent_hook_completion_body(&provider, &model_name, &messages);
        let response = match client
            .post(&url)
            .bearer_auth(&api_key)
            .timeout(remaining_timeout)
            .json(&request_body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let is_timeout = error.is_timeout();
                let error_kind = if is_timeout {
                    "request_timeout"
                } else {
                    "request_error"
                };
                let raw_error = if is_timeout {
                    format!(
                        "agent hook model request timed out after {timeout_seconds} second(s): {error}"
                    )
                } else {
                    format!("agent hook model request failed: {error}")
                };
                let error = redact_provider_error(
                    &raw_error,
                    &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
                );
                return agent_hook_non_blocking_error(
                    &report_context,
                    error_kind,
                    &error,
                    None,
                    assistant_turns,
                    tool_call_count,
                )
                .with_processed_prompt_preview(prompt_preview, prompt_truncated);
            }
        };
        let status_code = response.status().as_u16();
        if !response.status().is_success() {
            return agent_hook_non_blocking_error(
                &report_context,
                "http_status_error",
                &format!("Agent hook provider returned HTTP {}.", response.status()),
                Some(status_code),
                assistant_turns,
                tool_call_count,
            )
            .with_processed_prompt_preview(prompt_preview, prompt_truncated);
        }
        let payload = match response.json::<Value>().await {
            Ok(payload) => payload,
            Err(error) => {
                let error = redact_provider_error(
                    &error.to_string(),
                    &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
                );
                return agent_hook_non_blocking_error(
                    &report_context,
                    "response_json_error",
                    &error,
                    Some(status_code),
                    assistant_turns,
                    tool_call_count,
                )
                .with_processed_prompt_preview(prompt_preview, prompt_truncated);
            }
        };
        if let Some(run_id) = run_id.as_ref() {
            record_existing_run_token_usage(
                state,
                run_id,
                provider_token_usage(&request_body, &payload),
            );
        }
        let message = agent_hook_first_message(&payload)
            .cloned()
            .unwrap_or_else(|| json!({ "role": "assistant", "content": "" }));
        assistant_turns = assistant_turns.saturating_add(1);
        let tool_calls = agent_hook_tool_calls(&message);
        if let Some(structured_output) = agent_hook_structured_output_from_tool_calls(&tool_calls) {
            return match structured_output {
                Ok(output) => agent_hook_structured_result(
                    &report_context,
                    assistant_turns,
                    tool_call_count.saturating_add(1),
                    output,
                )
                .with_processed_prompt_preview(prompt_preview, prompt_truncated),
                Err(error) => agent_hook_non_blocking_error(
                    &report_context,
                    "invalid_agent_hook_structured_output",
                    &error,
                    Some(status_code),
                    assistant_turns,
                    tool_call_count,
                )
                .with_processed_prompt_preview(prompt_preview, prompt_truncated),
            };
        }
        let content = agent_hook_message_content(&message);
        if let Some(structured_output) = agent_hook_structured_output_from_content(&content) {
            return match structured_output {
                Ok(output) => agent_hook_structured_result(
                    &report_context,
                    assistant_turns,
                    tool_call_count,
                    output,
                )
                .with_processed_prompt_preview(prompt_preview, prompt_truncated),
                Err(error) => agent_hook_non_blocking_error(
                    &report_context,
                    "invalid_agent_hook_content_json",
                    &error,
                    Some(status_code),
                    assistant_turns,
                    tool_call_count,
                )
                .with_processed_prompt_preview(prompt_preview, prompt_truncated),
            };
        }
        if assistant_turns >= CLAUDE_AGENT_HOOK_MAX_TURNS {
            return agent_hook_cancelled(
                &report_context,
                assistant_turns,
                tool_call_count,
                "max_turns",
                "Agent hook reached the maximum assistant turn count before producing structured output.",
            )
            .with_processed_prompt_preview(prompt_preview, prompt_truncated);
        }
        messages.push(agent_hook_assistant_message_for_history(&message));
        if tool_calls.is_empty() {
            messages.push(json!({
                "role": "user",
                "content": format!(
                    "You MUST call the {} tool to complete this request. Call this tool now.",
                    CLAUDE_SYNTHETIC_OUTPUT_TOOL_NAME
                )
            }));
            continue;
        }
        for tool_call in tool_calls {
            tool_call_count = tool_call_count.saturating_add(1);
            messages
                .push(execute_agent_hook_model_tool_call(state, &tool_call, host_context).await);
        }
    }
}

#[derive(Debug, Clone)]
struct AgentHookStructuredOutput {
    ok: bool,
    reason: String,
    raw: Value,
    output_kind: &'static str,
}

fn agent_hook_model_spec(
    context: &ModelToolHookContext,
    provider_settings: &ProviderSettings,
    hook_model: Option<&str>,
) -> (ModelSpec, &'static str) {
    if hook_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .is_some()
    {
        return prompt_hook_model_spec(context, provider_settings, hook_model);
    }
    if let Some(spec) = context.models.get("small_fast") {
        return (spec.clone(), "config_small_fast_model");
    }
    prompt_hook_model_spec(context, provider_settings, None)
}

fn agent_hook_system_prompt(hook_input: &Value) -> String {
    let transcript_path = hook_input
        .get("transcript_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let cwd = hook_input.get("cwd").and_then(Value::as_str).unwrap_or(".");
    format!(
        "You are verifying a hook condition in Claude Code. The hook input JSON names the tool, arguments, session, and working directory.\n\
Use the available read-only tools to inspect the codebase when needed. Be efficient and direct.\n\
Current working directory: {cwd}\n\
Conversation transcript path, if available: {transcript_path}\n\n\
When done, return your result using the {CLAUDE_SYNTHETIC_OUTPUT_TOOL_NAME} tool with:\n\
- ok: true if the condition is met\n\
- ok: false with reason if the condition is not met"
    )
}

fn remaining_agent_hook_timeout(started: Instant, timeout_seconds: u64) -> Option<Duration> {
    Duration::from_secs(timeout_seconds)
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
}

fn agent_hook_completion_body(provider: &str, model_name: &str, messages: &[Value]) -> Value {
    let mut body = json!({
        "model": model_name,
        "messages": messages,
        "tools": agent_hook_openai_tools(),
        "tool_choice": "auto",
        "temperature": 0,
        "max_tokens": PROMPT_HOOK_MAX_OUTPUT_TOKENS
    });
    if provider == "deepseek" {
        body["thinking"] = json!({"type": "disabled"});
    }
    body
}

fn agent_hook_openai_tools() -> Vec<Value> {
    vec![
        agent_hook_openai_tool(
            "repo_find_files",
            "Find files under a repository root.",
            json!({
                "type": "object",
                "properties": {
                    "repo_root": { "type": "string" },
                    "query": { "type": "string" },
                    "extensions": { "type": "array", "items": { "type": "string" } },
                    "max_results": { "type": "integer", "minimum": 1 },
                    "run_id": { "type": "string" }
                },
                "required": ["repo_root"],
                "additionalProperties": false
            }),
        ),
        agent_hook_openai_tool(
            "repo_search_text",
            "Search text under a repository root.",
            json!({
                "type": "object",
                "properties": {
                    "repo_root": { "type": "string" },
                    "query": { "type": "string" },
                    "max_file_bytes": { "type": "integer", "minimum": 1 },
                    "max_matches": { "type": "integer", "minimum": 1 },
                    "run_id": { "type": "string" }
                },
                "required": ["repo_root", "query"],
                "additionalProperties": false
            }),
        ),
        agent_hook_openai_tool(
            "repo_read_file",
            "Read one file under a repository root.",
            json!({
                "type": "object",
                "properties": {
                    "repo_root": { "type": "string" },
                    "path": { "type": "string" },
                    "max_file_bytes": { "type": "integer", "minimum": 1 },
                    "run_id": { "type": "string" }
                },
                "required": ["repo_root", "path"],
                "additionalProperties": false
            }),
        ),
        agent_hook_openai_tool(
            "repo_read_file_range",
            "Read a line range from one file under a repository root.",
            json!({
                "type": "object",
                "properties": {
                    "repo_root": { "type": "string" },
                    "path": { "type": "string" },
                    "start_line": { "type": "integer", "minimum": 1 },
                    "max_lines": { "type": "integer", "minimum": 1 },
                    "max_chars": { "type": "integer", "minimum": 1 },
                    "run_id": { "type": "string" }
                },
                "required": ["repo_root", "path"],
                "additionalProperties": false
            }),
        ),
        agent_hook_openai_tool(
            "git_status",
            "Read git status for a repository root.",
            json!({
                "type": "object",
                "properties": {
                    "repo_root": { "type": "string" },
                    "run_id": { "type": "string" }
                },
                "required": ["repo_root"],
                "additionalProperties": false
            }),
        ),
        agent_hook_openai_tool(
            "git_diff",
            "Read git diff for a repository root.",
            json!({
                "type": "object",
                "properties": {
                    "repo_root": { "type": "string" },
                    "max_output_bytes": { "type": "integer", "minimum": 1 },
                    "run_id": { "type": "string" }
                },
                "required": ["repo_root"],
                "additionalProperties": false
            }),
        ),
        agent_hook_openai_tool(
            "sleep",
            "Wait briefly before checking for queued notifications.",
            json!({
                "type": "object",
                "properties": {
                    "duration_ms": { "type": "integer", "minimum": 0, "maximum": 120000 },
                    "seconds": { "type": "number", "minimum": 0, "maximum": 120 }
                },
                "additionalProperties": false
            }),
        ),
        agent_hook_openai_tool(
            CLAUDE_SYNTHETIC_OUTPUT_TOOL_NAME,
            "Use this tool to return your verification result. You MUST call this tool exactly once at the end of your response.",
            agent_hook_structured_output_schema(),
        ),
    ]
}

fn agent_hook_openai_tool(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters
        }
    })
}

fn agent_hook_structured_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ok": {
                "type": "boolean",
                "description": "Whether the condition was met"
            },
            "reason": {
                "type": "string",
                "description": "Reason, if the condition was not met"
            }
        },
        "required": ["ok"],
        "additionalProperties": false
    })
}

fn agent_hook_first_message(payload: &Value) -> Option<&Value> {
    payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
}

fn agent_hook_tool_calls(message: &Value) -> Vec<Value> {
    message
        .get("tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn agent_hook_message_content(message: &Value) -> String {
    let Some(content) = message.get("content") else {
        return String::new();
    };
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| item.as_str())
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn agent_hook_structured_output_from_tool_calls(
    tool_calls: &[Value],
) -> Option<Result<AgentHookStructuredOutput, String>> {
    for tool_call in tool_calls {
        let name = agent_hook_tool_call_name(tool_call).unwrap_or_default();
        if agent_hook_is_structured_output_tool(&name) {
            let input = agent_hook_tool_call_input(tool_call);
            return Some(agent_hook_structured_output_from_value(
                input,
                "agent_structured_output_tool",
            ));
        }
    }
    None
}

fn agent_hook_structured_output_from_content(
    content: &str,
) -> Option<Result<AgentHookStructuredOutput, String>> {
    let trimmed = content.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    Some(
        serde_json::from_str::<Value>(trimmed)
            .map_err(|error| error.to_string())
            .and_then(|value| {
                agent_hook_structured_output_from_value(value, "agent_content_json_fallback")
            }),
    )
}

fn agent_hook_structured_output_from_value(
    value: Value,
    output_kind: &'static str,
) -> Result<AgentHookStructuredOutput, String> {
    let Some(ok) = value.get("ok").and_then(Value::as_bool) else {
        return Err("Agent hook structured output must include boolean field 'ok'.".to_owned());
    };
    let reason = value
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    Ok(AgentHookStructuredOutput {
        ok,
        reason,
        raw: value,
        output_kind,
    })
}

fn agent_hook_tool_call_id(tool_call: &Value) -> String {
    tool_call
        .get("id")
        .or_else(|| tool_call.get("tool_call_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("tool_call_{}", uuid::Uuid::new_v4()))
}

fn agent_hook_tool_call_name(tool_call: &Value) -> Option<String> {
    tool_call
        .get("function")
        .and_then(|function| function.get("name"))
        .or_else(|| tool_call.get("name"))
        .or_else(|| tool_call.get("tool_name"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn agent_hook_tool_call_input(tool_call: &Value) -> Value {
    let arguments = tool_call
        .get("function")
        .and_then(|function| function.get("arguments"))
        .or_else(|| tool_call.get("arguments"))
        .or_else(|| tool_call.get("input"));
    match arguments {
        Some(Value::String(text)) => serde_json::from_str::<Value>(text).unwrap_or_else(|_| {
            json!({
                "raw_arguments": text
            })
        }),
        Some(value) => value.clone(),
        None => json!({}),
    }
}

fn agent_hook_is_structured_output_tool(name: &str) -> bool {
    let normalized = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(normalized.as_str(), "structuredoutput" | "syntheticoutput")
}

fn agent_hook_assistant_message_for_history(message: &Value) -> Value {
    let mut message = message.clone();
    if let Value::Object(object) = &mut message {
        object
            .entry("role".to_owned())
            .or_insert_with(|| Value::String("assistant".to_owned()));
        return message;
    }
    json!({
        "role": "assistant",
        "content": ""
    })
}

async fn execute_agent_hook_model_tool_call(
    state: &ApiState,
    tool_call: &Value,
    host_context: &ModelToolHostContext,
) -> Value {
    let tool_call_id = agent_hook_tool_call_id(tool_call);
    let tool_name = agent_hook_tool_call_name(tool_call).unwrap_or_else(|| "unknown".to_owned());
    let canonical_tool_name = canonical_model_tool_name(&tool_name);
    if !agent_hook_model_tool_allowed(canonical_tool_name) {
        return json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "name": tool_name,
            "content": format!(
                "<tool_use_error>Tool '{}' is not available to hook agents.</tool_use_error>",
                tool_name
            )
        });
    }
    let mut request = ModelToolExecuteRequest {
        tool_use_id: tool_call_id.clone(),
        tool_name: tool_name.clone(),
        run_id: host_context.run_id.clone(),
        harness_id: host_context.harness_id.clone(),
        agent_id: host_context.agent_id.clone(),
        current_model: host_context.current_model.clone(),
        current_effort: host_context.current_effort.clone(),
        skill_context_modifiers: host_context.skill_context_modifiers.clone(),
        input: agent_hook_tool_call_input(tool_call),
    };
    apply_model_tool_request_context(&mut request);
    let response = match execute_model_tool_request(state.clone(), request, host_context).await {
        Ok(payload) => {
            model_tool_success_response(tool_call_id.clone(), tool_name.clone(), payload)
        }
        Err(error) => model_tool_error_response(tool_call_id.clone(), tool_name.clone(), error),
    };
    let mut response = response;
    if let Err(error) = maybe_persist_large_model_tool_result(&state.store, &mut response) {
        response = model_tool_error_response(
            response.tool_use_id,
            response.tool_name,
            ApiError::internal(error.to_string()),
        );
    }
    json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "name": tool_name,
        "content": response.content
    })
}

fn agent_hook_model_tool_allowed(canonical_tool_name: &str) -> bool {
    matches!(
        canonical_tool_name,
        "repo_find_files"
            | "repo_search_text"
            | "repo_read_file"
            | "repo_read_file_range"
            | "git_status"
            | "git_diff"
            | "skill"
            | "sleep"
    )
}

fn agent_hook_structured_result(
    context: &AgentHookReportContext<'_>,
    assistant_turns: usize,
    tool_call_count: usize,
    output: AgentHookStructuredOutput,
) -> ModelToolHookExecution {
    let blocking_error = if output.ok {
        None
    } else {
        Some(format!(
            "Agent hook condition was not met: {}",
            if output.reason.is_empty() {
                "No reason provided"
            } else {
                output.reason.as_str()
            }
        ))
    };
    ModelToolHookExecution {
        payload: json!({
            "type": "agent",
            "prompt": context.prompt,
            "hook_event": hook_event_name(context.event),
            "tool_name": context.requested_tool_name,
            "outcome": if blocking_error.is_some() { "blocking" } else { "success" },
            "provider": context.provider,
            "model": context.model_name,
            "model_source": context.model_source,
            "duration_ms": context.started.elapsed().as_millis() as u64,
            "timeout_seconds": context.timeout_seconds,
            "default_timeout_seconds": CLAUDE_AGENT_HOOK_EXECUTION_TIMEOUT_SECONDS,
            "max_agent_turns": CLAUDE_AGENT_HOOK_MAX_TURNS,
            "assistant_turns": assistant_turns,
            "tool_call_count": tool_call_count,
            "hook_agent_id": context.hook_agent_id,
            "request_protocol": "claude.hook_input.v1",
            "prompt_argument_protocol": "claude.addArgumentsToPrompt",
            "structured_output_tool": CLAUDE_SYNTHETIC_OUTPUT_TOOL_NAME,
            "hook_output_kind": output.output_kind,
            "hook_json_output": output.raw,
            "blocking_error": blocking_error.clone(),
            "runtime_contract": agent_hook_runtime_contract(),
            "available_tools_policy": agent_hook_available_tools_policy(),
            "claude_sources": agent_hook_claude_sources()
        }),
        blocking_error,
        effects: ModelToolHookEffects::default(),
    }
}

fn agent_hook_non_blocking_error(
    context: &AgentHookReportContext<'_>,
    error_kind: &'static str,
    error: &str,
    status_code: Option<u16>,
    assistant_turns: usize,
    tool_call_count: usize,
) -> ModelToolHookExecution {
    let mut payload = json!({
        "type": "agent",
        "prompt": context.prompt,
        "hook_event": hook_event_name(context.event),
        "tool_name": context.requested_tool_name,
        "outcome": "non_blocking_error",
        "provider": context.provider,
        "model": context.model_name,
        "model_source": context.model_source,
        "duration_ms": context.started.elapsed().as_millis() as u64,
        "timeout_seconds": context.timeout_seconds,
        "default_timeout_seconds": CLAUDE_AGENT_HOOK_EXECUTION_TIMEOUT_SECONDS,
        "max_agent_turns": CLAUDE_AGENT_HOOK_MAX_TURNS,
        "assistant_turns": assistant_turns,
        "tool_call_count": tool_call_count,
        "structured_output_tool": CLAUDE_SYNTHETIC_OUTPUT_TOOL_NAME,
        "hook_output_kind": error_kind,
        "hook_json_output": Value::Null,
        "hook_output_validation_error": error,
        "runtime_contract": agent_hook_runtime_contract(),
        "available_tools_policy": agent_hook_available_tools_policy(),
        "claude_sources": agent_hook_claude_sources()
    });
    if let Some(status_code) = status_code {
        if let Value::Object(payload) = &mut payload {
            payload.insert("status_code".to_owned(), json!(status_code));
        }
    }
    ModelToolHookExecution {
        payload,
        blocking_error: None,
        effects: ModelToolHookEffects::default(),
    }
}

fn agent_hook_cancelled(
    context: &AgentHookReportContext<'_>,
    assistant_turns: usize,
    tool_call_count: usize,
    reason: &'static str,
    detail: &str,
) -> ModelToolHookExecution {
    ModelToolHookExecution {
        payload: json!({
            "type": "agent",
            "prompt": context.prompt,
            "hook_event": hook_event_name(context.event),
            "tool_name": context.requested_tool_name,
            "outcome": "cancelled",
            "reason": reason,
            "detail": detail,
            "provider": context.provider,
            "model": context.model_name,
            "model_source": context.model_source,
            "duration_ms": context.started.elapsed().as_millis() as u64,
            "timeout_seconds": context.timeout_seconds,
            "default_timeout_seconds": CLAUDE_AGENT_HOOK_EXECUTION_TIMEOUT_SECONDS,
            "max_agent_turns": CLAUDE_AGENT_HOOK_MAX_TURNS,
            "assistant_turns": assistant_turns,
            "tool_call_count": tool_call_count,
            "hook_agent_id": context.hook_agent_id,
            "structured_output_tool": CLAUDE_SYNTHETIC_OUTPUT_TOOL_NAME,
            "runtime_contract": agent_hook_runtime_contract(),
            "available_tools_policy": agent_hook_available_tools_policy(),
            "claude_sources": agent_hook_claude_sources()
        }),
        blocking_error: None,
        effects: ModelToolHookEffects::default(),
    }
}

fn agent_hook_runtime_contract() -> Value {
    json!({
        "isolated_agent_id_prefix": "hook-agent-",
        "structured_output_schema": agent_hook_structured_output_schema(),
        "thinking": "disabled",
        "non_interactive": true,
        "max_agent_turns": CLAUDE_AGENT_HOOK_MAX_TURNS,
        "must_filter_recursive_agent_tools": true
    })
}

fn agent_hook_available_tools_policy() -> Value {
    json!({
        "mode": "read_only_minimal",
        "allowed_model_tools": [
            "repo_find_files",
            "repo_search_text",
            "repo_read_file",
            "repo_read_file_range",
            "git_status",
            "git_diff",
            "skill",
            "sleep",
            CLAUDE_SYNTHETIC_OUTPUT_TOOL_NAME
        ],
        "filtered_tool_families": [
            "agent_subagent",
            "command_run",
            "command_background",
            "patch_preview",
            "patch_apply",
            "cancel_background"
        ],
        "claude_filter_reference": "ALL_AGENT_DISALLOWED_TOOLS"
    })
}

fn agent_hook_claude_sources() -> Vec<&'static str> {
    vec![
        "src/utils/hooks/execAgentHook.ts",
        "src/utils/hooks/hookHelpers.ts createStructuredOutputTool",
        "src/utils/hooks/hookHelpers.ts registerStructuredOutputEnforcement",
        "src/constants/tools.ts ALL_AGENT_DISALLOWED_TOOLS",
        "src/utils/agentToolFilter.ts filterParentToolsForFork",
    ]
}
