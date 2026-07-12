use std::{collections::BTreeSet, sync::Arc, time::Duration};

use async_trait::async_trait;
use coder_config::ResolvedAgentRuntimePolicy;
use coder_core::{EvidenceRef, FinalReport, RunId};
use coder_harness::{
    HarnessBackend, HarnessError, HarnessRunEvent, HarnessRunEventRef, HarnessRunResult,
};
use coder_tools::builtin_tools;
use coder_workflow::{
    execute_model_tool_turn, DeterministicNativeBackend, ModelToolLoopOptions,
    ModelToolResultBlock, ModelToolUseBlock, TurnContext,
};
use reqwest::Client;
use serde_json::{json, Value};

use crate::model_tool_async_attachments::{
    drain_async_hook_response_attachments, drain_async_rewake_notification_attachments,
    drain_planner_user_guidance_attachments,
};
use crate::model_tool_input::canonical_model_tool_name;
use crate::model_tool_server_executor::server_model_tool_executor_with_mcp;
use crate::native_model_mcp::{
    native_model_mcp_routes, snapshot_native_model_mcp_tools, NativeModelMcpTool,
};
use crate::provider_runtime::{
    harness_agent_runtime, harness_model_spec, model_name_for_settings, model_provider_base_url,
    model_provider_for_settings, normalize_provider, provider_api_key,
    provider_chat_completions_endpoint, provider_http_client_builder, provider_proxy_url_for_url,
    provider_reasoning_effort, provider_request_max_retries, redact_provider_error,
    send_provider_request_with_retry,
};
use crate::run_token_budget::{
    check_run_token_budget, provider_token_usage, record_run_token_usage, RunTokenUsage,
};
use crate::ApiState;

const NATIVE_MODEL_TOOL_RESULT_MAX_CHARS: usize = 24_000;
const NATIVE_MODEL_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
const NATIVE_READ_ONLY_MAX_TURNS: usize = 8;

#[derive(Debug, Clone)]
pub(crate) struct NativeModelBackend {
    state: ApiState,
    mock_backend: Arc<DeterministicNativeBackend>,
}

impl NativeModelBackend {
    pub(crate) fn new(state: ApiState) -> Self {
        Self {
            mock_backend: Arc::new(DeterministicNativeBackend::new(state.store.clone())),
            state,
        }
    }
}

#[async_trait]
impl HarnessBackend for NativeModelBackend {
    async fn run(
        &self,
        request: coder_harness::HarnessRunRequest,
    ) -> Result<HarnessRunResult, HarnessError> {
        let settings = self.state.provider_settings.lock().unwrap().clone();
        if settings.mock_mode {
            return self.mock_backend.run(request).await;
        }

        let started = HarnessRunEvent::new(
            "backend.native_rust.started",
            json!({
                "backend": "native-rust",
                "implementation": "native-model-tool-loop",
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "model_driven": true,
                "side_effect_boundary": "structured_tools"
            }),
        );
        if !native_model_agent_can_execute(&request) {
            return Ok(blocked_result(
                started,
                "Native model execution requires an Executor or subagent role.",
                "native_model_role_not_executable",
            ));
        }
        if !start_work_authorized(&request) {
            return Ok(blocked_result(
                started,
                "Native model execution requires Start Work authorization.",
                "missing_start_work_approval",
            ));
        }

        let model = harness_model_spec(&request);
        let runtime = harness_agent_runtime(&request);
        if !runtime.supports_tool_calls {
            return Ok(blocked_result(
                started,
                "The selected model does not support tool calls required by the native executor.",
                "model_tool_calls_unsupported",
            ));
        }
        let provider = model_provider_for_settings(&settings, &model);
        let model_name = model_name_for_settings(&settings, &model);
        let Some((api_key, credential_source)) =
            provider_api_key(&settings, &provider, model.api_key_env.as_deref())
        else {
            return Ok(blocked_result(
                started,
                "Native model execution requires configured provider credentials.",
                "missing_provider_credentials",
            ));
        };
        let Some(base_url) = model_provider_base_url(&settings, &provider, &model) else {
            return Ok(blocked_result(
                started,
                "Native model executor needs a provider base URL before it can generate code.",
                "missing_provider_base_url",
            ));
        };
        let url = provider_chat_completions_endpoint(&base_url);
        let proxy_url = provider_proxy_url_for_url(&settings, &provider, Some(&url));
        let client = provider_http_client_builder(&settings, &provider, &url)
            .map_err(|error| {
                HarnessError::Failed(redact_provider_error(
                    &error,
                    &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
                ))
            })?
            .timeout(NATIVE_MODEL_REQUEST_TIMEOUT)
            .build()
            .map_err(|error| {
                HarnessError::Failed(redact_provider_error(
                    &error.to_string(),
                    &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
                ))
            })?;

        let outcome = run_native_model_provider(NativeModelProviderContext {
            state: &self.state,
            client: &client,
            url: &url,
            api_key: &api_key,
            provider: &provider,
            model: &model_name,
            request: &request,
            max_output_tokens: runtime.max_output_tokens,
            request_max_retries: provider_request_max_retries(&settings, &provider),
            runtime,
        })
        .await
        .map_err(|error| {
            HarnessError::Failed(redact_provider_error(
                &error,
                &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
            ))
        })?;
        tool_loop_result(
            started,
            credential_source,
            provider,
            model_name,
            outcome,
            &self.state,
            &request,
        )
        .map_err(|error| HarnessError::Failed(error.to_string()))
    }
}

#[derive(Debug, Default)]
struct NativeModelToolLoopOutcome {
    status: String,
    summary: String,
    checks: Vec<String>,
    blockers: Vec<String>,
    changed_files: Vec<String>,
    evidence_refs: Vec<EvidenceRef>,
    events: Vec<HarnessRunEvent>,
    tool_call_count: usize,
}

#[derive(Debug)]
struct NativeModelToolCall {
    id: String,
    name: String,
    arguments: Result<Value, String>,
}

#[derive(Debug)]
struct NativeModelToolCallResult {
    tool_call_id: String,
    tool_name: String,
    status: String,
    is_error: bool,
    content: String,
    refs: Vec<HarnessRunEventRef>,
}

enum PreparedNativeModelToolCall {
    Shared {
        tool_call_id: String,
        canonical_tool_name: String,
    },
    Synthetic(NativeModelToolCallResult),
}

struct NativeModelProviderContext<'a> {
    state: &'a ApiState,
    client: &'a Client,
    url: &'a str,
    api_key: &'a str,
    provider: &'a str,
    model: &'a str,
    request: &'a coder_harness::HarnessRunRequest,
    max_output_tokens: u32,
    request_max_retries: u64,
    runtime: ResolvedAgentRuntimePolicy,
}

async fn run_native_model_provider(
    context: NativeModelProviderContext<'_>,
) -> Result<NativeModelToolLoopOutcome, String> {
    let NativeModelProviderContext {
        state,
        client,
        url,
        api_key,
        provider,
        model,
        request,
        max_output_tokens,
        request_max_retries,
        runtime,
    } = context;
    let mcp_tools = if native_model_is_read_only(request) {
        Vec::new()
    } else {
        snapshot_native_model_mcp_tools(state.mcp_runtime.list_tools().await)
    };
    let mcp_routes = native_model_mcp_routes(&mcp_tools);
    let mut messages = native_model_initial_messages(request);
    let mut outcome = NativeModelToolLoopOutcome {
        status: "completed".to_owned(),
        ..NativeModelToolLoopOutcome::default()
    };
    let max_output_recovery_attempts = runtime.max_output_recovery_attempts;
    let mut output_recovery_attempts = 0_u8;

    let max_turns = native_model_max_turns(request, &runtime);
    let mut turn = 0usize;
    loop {
        if turn >= max_turns {
            break;
        }
        turn = turn.saturating_add(1);
        if let Some(budget) = check_run_token_budget(state, request) {
            if budget.exhausted() {
                outcome.status = "blocked".to_owned();
                outcome
                    .blockers
                    .push("workflow token budget was exhausted".to_owned());
                outcome.events.push(HarnessRunEvent::new(
                    "model.token_budget.exhausted",
                    json!({
                        "contract": "coder.run_token_budget.v1",
                        "run_id": request.run_id,
                        "budget": budget.as_json(),
                        "next_turn": turn
                    }),
                ));
                return Ok(outcome);
            }
        }
        let pending_attachments = drain_native_model_async_attachments(state, request);
        append_native_model_attachment_messages(
            &mut messages,
            pending_attachments,
            &mut outcome,
            turn,
            "before_provider_request",
        );
        let body = native_model_chat_completion_body(
            provider,
            model,
            messages.clone(),
            max_output_tokens,
            request,
            runtime.supports_parallel_tool_calls,
            &mcp_tools,
        );
        let (payload, request_attempts) =
            send_native_chat_completion(client, url, api_key, &body, request_max_retries).await?;
        let mut usage_event = native_model_provider_usage_event(
            provider,
            model,
            turn,
            &body,
            &payload,
            request_attempts,
        );
        attach_run_token_budget(state, request, &mut usage_event);
        outcome.events.push(usage_event);
        let message = native_assistant_message(&payload)?;
        let content = native_assistant_message_content(&message);
        let output_limit_hit = native_model_output_limit_hit(&payload);
        let tool_calls = native_model_tool_calls(&message, output_limit_hit);
        if tool_calls.is_empty() {
            if output_limit_hit {
                if output_recovery_attempts < max_output_recovery_attempts {
                    output_recovery_attempts += 1;
                    messages.push(native_assistant_tool_history_message(&message));
                    messages.push(native_model_output_recovery_message(
                        output_recovery_attempts,
                        max_output_recovery_attempts,
                    ));
                    outcome.events.push(native_model_output_recovery_event(
                        output_recovery_attempts,
                        max_output_recovery_attempts,
                        max_output_tokens,
                    ));
                    continue;
                }
                outcome.status = "blocked".to_owned();
                outcome
                    .blockers
                    .push("native model output recovery attempts were exhausted".to_owned());
                return Ok(outcome);
            }
            let pending_attachments = drain_native_model_async_attachments(state, request);
            if !pending_attachments.is_empty() {
                messages.push(native_assistant_tool_history_message(&message));
                append_native_model_attachment_messages(
                    &mut messages,
                    pending_attachments,
                    &mut outcome,
                    turn,
                    "before_final_response",
                );
                continue;
            }
            apply_native_model_final_content(&mut outcome, content.as_deref());
            return Ok(outcome);
        }

        outcome.events.push(HarnessRunEvent::new(
            "model.tool_turn.started",
            json!({
                "backend": "native-rust",
                "implementation": "native-model-tool-loop",
                "execution_mode": "tool_loop",
                "turn": turn,
                "tool_call_count": tool_calls.len()
            }),
        ));
        messages.push(native_assistant_tool_history_message(&message));
        let (prepared_calls, tool_uses) =
            prepare_native_model_tool_turn(request, tool_calls, &mcp_tools);
        outcome.tool_call_count = outcome.tool_call_count.saturating_add(prepared_calls.len());
        let options = if runtime.supports_parallel_tool_calls {
            ModelToolLoopOptions::default()
        } else {
            ModelToolLoopOptions::with_max_tool_use_concurrency(1)
        };
        let turn_output = execute_model_tool_turn(
            tool_uses,
            server_model_tool_executor_with_mcp(state.clone(), mcp_routes.clone()),
            options.with_turn_context(native_model_turn_context(request, &mcp_tools)),
        )
        .await;
        let mut shared_results = turn_output
            .results
            .into_iter()
            .map(|result| (result.tool_use_id.clone(), result))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut finish_requested = false;
        for prepared in prepared_calls {
            let result = match prepared {
                PreparedNativeModelToolCall::Shared {
                    tool_call_id,
                    canonical_tool_name,
                } => {
                    let result = shared_results.remove(&tool_call_id).unwrap_or_else(|| {
                        synthetic_missing_native_tool_result(&tool_call_id, &canonical_tool_name)
                    });
                    absorb_native_model_shared_tool_report(
                        &mut outcome,
                        &canonical_tool_name,
                        &result.payload,
                    );
                    native_model_tool_call_result_from_shared(result, canonical_tool_name)
                }
                PreparedNativeModelToolCall::Synthetic(result) => result,
            };
            finish_requested |= result.tool_name == "finish" && !result.is_error;
            let mut event = HarnessRunEvent::new(
                "model.tool_call.completed",
                json!({
                    "backend": "native-rust",
                    "implementation": "native-model-tool-loop",
                    "execution_mode": "tool_loop",
                    "tool_call_id": &result.tool_call_id,
                    "tool_name": &result.tool_name,
                    "status": &result.status,
                    "is_error": result.is_error,
                    "summary": &result.content
                }),
            );
            for reference in &result.refs {
                event = event.with_ref(reference.label.clone(), reference.uri.clone());
            }
            outcome
                .evidence_refs
                .extend(evidence_refs_from_event_refs(&result.refs));
            outcome.events.push(event);
            messages.push(json!({
                "role": "tool",
                "tool_call_id": result.tool_call_id,
                "content": result.content
            }));
        }
        append_native_model_attachment_messages(
            &mut messages,
            turn_output.attachments,
            &mut outcome,
            turn,
            "after_tool_turn",
        );
        if output_limit_hit {
            if output_recovery_attempts < max_output_recovery_attempts {
                output_recovery_attempts += 1;
                messages.push(native_model_output_recovery_message(
                    output_recovery_attempts,
                    max_output_recovery_attempts,
                ));
                outcome.events.push(native_model_output_recovery_event(
                    output_recovery_attempts,
                    max_output_recovery_attempts,
                    max_output_tokens,
                ));
            } else {
                outcome.status = "blocked".to_owned();
                outcome
                    .blockers
                    .push("native model output token recovery attempts were exhausted".to_owned());
                return Ok(outcome);
            }
        }
        if finish_requested {
            return Ok(outcome);
        }
    }

    if outcome.changed_files.is_empty() {
        outcome.status = "blocked".to_owned();
        outcome
            .blockers
            .push("native model tool loop reached its turn limit".to_owned());
    } else {
        outcome.status = "completed".to_owned();
        if outcome.summary.trim().is_empty() {
            outcome.summary = format!(
                "Native model tool loop wrote {} file(s) and stopped after the tool turn limit without a final response.",
                outcome.changed_files.len()
            );
        }
        outcome
            .checks
            .push("native_model_tool_loop: stopped_after_turn_limit_with_file_writes".to_owned());
    }
    Ok(outcome)
}

async fn send_native_chat_completion(
    client: &Client,
    url: &str,
    api_key: &str,
    body: &Value,
    request_max_retries: u64,
) -> Result<(Value, u32), String> {
    let outcome = send_provider_request_with_retry(
        || client.post(url).bearer_auth(api_key).json(body),
        None,
        request_max_retries,
    )
    .await
    .map_err(|error| format!("native model request failed: {error}"))?;
    let request_attempts = outcome.attempts;
    let response = outcome.response;
    if !response.status().is_success() {
        return Err(format!("native model returned HTTP {}", response.status()));
    }
    let payload = response
        .json()
        .await
        .map_err(|error| format!("native model response was not JSON: {error}"))?;
    Ok((payload, request_attempts))
}

fn native_model_provider_usage_event(
    provider: &str,
    model: &str,
    turn: usize,
    request_body: &Value,
    response_payload: &Value,
    request_attempts: u32,
) -> HarnessRunEvent {
    let serialized_request = request_body.to_string();
    let usage = response_payload.get("usage").unwrap_or(&Value::Null);
    let token_usage = provider_token_usage(request_body, response_payload);
    let total_tokens = provider_usage_u64(usage, &["total_tokens"]);
    HarnessRunEvent::new(
        "model.provider_turn.completed",
        json!({
            "backend": "native-rust",
            "provider": provider,
            "model": model,
            "turn": turn,
            "request_attempts": request_attempts,
            "request_chars": serialized_request.chars().count(),
            "estimated_input_tokens": token_usage.estimated_input_tokens,
            "estimated_output_tokens": token_usage.estimated_output_tokens,
            "input_tokens": token_usage.input_tokens,
            "output_tokens": token_usage.output_tokens,
            "total_tokens": total_tokens,
            "cache_read_tokens": token_usage.cache_read_tokens,
            "usage_reported": !usage.is_null()
        }),
    )
}

fn attach_run_token_budget(
    state: &ApiState,
    request: &coder_harness::HarnessRunRequest,
    event: &mut HarnessRunEvent,
) {
    let usage = RunTokenUsage {
        input_tokens: event.payload.get("input_tokens").and_then(Value::as_u64),
        output_tokens: event.payload.get("output_tokens").and_then(Value::as_u64),
        cache_read_tokens: event
            .payload
            .get("cache_read_tokens")
            .and_then(Value::as_u64),
        estimated_input_tokens: event
            .payload
            .get("estimated_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        estimated_output_tokens: event
            .payload
            .get("estimated_output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    };
    if let Some(snapshot) = record_run_token_usage(state, request, usage) {
        event.payload["run_token_budget"] = snapshot.as_json();
    }
}

fn provider_usage_u64(usage: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| usage.get(key).and_then(Value::as_u64))
}

fn native_assistant_message(payload: &Value) -> Result<Value, String> {
    payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .cloned()
        .ok_or_else(|| "native model response did not include assistant message".to_owned())
}

fn native_assistant_message_content(message: &Value) -> Option<String> {
    match message.get("content") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    }
}

fn native_model_output_limit_hit(payload: &Value) -> bool {
    payload
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| matches!(reason, "length" | "max_tokens"))
}

fn native_model_output_recovery_message(attempt: u8, max_attempts: u8) -> Value {
    json!({
        "role": "user",
        "content": format!(
            "Output token limit hit (recovery {attempt}/{max_attempts}). Resume directly without apology or recap. Break the remaining work into smaller atomic apply_patch calls."
        )
    })
}

fn native_model_output_recovery_event(
    attempt: u8,
    max_attempts: u8,
    max_output_tokens: u32,
) -> HarnessRunEvent {
    HarnessRunEvent::new(
        "model.output_limit.recovery",
        json!({
            "backend": "native-rust",
            "attempt": attempt,
            "max_attempts": max_attempts,
            "max_output_tokens": max_output_tokens,
            "strategy": "resume-smaller-pieces"
        }),
    )
}

fn native_model_chat_completion_body(
    provider: &str,
    model: &str,
    messages: Vec<Value>,
    max_output_tokens: u32,
    request: &coder_harness::HarnessRunRequest,
    supports_parallel_tool_calls: bool,
    mcp_tools: &[NativeModelMcpTool],
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": messages,
        "temperature": 0.2,
        "max_tokens": max_output_tokens
    });
    body["tools"] = native_model_tools_schema(request, mcp_tools);
    body["tool_choice"] = json!("auto");
    body["parallel_tool_calls"] = json!(supports_parallel_tool_calls);
    let effort = request
        .backend_context
        .pointer("/coder/agent/runtime/effort")
        .and_then(Value::as_str);
    let reasoning_effort = provider_reasoning_effort(effort);
    if normalize_provider(provider) == "deepseek" {
        body["thinking"] = if reasoning_effort.is_some() {
            json!({"type": "enabled"})
        } else {
            json!({"type": "disabled"})
        };
    } else if let Some(reasoning_effort) = reasoning_effort {
        body["reasoning_effort"] = json!(reasoning_effort);
    }
    body
}

fn native_model_initial_messages(request: &coder_harness::HarnessRunRequest) -> Vec<Value> {
    vec![
        json!({
            "role": "system",
            "content": native_model_system_prompt(request)
        }),
        json!({
            "role": "user",
            "content": native_model_user_prompt(request)
        }),
    ]
}

fn native_model_system_prompt(request: &coder_harness::HarnessRunRequest) -> String {
    const EXECUTION_CONTRACT: &str = "Work only after Start Work approval. Prefer tool calls for inspect -> write -> verify. Available tool calls use repo-relative paths; never include the repo root, absolute paths, or secrets. Use the apply_patch tool to edit files so related add, update, delete, and move operations commit atomically. Do not use command tools for manual file edits; reserve them for checks and task-required generators. Use command_background for long-running checks, then read_command_output to observe completion. Finish with a short status.";
    let agent_system = request
        .backend_context
        .pointer("/coder/agent/system")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|system| !system.is_empty())
        .unwrap_or("You are Coder native executor.");
    let read_only_contract = if native_model_is_read_only(request) {
        "\nThis is a typed read-only task. Use only the exposed repository and git inspection tools. Do not call commands, writes, skills, or subagents. Finish as soon as the requested facts and evidence are sufficient."
    } else {
        ""
    };
    format!("{agent_system}\n\n{EXECUTION_CONTRACT}{read_only_contract}")
}

fn native_model_user_prompt(request: &coder_harness::HarnessRunRequest) -> String {
    let plan_context = request
        .backend_context
        .pointer("/coder/plan_context")
        .cloned()
        .unwrap_or(Value::Null);
    let runtime = harness_agent_runtime(request);
    let turn_limit = format!(
        "Execution budget: at most {} provider turns. ",
        native_model_max_turns(request, &runtime)
    );
    format!(
        "Task:\n{}\n\nRepo root is already selected and must not be repeated in file paths.\n{}Finish as soon as implementation and checks are complete.\nPlan context JSON:\n{}\n\nTreat affected_paths as the approved change scope when it is present.",
        request.task, turn_limit, plan_context
    )
}

fn native_model_tools_schema(
    request: &coder_harness::HarnessRunRequest,
    mcp_tools: &[NativeModelMcpTool],
) -> Value {
    let mut specs = builtin_tools()
        .iter()
        .copied()
        .filter_map(|tool| {
            let spec = tool.model_spec()?;
            native_model_tool_is_selected(request, tool.name).then_some(spec)
        })
        .collect::<Vec<_>>();
    if !native_model_is_read_only(request) {
        specs.extend(mcp_tools.iter().map(NativeModelMcpTool::model_spec));
    }
    Value::Array(specs)
}

fn native_model_tool_is_selected(
    request: &coder_harness::HarnessRunRequest,
    tool_name: &str,
) -> bool {
    if tool_name == "finish" {
        return true;
    }
    if native_model_is_read_only(request)
        && !matches!(
            canonical_model_tool_name(tool_name),
            "repo_find_files"
                | "repo_search_text"
                | "repo_read_file"
                | "repo_read_file_range"
                | "git_status"
                | "git_diff"
        )
    {
        return false;
    }
    let selected = request
        .backend_context
        .pointer("/coder/harness/selected_tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(canonical_model_tool_name)
        .collect::<BTreeSet<_>>();
    let canonical = canonical_model_tool_name(tool_name);
    if canonical == "apply_patch" {
        return selected.contains("apply_patch") || selected.contains("patch_apply");
    }
    if tool_name == "edit_text_file" {
        return selected.contains("write_text_file");
    }
    if canonical == "write_text_file" {
        return selected.contains("write_text_file");
    }
    canonical != "unknown" && selected.contains(canonical)
}

fn native_model_tool_is_authorized(
    request: &coder_harness::HarnessRunRequest,
    tool_name: &str,
) -> bool {
    if native_model_tool_is_selected(request, tool_name) {
        return true;
    }
    if native_model_is_read_only(request)
        || !matches!(tool_name, "edit_text_file" | "write_text_file")
    {
        return false;
    }
    request
        .backend_context
        .pointer("/coder/harness/selected_tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(canonical_model_tool_name)
        .any(|tool| tool == "apply_patch")
}

fn native_model_is_read_only(request: &coder_harness::HarnessRunRequest) -> bool {
    request
        .backend_context
        .pointer("/coder/plan_context/plan_draft/execution_mode")
        .and_then(Value::as_str)
        == Some("read_only")
}

fn native_model_max_turns(
    request: &coder_harness::HarnessRunRequest,
    runtime: &ResolvedAgentRuntimePolicy,
) -> usize {
    let configured = usize::try_from(runtime.max_turns).unwrap_or(usize::MAX);
    if native_model_is_read_only(request) {
        configured.min(NATIVE_READ_ONLY_MAX_TURNS)
    } else {
        configured
    }
}

fn native_assistant_tool_history_message(message: &Value) -> Value {
    let mut history = json!({
        "role": "assistant",
        "content": message.get("content").cloned().unwrap_or(Value::Null)
    });
    if let Some(tool_calls) = message.get("tool_calls") {
        history["tool_calls"] = tool_calls.clone();
    }
    history
}

fn native_model_tool_calls(message: &Value, output_limit_hit: bool) -> Vec<NativeModelToolCall> {
    message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|call| {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("native-tool-call")
                .to_owned();
            let function = call.get("function")?;
            let name = function.get("name")?.as_str()?.to_owned();
            let arguments = match function.get("arguments") {
                Some(Value::String(text)) => serde_json::from_str::<Value>(text).map_err(|error| {
                    if output_limit_hit {
                        format!("provider output limit truncated tool arguments: {error}")
                    } else {
                        format!("invalid JSON arguments: {error}")
                    }
                }),
                Some(Value::Object(_)) => Ok(function["arguments"].clone()),
                Some(other) => Err(format!(
                    "arguments must be object or JSON string, got {other}"
                )),
                None => Ok(json!({})),
            };
            Some(NativeModelToolCall {
                id,
                name,
                arguments,
            })
        })
        .collect()
}

fn prepare_native_model_tool_turn(
    request: &coder_harness::HarnessRunRequest,
    tool_calls: Vec<NativeModelToolCall>,
    mcp_tools: &[NativeModelMcpTool],
) -> (Vec<PreparedNativeModelToolCall>, Vec<ModelToolUseBlock>) {
    let mut prepared = Vec::with_capacity(tool_calls.len());
    let mut tool_uses = Vec::with_capacity(tool_calls.len());
    for tool_call in tool_calls {
        let tool_call_id = tool_call.id;
        let requested_tool_name = tool_call.name;
        let canonical = canonical_model_tool_name(&requested_tool_name);
        let canonical_tool_name = if canonical == "unknown" {
            requested_tool_name.clone()
        } else {
            canonical.to_owned()
        };
        let mut arguments = match tool_call.arguments {
            Ok(arguments) => arguments,
            Err(error) => {
                prepared.push(PreparedNativeModelToolCall::Synthetic(
                    native_model_tool_error(tool_call_id, canonical_tool_name, error),
                ));
                continue;
            }
        };
        prepare_native_model_tool_input(&canonical_tool_name, request, &mut arguments);
        let model_tool = mcp_tools
            .iter()
            .find(|tool| tool.provider_name == requested_tool_name);
        tool_uses.push(if let Some(model_tool) = model_tool {
            ModelToolUseBlock::with_concurrency(
                tool_call_id.clone(),
                requested_tool_name,
                arguments,
                model_tool.concurrency(),
            )
        } else {
            ModelToolUseBlock::new(tool_call_id.clone(), requested_tool_name, arguments)
        });
        prepared.push(PreparedNativeModelToolCall::Shared {
            tool_call_id,
            canonical_tool_name,
        });
    }
    (prepared, tool_uses)
}

fn native_model_turn_context(
    request: &coder_harness::HarnessRunRequest,
    mcp_tools: &[NativeModelMcpTool],
) -> TurnContext {
    let mut selected_tools = builtin_tools()
        .iter()
        .filter(|tool| native_model_tool_is_authorized(request, tool.name))
        .map(|tool| tool.name.to_owned())
        .collect::<Vec<_>>();
    if !native_model_is_read_only(request) {
        selected_tools.extend(mcp_tools.iter().map(|tool| tool.provider_name.clone()));
    }
    TurnContext {
        run_id: Some(request.run_id.to_string()),
        repo_root: Some(request.repo_root.clone()),
        harness_id: Some(request.harness_id.clone()),
        agent_id: Some(request.agent_id.clone()),
        agent_role: request
            .backend_context
            .pointer("/coder/agent/role")
            .and_then(Value::as_str)
            .map(str::to_owned),
        current_model: request
            .backend_context
            .pointer("/coder/model/model")
            .and_then(Value::as_str)
            .map(str::to_owned),
        model_capabilities: Some(harness_model_spec(request).resolved_capabilities()),
        current_effort: request
            .backend_context
            .pointer("/coder/agent/runtime/effort")
            .cloned(),
        selected_tools,
        permission_policy: request
            .backend_context
            .pointer("/coder/harness/permissions")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),
        start_work_authorized: start_work_authorized(request),
        token_budget: request
            .backend_context
            .pointer("/coder/workflow_loop/token_budget")
            .and_then(Value::as_u64),
        ..TurnContext::default()
    }
}

fn native_model_tool_call_result_from_shared(
    result: ModelToolResultBlock,
    canonical_tool_name: String,
) -> NativeModelToolCallResult {
    NativeModelToolCallResult {
        tool_call_id: result.tool_use_id,
        tool_name: canonical_tool_name,
        status: result.status,
        is_error: result.is_error,
        content: truncate_tool_content(result.content),
        refs: result.refs,
    }
}

fn synthetic_missing_native_tool_result(
    tool_call_id: &str,
    tool_name: &str,
) -> ModelToolResultBlock {
    ModelToolResultBlock {
        contract: coder_workflow::MODEL_TOOL_RESULT_CONTRACT,
        source: "coder-workflow",
        result_type: "tool_result",
        tool_use_id: tool_call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        status: "failed".to_owned(),
        is_error: true,
        content: format!(
            "<tool_use_error>Error calling tool ({tool_name}): missing shared tool result</tool_use_error>"
        ),
        content_truncated: false,
        payload: json!({"status": "failed", "error": "missing shared tool result"}),
        refs: Vec::new(),
        phases: Vec::new(),
    }
}

fn absorb_native_model_shared_tool_report(
    outcome: &mut NativeModelToolLoopOutcome,
    tool_name: &str,
    payload: &Value,
) {
    let payload = payload.get("original_payload").unwrap_or(payload);
    if matches!(tool_name, "write_text_file" | "edit_text_file") {
        if let Some(path) = payload
            .pointer("/changed_file/path")
            .and_then(Value::as_str)
        {
            outcome.changed_files.push(path.to_owned());
        }
        if let Some(ref_id) = payload
            .pointer("/evidence_ref/ref_id")
            .and_then(Value::as_str)
        {
            outcome.evidence_refs.push(EvidenceRef {
                kind: "repo_evidence".to_owned(),
                reference: format!("repo-evidence://{ref_id}"),
            });
        }
    }
    if tool_name == "finish" {
        outcome.status = payload
            .get("status")
            .and_then(Value::as_str)
            .filter(|status| *status == "blocked")
            .map(|_| "blocked")
            .unwrap_or("completed")
            .to_owned();
        if let Some(summary) = payload.get("summary").and_then(Value::as_str) {
            outcome.summary = summary.to_owned();
        }
        outcome
            .checks
            .extend(native_tool_string_array(payload, "checks"));
        outcome
            .blockers
            .extend(native_tool_string_array(payload, "blockers"));
        return;
    }
    if matches!(tool_name, "agent_subagent" | "read_subagent_status") {
        if let Some(report) = payload
            .get("report")
            .cloned()
            .and_then(|value| serde_json::from_value::<FinalReport>(value).ok())
        {
            outcome.changed_files.extend(report.changed_files);
            outcome.evidence_refs.extend(report.evidence_refs);
        }
    }
}

fn drain_native_model_async_attachments(
    state: &ApiState,
    request: &coder_harness::HarnessRunRequest,
) -> Vec<Value> {
    let run_id = RunId::from_string(request.run_id.to_string());
    let mut attachments = drain_async_hook_response_attachments(&state.store, &run_id);
    attachments.extend(drain_planner_user_guidance_attachments(state, &run_id));
    attachments.extend(drain_async_rewake_notification_attachments(
        &state.store,
        &run_id,
        true,
        Some(request.agent_id.as_str()),
    ));
    attachments
}

fn append_native_model_attachment_messages(
    messages: &mut Vec<Value>,
    attachments: Vec<Value>,
    outcome: &mut NativeModelToolLoopOutcome,
    turn: usize,
    delivery_point: &'static str,
) {
    if attachments.is_empty() {
        return;
    }
    let attachment_types = attachments
        .iter()
        .filter_map(|attachment| attachment.get("type").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for attachment in attachments {
        messages.push(json!({
            "role": "system",
            "content": native_model_attachment_content(&attachment)
        }));
    }
    outcome.events.push(HarnessRunEvent::new(
        "model.tool_turn.attachments_delivered",
        json!({
            "backend": "native-rust",
            "implementation": "native-model-tool-loop",
            "execution_mode": "tool_loop",
            "turn": turn,
            "delivery_point": delivery_point,
            "attachment_count": attachment_types.len(),
            "attachment_types": attachment_types,
            "delivery_channel": "model_tool_turn_attachment"
        }),
    ));
}

fn native_model_attachment_content(attachment: &Value) -> String {
    if let Some(text) = attachment
        .get("model_content")
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
    {
        return text.to_owned();
    }
    if let Some(blocks) = attachment.get("model_content").and_then(Value::as_array) {
        let text = blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n");
        if !text.trim().is_empty() {
            return text;
        }
    }
    if let Some(prompt) = attachment.get("prompt").and_then(Value::as_str) {
        return prompt.to_owned();
    }
    serde_json::to_string_pretty(attachment).unwrap_or_else(|_| attachment.to_string())
}

fn prepare_native_model_tool_input(
    tool_name: &str,
    request: &coder_harness::HarnessRunRequest,
    input: &mut Value,
) {
    if !input.is_object() {
        *input = json!({});
    }
    let Some(object) = input.as_object_mut() else {
        return;
    };
    match tool_name {
        "repo_find_files" => {
            object.entry("max_results".to_owned()).or_insert(json!(80));
        }
        "command_run" | "command_background" => {
            object
                .entry("cwd".to_owned())
                .or_insert_with(|| Value::String(".".to_owned()));
            object
                .entry("source".to_owned())
                .or_insert_with(|| Value::String("model".to_owned()));
            object
                .entry("sandbox".to_owned())
                .or_insert_with(|| Value::Bool(true));
            object
                .entry("max_output_bytes".to_owned())
                .or_insert_with(|| json!(coder_tools::DEFAULT_MAX_COMMAND_OUTPUT_BYTES));
            if tool_name == "command_run" {
                object
                    .entry("background_on_timeout".to_owned())
                    .or_insert_with(|| Value::Bool(true));
            }
        }
        "agent_subagent" => {
            object
                .entry("backend_context".to_owned())
                .or_insert_with(|| request.backend_context.clone());
        }
        _ => {}
    }
}

fn native_model_tool_error(
    tool_call_id: impl Into<String>,
    tool_name: impl Into<String>,
    error: impl ToString,
) -> NativeModelToolCallResult {
    let error = error.to_string();
    let payload = json!({
        "status": "failed",
        "error": error
    });
    NativeModelToolCallResult {
        tool_call_id: tool_call_id.into(),
        tool_name: tool_name.into(),
        status: "failed".to_owned(),
        is_error: true,
        content: bounded_tool_content(&payload),
        refs: Vec::new(),
    }
}

fn apply_native_model_final_content(
    outcome: &mut NativeModelToolLoopOutcome,
    content: Option<&str>,
) {
    let Some(content) = content.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    if outcome.summary.trim().is_empty() {
        outcome.summary = content.chars().take(400).collect();
    }
}

fn tool_loop_result(
    started: HarnessRunEvent,
    credential_source: String,
    provider: String,
    model_name: String,
    mut outcome: NativeModelToolLoopOutcome,
    state: &ApiState,
    request: &coder_harness::HarnessRunRequest,
) -> Result<HarnessRunResult, coder_store::StoreError> {
    let mut events = vec![started];
    events.push(HarnessRunEvent::new(
        "executor.reasoning_summary",
        json!({
            "backend": "native-rust",
            "implementation": "native-model-tool-loop",
            "execution_mode": "tool_loop",
            "summary": "Run a provider-driven tool loop where Rust executes repo-scoped tools and returns observations.",
            "credential_source": credential_source,
            "provider": provider,
            "model": model_name,
            "tool_call_count": outcome.tool_call_count
        }),
    ));
    events.extend(outcome.events);

    if outcome.changed_files.is_empty() {
        outcome
            .changed_files
            .extend(recorded_run_changed_files(state, &request.run_id)?);
    }
    let mut changed_files = dedupe_strings(outcome.changed_files);
    let evidence_refs = outcome.evidence_refs;
    let checks = if outcome.checks.is_empty() {
        vec![format!(
            "native_model_tool_loop: completed {} tool call(s)",
            outcome.tool_call_count
        )]
    } else {
        dedupe_strings(outcome.checks)
    };
    let status = if outcome.status == "blocked" || !outcome.blockers.is_empty() {
        "blocked"
    } else {
        "completed"
    };
    let summary = if outcome.summary.trim().is_empty() {
        format!(
            "Native model tool loop wrote {} file(s).",
            changed_files.len()
        )
    } else {
        outcome.summary
    };
    let mut report = if status == "blocked" {
        FinalReport::blocked(
            if summary.trim().is_empty() {
                "Native model tool loop stopped before completion.".to_owned()
            } else {
                summary
            },
            concise_blocker(&outcome.blockers),
        )
    } else {
        FinalReport::completed(summary)
    };
    report.changed_files.append(&mut changed_files);
    report.checks = checks;
    report.evidence_refs = dedupe_evidence_refs(evidence_refs);
    events.push(HarnessRunEvent::new(
        format!("backend.native_rust.{status}"),
        json!({
            "backend": "native-rust",
            "implementation": "native-model-tool-loop",
            "execution_mode": "tool_loop",
            "status": status,
            "changed_files": &report.changed_files,
            "checks": &report.checks,
            "tool_call_count": outcome.tool_call_count
        }),
    ));
    Ok(HarnessRunResult {
        status: status.to_owned(),
        report: Some(report),
        events,
    })
}

pub(crate) fn recorded_run_changed_files(
    state: &ApiState,
    run_id: &RunId,
) -> Result<Vec<String>, coder_store::StoreError> {
    let mut changed_files = Vec::new();
    for event in state.store.read_events(run_id)? {
        if let Some(path) = event.payload.get("path").and_then(Value::as_str) {
            if matches!(event.kind.as_str(), "file.written" | "patch.applied") {
                changed_files.push(path.to_owned());
            }
        }
        if let Some(paths) = event.payload.get("changed_files").and_then(Value::as_array) {
            changed_files.extend(paths.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    Ok(dedupe_strings(changed_files))
}

fn bounded_tool_content(payload: &Value) -> String {
    let text = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_owned());
    truncate_tool_content(text)
}

fn truncate_tool_content(mut text: String) -> String {
    if text.len() > NATIVE_MODEL_TOOL_RESULT_MAX_CHARS {
        text.truncate(NATIVE_MODEL_TOOL_RESULT_MAX_CHARS);
        text.push_str("...[truncated]");
    }
    text
}

fn evidence_refs_from_event_refs(refs: &[HarnessRunEventRef]) -> Vec<EvidenceRef> {
    refs.iter()
        .filter(|reference| reference.label == "repo_evidence")
        .map(|reference| EvidenceRef {
            kind: "repo_evidence".to_owned(),
            reference: reference.uri.clone(),
        })
        .collect()
}

fn native_tool_string_array(input: &Value, key: &str) -> Vec<String> {
    input
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn dedupe_evidence_refs(values: Vec<EvidenceRef>) -> Vec<EvidenceRef> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert((value.kind.clone(), value.reference.clone())))
        .collect()
}

fn start_work_authorized(request: &coder_harness::HarnessRunRequest) -> bool {
    request
        .backend_context
        .pointer("/coder/plan_context/start_work_authorized")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn request_agent_role(request: &coder_harness::HarnessRunRequest) -> Option<&str> {
    request
        .backend_context
        .pointer("/coder/agent/role")
        .and_then(Value::as_str)
}

fn native_model_agent_can_execute(request: &coder_harness::HarnessRunRequest) -> bool {
    request_agent_role(request) == Some("executor") || request_is_native_subagent(request)
}

fn request_is_native_subagent(request: &coder_harness::HarnessRunRequest) -> bool {
    request
        .backend_context
        .pointer("/coder/subagent/context/agent_type")
        .and_then(Value::as_str)
        == Some("subagent")
        || request
            .backend_context
            .pointer("/coder_subagent/context/agent_type")
            .and_then(Value::as_str)
            == Some("subagent")
}

fn blocked_result(
    started: HarnessRunEvent,
    summary: impl Into<String>,
    blocker: impl Into<String>,
) -> HarnessRunResult {
    let blocker = blocker.into();
    HarnessRunResult {
        status: "blocked".to_owned(),
        report: Some(FinalReport::blocked(summary, blocker.clone())),
        events: vec![
            started,
            HarnessRunEvent::new(
                "backend.native_rust.blocked",
                json!({
                    "backend": "native-rust",
                    "implementation": "native-model-tool-loop",
                    "status": "blocked",
                    "reason": blocker
                }),
            ),
        ],
    }
}

fn concise_blocker(blockers: &[String]) -> String {
    if blockers.is_empty() {
        "model_reported_blocked".to_owned()
    } else {
        blockers
            .iter()
            .take(3)
            .map(|item| item.trim())
            .filter(|item| !item.is_empty())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coder_harness::SideEffectLevel;
    use coder_store::RunStore;

    fn budget_test_request(run_id: &str, token_budget: u64) -> coder_harness::HarnessRunRequest {
        coder_harness::HarnessRunRequest {
            run_id: RunId::from_string(run_id),
            workflow_id: "planner-led".to_owned(),
            node_id: "executor".to_owned(),
            agent_id: "executor".to_owned(),
            harness_id: "native-code-edit".to_owned(),
            repo_root: ".".to_owned(),
            task: "test".to_owned(),
            backend_context: json!({
                "coder": {
                    "agent": {"runtime": {"max_turns": 2}},
                    "workflow_loop": {"token_budget": token_budget}
                }
            }),
        }
    }

    #[test]
    fn native_provider_usage_charges_shared_non_cached_run_budget() {
        let root = std::env::temp_dir().join(format!("coder-budget-{}", uuid::Uuid::new_v4()));
        let state = ApiState::new(RunStore::new(&root));
        let request = budget_test_request("run-shared-budget", 100);
        let mut first = HarnessRunEvent::new(
            "model.provider_turn.completed",
            json!({
                "input_tokens": 120,
                "cache_read_tokens": 100,
                "output_tokens": 30,
                "estimated_input_tokens": 999,
                "estimated_output_tokens": 999
            }),
        );
        attach_run_token_budget(&state, &request, &mut first);
        assert_eq!(first.payload["run_token_budget"]["used_tokens"], 50);

        let child_request = budget_test_request("run-shared-budget", 100);
        let mut second = HarnessRunEvent::new(
            "model.provider_turn.completed",
            json!({"input_tokens": 10, "cache_read_tokens": 10, "output_tokens": 51}),
        );
        attach_run_token_budget(&state, &child_request, &mut second);
        assert_eq!(second.payload["run_token_budget"]["used_tokens"], 101);
        assert!(check_run_token_budget(&state, &request)
            .expect("configured budget")
            .exhausted());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_executor_effort_reaches_provider_request() {
        let mut request = budget_test_request("run-effort", 100);
        request.backend_context["coder"]["agent"]["runtime"]["effort"] = json!("high");

        let deepseek = native_model_chat_completion_body(
            "deepseek",
            "deepseek-chat",
            Vec::new(),
            256,
            &request,
            true,
            &[],
        );
        assert_eq!(deepseek["thinking"]["type"], "enabled");
        assert!(deepseek["tools"].is_array());
        assert_eq!(deepseek["tool_choice"], "auto");

        let generic = native_model_chat_completion_body(
            "openai-compatible",
            "reasoning-model",
            Vec::new(),
            256,
            &request,
            true,
            &[],
        );
        assert_eq!(generic["reasoning_effort"], "high");

        request.backend_context["coder"]["agent"]["runtime"]
            .as_object_mut()
            .unwrap()
            .remove("effort");
        let default_deepseek = native_model_chat_completion_body(
            "deepseek",
            "deepseek-chat",
            Vec::new(),
            256,
            &request,
            true,
            &[],
        );
        assert_eq!(default_deepseek["thinking"]["type"], "disabled");
    }

    #[tokio::test]
    async fn native_provider_does_not_send_another_request_after_budget_exhaustion() {
        let root = std::env::temp_dir().join(format!("coder-budget-{}", uuid::Uuid::new_v4()));
        let state = ApiState::new(RunStore::new(&root));
        let request = budget_test_request("run-exhausted-budget", 10);
        record_run_token_usage(
            &state,
            &request,
            RunTokenUsage {
                output_tokens: Some(10),
                ..RunTokenUsage::default()
            },
        );

        let output = run_native_model_provider(NativeModelProviderContext {
            state: &state,
            client: &Client::new(),
            url: "http://127.0.0.1:1/should-not-be-called",
            api_key: "unused",
            provider: "test",
            model: "test",
            request: &request,
            max_output_tokens: 256,
            request_max_retries: crate::provider_runtime::PROVIDER_REQUEST_MAX_RETRIES,
            runtime: harness_agent_runtime(&request),
        })
        .await
        .unwrap();
        let outcome = output;
        assert_eq!(outcome.status, "blocked");
        assert!(outcome
            .events
            .iter()
            .any(|event| event.kind == "model.token_budget.exhausted"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_system_prompt_uses_configured_agent_instructions() {
        let request = coder_harness::HarnessRunRequest {
            run_id: RunId::from_string("run-system-prompt"),
            workflow_id: "planner-led".to_owned(),
            node_id: "executor".to_owned(),
            agent_id: "executor".to_owned(),
            harness_id: "native-code-edit".to_owned(),
            repo_root: ".".to_owned(),
            task: "test".to_owned(),
            backend_context: json!({
                "coder": {"agent": {"system": "Role-specific instructions."}}
            }),
        };

        let prompt = native_model_system_prompt(&request);
        assert!(prompt.starts_with("Role-specific instructions."));
        assert!(prompt.contains("inspect -> write -> verify"));
        assert!(prompt.contains("Use the apply_patch tool to edit files"));
        assert!(prompt.contains("Do not use command tools for manual file edits"));
        assert!(prompt.contains("commit atomically"));
        assert!(!prompt.contains("strict JSON"));
    }

    #[test]
    fn plain_assistant_text_is_summary_only_not_a_file_plan() {
        let mut outcome = NativeModelToolLoopOutcome::default();
        apply_native_model_final_content(
            &mut outcome,
            Some(r#"{"status":"completed","files":[{"path":"README.md","content":"x"}]}"#),
        );

        assert!(outcome.summary.contains("README.md"));
        assert!(outcome.changed_files.is_empty());
        assert!(outcome.evidence_refs.is_empty());
    }

    #[test]
    fn native_user_prompt_does_not_repeat_structured_tool_specs_or_task_goal() {
        let request = coder_harness::HarnessRunRequest {
            run_id: RunId::from_string("run-compact-user-prompt"),
            workflow_id: "planner-led".to_owned(),
            node_id: "executor".to_owned(),
            agent_id: "executor".to_owned(),
            harness_id: "native-code-edit".to_owned(),
            repo_root: ".".to_owned(),
            task: "Create README.md.".to_owned(),
            backend_context: json!({
                "coder": {
                    "harness": {"selected_tools": ["repo_read_file", "write_text_file"]},
                    "plan_context": {
                        "start_work_authorized": true,
                        "plan_draft": {
                            "affected_paths": ["README.md"],
                            "acceptance_criteria": ["README.md exists"]
                        }
                    }
                }
            }),
        };

        let prompt = native_model_user_prompt(&request);

        assert_eq!(prompt.matches("Create README.md.").count(), 1);
        assert!(!prompt.contains("Selected tools JSON"));
        assert!(!prompt.contains("repo_read_file"));
        assert!(prompt.contains("README.md exists"));
    }

    #[test]
    fn native_tool_schema_enforces_subagent_inherited_tools() {
        let request = coder_harness::HarnessRunRequest {
            run_id: RunId::from_string("run-subagent-tools"),
            workflow_id: "planner-led".to_owned(),
            node_id: "executor::child".to_owned(),
            agent_id: "child".to_owned(),
            harness_id: "native-code-edit".to_owned(),
            repo_root: ".".to_owned(),
            task: "review".to_owned(),
            backend_context: json!({
                "coder": {
                    "harness": {
                        "selected_tools": [
                            "repo_read_file",
                            "patch_apply",
                            "cancel_command_background"
                        ]
                    },
                    "subagent": {
                        "context": {"agent_type": "subagent"}
                    }
                }
            }),
        };

        let schema = native_model_tools_schema(&request, &[]);
        let names = schema
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();

        assert!(names.contains("repo_read_file"));
        assert!(names.contains("apply_patch"));
        assert!(names.contains("cancel_command_background"));
        assert!(names.contains("finish"));
        assert!(!names.contains("edit_text_file"));
        assert!(!names.contains("write_text_file"));
        assert!(!names.contains("agent_subagent"));
        assert!(!names.contains("command_run"));
        let patch_tool = schema
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["function"]["name"] == "apply_patch")
            .unwrap();
        assert!(
            patch_tool["function"]["parameters"]["properties"]["patch"]["description"]
                .as_str()
                .unwrap()
                .contains("start: begin_patch hunk+ end_patch")
        );
    }

    #[test]
    fn native_tool_schema_exposes_frozen_mcp_snapshot_only_to_executable_tasks() {
        let mut request = budget_test_request("run-mcp-schema", 100);
        request.backend_context["coder"]["harness"] = json!({"selected_tools": ["finish"]});
        let mcp_tools = vec![NativeModelMcpTool {
            provider_name: "mcp__local__lookup".to_owned(),
            server_id: "local".to_owned(),
            tool_name: "lookup".to_owned(),
            description: "Look up local data.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }),
            side_effect: SideEffectLevel::Read,
        }];

        let schema = native_model_tools_schema(&request, &mcp_tools);
        let mcp = schema
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["function"]["name"] == "mcp__local__lookup")
            .unwrap();
        assert_eq!(mcp["function"]["parameters"]["required"][0], "query");
        assert!(native_model_turn_context(&request, &mcp_tools)
            .selected_tools
            .contains(&"mcp__local__lookup".to_owned()));

        request.backend_context["coder"]["plan_context"]["plan_draft"] =
            json!({"execution_mode": "read_only"});
        let read_only_schema = native_model_tools_schema(&request, &mcp_tools);
        assert!(!read_only_schema
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "mcp__local__lookup"));
        assert!(!native_model_turn_context(&request, &mcp_tools)
            .selected_tools
            .contains(&"mcp__local__lookup".to_owned()));
    }

    #[test]
    fn typed_read_only_plan_removes_side_effect_tools_and_caps_turns() {
        let request = coder_harness::HarnessRunRequest {
            run_id: RunId::from_string("run-read-only-tools"),
            workflow_id: "planner-led".to_owned(),
            node_id: "executor".to_owned(),
            agent_id: "executor".to_owned(),
            harness_id: "native-code-edit".to_owned(),
            repo_root: ".".to_owned(),
            task: "Review README.md.".to_owned(),
            backend_context: json!({
                "coder": {
                    "agent": {"runtime": {"max_turns": 24}},
                    "harness": {
                        "selected_tools": [
                            "repo_find_files",
                            "repo_search_text",
                            "repo_read_file",
                            "repo_read_file_range",
                            "git_status",
                            "git_diff",
                            "command_run",
                            "write_text_file",
                            "agent_subagent",
                            "Skill"
                        ]
                    },
                    "plan_context": {
                        "start_work_authorized": true,
                        "plan_draft": {"execution_mode": "read_only"}
                    }
                }
            }),
        };
        let schema = native_model_tools_schema(&request, &[]);
        let names = schema
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();

        assert_eq!(
            names,
            BTreeSet::from([
                "finish",
                "git_diff",
                "git_status",
                "repo_find_files",
                "repo_read_file",
                "repo_read_file_range",
                "repo_search_text"
            ])
        );
        assert_eq!(
            native_model_max_turns(&request, &harness_agent_runtime(&request)),
            NATIVE_READ_ONLY_MAX_TURNS
        );
        let system = native_model_system_prompt(&request);
        assert!(system.contains("typed read-only task"));
        assert!(system.contains("Do not call commands, writes, skills, or subagents"));
        assert!(native_model_user_prompt(&request).contains("at most 8 provider turns"));
    }

    #[test]
    fn native_executor_uses_configured_turn_and_response_bounds() {
        let mut request = coder_harness::HarnessRunRequest {
            run_id: RunId::from_string("run-default-turn-budget"),
            workflow_id: "planner-led".to_owned(),
            node_id: "executor".to_owned(),
            agent_id: "executor".to_owned(),
            harness_id: "native-code-edit".to_owned(),
            repo_root: ".".to_owned(),
            task: "test".to_owned(),
            backend_context: json!({"coder": {"agent": {"runtime": {}}}}),
        };

        assert_eq!(harness_agent_runtime(&request).max_turns, 24);
        assert!(native_model_user_prompt(&request).contains("at most 24 provider turns"));

        request.backend_context["coder"]["agent"]["runtime"]["max_turns"] = json!(7);
        assert_eq!(harness_agent_runtime(&request).max_turns, 7);
        assert!(native_model_user_prompt(&request).contains("at most 7 provider turns"));

        request.backend_context["coder"]["agent"]["runtime"]["max_output_tokens"] = json!(64_000);
        assert_eq!(harness_agent_runtime(&request).max_output_tokens, 8_000);
    }

    #[test]
    fn native_output_limit_recovery_is_detected_and_bounded() {
        let payload = json!({
            "choices": [{
                "finish_reason": "length",
                "message": {
                    "tool_calls": [{
                        "id": "truncated-write",
                        "function": {
                            "name": "write_text_file",
                            "arguments": "{\"path\":\"main.js\",\"content\":\"cut"
                        }
                    }]
                }
            }]
        });
        let message = native_assistant_message(&payload).unwrap();
        let calls = native_model_tool_calls(&message, native_model_output_limit_hit(&payload));

        assert!(native_model_output_limit_hit(&payload));
        assert!(calls[0]
            .arguments
            .as_ref()
            .unwrap_err()
            .contains("provider output limit truncated tool arguments"));
        assert!(native_model_output_recovery_message(1, 3)["content"]
            .as_str()
            .unwrap()
            .contains("smaller atomic apply_patch calls"));
    }

    #[test]
    fn exact_edit_arguments_are_bounded_by_the_change_not_the_file() {
        let full_write = json!({
            "path": "main.js",
            "content": format!("{}const plantType = 's';", "x".repeat(32_000))
        })
        .to_string();
        let exact_edit = json!({
            "path": "main.js",
            "old_string": "const plantType = 'sunflower';",
            "new_string": "const plantType = 's';",
            "replace_all": false
        })
        .to_string();

        assert!(full_write.len() > exact_edit.len() * 100);
    }
}
