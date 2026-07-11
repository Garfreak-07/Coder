use std::{collections::BTreeSet, path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use coder_config::AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_DEFAULT;
use coder_core::{EvidenceRef, FinalReport, RunId};
use coder_harness::{
    HarnessBackend, HarnessError, HarnessRunEvent, HarnessRunEventRef, HarnessRunResult,
};
use coder_store::{RepoEvidenceKind, RepoEvidenceRef};
use coder_tools::{
    edit_text_file_batch, git_diff, write_text_file, FileEditBatchRequest, FileEditReplacement,
    FileWriteRequest,
};
use coder_workflow::NativeRustBackend;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::model_tool_async_attachments::{
    drain_async_hook_response_attachments, drain_async_rewake_notification_attachments,
    drain_planner_user_guidance_attachments,
};
use crate::model_tool_execute_pipeline::execute_model_tool_response;
use crate::model_tool_input::canonical_model_tool_name;
use crate::provider_runtime::{
    harness_model_spec, model_name_for_settings, model_provider_base_url,
    model_provider_for_settings, normalize_provider, provider_api_key,
    provider_chat_completions_endpoint, provider_http_client_builder, provider_proxy_url_for_url,
    provider_reasoning_effort, redact_provider_error,
};
use crate::run_token_budget::{
    check_run_token_budget, provider_token_usage, record_run_token_usage, RunTokenUsage,
};
use crate::{ApiState, ModelToolExecuteRequest};

const NATIVE_MODEL_MAX_FILES: usize = 12;
const NATIVE_MODEL_MAX_FILE_EDITS: usize = 32;
const NATIVE_MODEL_MAX_FILE_BYTES: usize = 512 * 1024;
const NATIVE_MODEL_TOOL_RESULT_MAX_CHARS: usize = 24_000;
const NATIVE_MODEL_DEFAULT_MAX_TURNS: usize = 24;

#[derive(Debug, Clone)]
pub(crate) struct NativeModelBackend {
    state: ApiState,
    fallback: Arc<NativeRustBackend>,
}

impl NativeModelBackend {
    pub(crate) fn new(state: ApiState) -> Self {
        Self {
            fallback: Arc::new(NativeRustBackend::new(state.store.clone())),
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
        if request.harness_id != "native-code-edit"
            || !native_model_agent_can_execute(&request)
            || !native_model_should_handle(&request)
        {
            return self.fallback.run(request).await;
        }
        if !start_work_authorized(&request) {
            return Ok(HarnessRunResult {
                status: "blocked".to_owned(),
                report: Some(FinalReport::blocked(
                    "Native model executor stopped before writing files.",
                    "file writes require Start Work approval in plan_context",
                )),
                events: vec![HarnessRunEvent::new(
                    "backend.native_rust.blocked",
                    json!({
                        "backend": "native-rust",
                        "implementation": "native-model-file-write",
                        "status": "blocked",
                        "reason": "missing_start_work_approval"
                    }),
                )],
            });
        }

        let started = HarnessRunEvent::new(
            "backend.native_rust.started",
            json!({
                "backend": "native-rust",
                "implementation": "native-model-file-write",
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "model_driven": true,
                "side_effect_boundary": "repo_scoped_file_write"
            }),
        );

        let settings = self.state.provider_settings.lock().unwrap().clone();
        if settings.mock_mode {
            return self.fallback.run(request).await;
        }
        let model = harness_model_spec(&request);
        let provider = model_provider_for_settings(&settings, &model);
        let model_name = model_name_for_settings(&settings, &model);
        let Some((api_key, credential_source)) =
            provider_api_key(&settings, &provider, model.api_key_env.as_deref())
        else {
            return self.fallback.run(request).await;
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
        let client = provider_http_client_builder(&url, proxy_url.as_deref())
            .map_err(|error| {
                HarnessError::Failed(redact_provider_error(
                    &error,
                    &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
                ))
            })?
            .timeout(Duration::from_millis(native_model_response_timeout_ms(
                &request,
            )))
            .build()
            .map_err(|error| {
                HarnessError::Failed(redact_provider_error(
                    &error.to_string(),
                    &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
                ))
            })?;

        let provider_output = run_native_model_provider(NativeModelProviderContext {
            state: &self.state,
            client: &client,
            url: &url,
            api_key: &api_key,
            provider: &provider,
            model: &model_name,
            request: &request,
            max_output_tokens: max_output_tokens_from_request(&request),
        })
        .await
        .map_err(|error| {
            HarnessError::Failed(redact_provider_error(
                &error,
                &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
            ))
        })?;
        if let NativeModelProviderOutput::ToolLoop(outcome) = provider_output {
            return tool_loop_result(
                started,
                credential_source,
                provider,
                model_name,
                outcome,
                &self.state,
                &request,
            )
            .map_err(|error| HarnessError::Failed(error.to_string()));
        }
        let NativeModelProviderOutput::Text(text_outcome) = provider_output else {
            unreachable!("native provider output was already handled")
        };
        let NativeModelTextOutcome {
            content: response,
            events: provider_events,
        } = text_outcome;
        let plan = match parse_native_model_plan(&response) {
            Ok(plan) => plan,
            Err(error) => {
                return Ok(failed_result(
                    started,
                    format!("Native model executor could not parse model output: {error}"),
                ));
            }
        };
        if plan.status == "blocked" || !plan.blockers.is_empty() {
            return Ok(blocked_result(
                started,
                "Native model executor reported it could not safely complete the task.",
                concise_blocker(&plan.blockers),
            ));
        }
        if plan.files.is_empty() {
            if !native_model_requires_file_writes(&request) {
                return Ok(no_file_report_result(
                    started,
                    credential_source,
                    provider,
                    model_name,
                    plan,
                ));
            }
            return Ok(blocked_result(
                started,
                "Native model executor produced no file writes.",
                "model_returned_no_files",
            ));
        }
        if plan.files.len() > NATIVE_MODEL_MAX_FILES {
            return Ok(blocked_result(
                started,
                "Native model executor refused an oversized file plan.",
                "model_returned_too_many_files",
            ));
        }

        let mut events = vec![started];
        events.extend(provider_events);
        events.push(HarnessRunEvent::new(
            "executor.reasoning_summary",
            json!({
                "backend": "native-rust",
                "implementation": "native-model-file-write",
                "summary": "Use the provider-generated file plan, then write only repo-scoped files and record evidence.",
                "credential_source": credential_source,
                "provider": provider,
                "model": model_name
            }),
        ));

        let mut changed_files = Vec::new();
        let mut evidence_refs = Vec::new();
        for file in plan.files {
            if file.content.is_empty() {
                return Ok(blocked_result(
                    events.remove(0),
                    "Native model executor refused an empty file write.",
                    format!("empty_file_content: {}", file.path),
                ));
            }
            let evidence = write_text_file(
                &request.repo_root,
                FileWriteRequest {
                    path: PathBuf::from(&file.path),
                    content: file.content,
                    max_bytes: NATIVE_MODEL_MAX_FILE_BYTES,
                    source: "model".to_owned(),
                },
            )
            .map_err(|error| HarnessError::Failed(error.to_string()))?;
            let evidence_ref = write_file_evidence(&self.state, &request.run_id, &evidence)
                .map_err(|error| HarnessError::Failed(error.to_string()))?;
            changed_files.push(evidence.path.clone());
            evidence_refs.push(repo_evidence_ref(&evidence_ref));
            events.push(
                HarnessRunEvent::new(
                    "file.written",
                    json!({
                        "backend": "native-rust",
                        "implementation": "native-model-file-write",
                        "path": &evidence.path,
                        "status": &evidence.status,
                        "created": evidence.created,
                        "bytes_written": evidence.bytes_written,
                        "evidence_ref": &evidence_ref.ref_id
                    }),
                )
                .with_ref(
                    "repo_evidence",
                    format!("repo-evidence://{}", evidence_ref.ref_id),
                ),
            );
        }

        if changed_files.is_empty() {
            return Ok(blocked_result(
                events.remove(0),
                "Native model executor made no file changes.",
                "no_changed_files",
            ));
        }
        let diff_ref = write_git_diff_evidence(&self.state, &request.run_id, &request.repo_root)
            .map_err(|error| HarnessError::Failed(error.to_string()))?;
        if let Some(reference) = diff_ref {
            evidence_refs.push(repo_evidence_ref(&reference));
            events.push(
                HarnessRunEvent::new(
                    "git.diff.recorded",
                    json!({
                        "backend": "native-rust",
                        "implementation": "native-model-file-write",
                        "evidence_ref": reference.ref_id
                    }),
                )
                .with_ref(
                    "repo_evidence",
                    format!("repo-evidence://{}", reference.ref_id),
                ),
            );
        }

        let mut report = FinalReport::completed(if plan.summary.trim().is_empty() {
            format!(
                "Native model executor wrote {} file(s).",
                changed_files.len()
            )
        } else {
            plan.summary
        });
        report.changed_files = changed_files;
        report.checks = if plan.checks.is_empty() {
            vec!["native_model_file_write: completed".to_owned()]
        } else {
            plan.checks
        };
        report.evidence_refs = evidence_refs;
        events.push(HarnessRunEvent::new(
            "backend.native_rust.completed",
            json!({
                "backend": "native-rust",
                "implementation": "native-model-file-write",
                "status": "completed",
                "changed_files": &report.changed_files,
                "checks": &report.checks
            }),
        ));
        Ok(HarnessRunResult {
            status: "completed".to_owned(),
            report: Some(report),
            events,
        })
    }
}

#[derive(Debug, Deserialize)]
struct NativeModelPlan {
    #[serde(default = "completed_status")]
    status: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    files: Vec<NativeModelFile>,
    #[serde(default)]
    checks: Vec<String>,
    #[serde(default)]
    blockers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct NativeModelFile {
    path: String,
    content: String,
}

fn completed_status() -> String {
    "completed".to_owned()
}

enum NativeModelProviderOutput {
    Text(NativeModelTextOutcome),
    ToolLoop(NativeModelToolLoopOutcome),
}

struct NativeModelTextOutcome {
    content: String,
    events: Vec<HarnessRunEvent>,
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
    attachments: Vec<Value>,
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
}

async fn run_native_model_provider(
    context: NativeModelProviderContext<'_>,
) -> Result<NativeModelProviderOutput, String> {
    let NativeModelProviderContext {
        state,
        client,
        url,
        api_key,
        provider,
        model,
        request,
        max_output_tokens,
    } = context;
    let mut messages = native_model_initial_messages(request);
    let mut outcome = NativeModelToolLoopOutcome {
        status: "completed".to_owned(),
        ..NativeModelToolLoopOutcome::default()
    };
    let mut used_tool_loop = false;
    let max_output_recovery_attempts = native_model_max_output_recovery_attempts(request);
    let mut output_recovery_attempts = 0_u8;

    let max_turns = native_model_max_turns_from_request(request);
    for turn in 0..max_turns {
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
                        "next_turn": turn + 1
                    }),
                ));
                return Ok(NativeModelProviderOutput::ToolLoop(outcome));
            }
        }
        let pending_attachments = drain_native_model_async_attachments(state, request);
        append_native_model_attachment_messages(
            &mut messages,
            pending_attachments,
            &mut outcome,
            turn + 1,
            "before_provider_request",
        );
        let body = native_model_chat_completion_body(
            provider,
            model,
            messages.clone(),
            max_output_tokens,
            request,
            true,
        );
        let payload = match send_native_chat_completion(client, url, api_key, &body).await {
            Ok(payload) => payload,
            Err(_error) if turn == 0 => {
                let fallback_body = native_model_chat_completion_body(
                    provider,
                    model,
                    native_model_initial_messages(request),
                    max_output_tokens,
                    request,
                    false,
                );
                let fallback_payload =
                    send_native_chat_completion(client, url, api_key, &fallback_body).await?;
                let mut usage_event = native_model_provider_usage_event(
                    provider,
                    model,
                    turn + 1,
                    &fallback_body,
                    &fallback_payload,
                    true,
                );
                attach_run_token_budget(state, request, &mut usage_event);
                outcome.events.push(usage_event);
                let content =
                    native_assistant_content(&fallback_payload)?.unwrap_or_else(|| "".to_owned());
                return Ok(NativeModelProviderOutput::Text(NativeModelTextOutcome {
                    content,
                    events: outcome.events,
                }));
            }
            Err(error) => return Err(error),
        };
        let mut usage_event =
            native_model_provider_usage_event(provider, model, turn + 1, &body, &payload, false);
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
                return Ok(NativeModelProviderOutput::ToolLoop(outcome));
            }
            let pending_attachments = drain_native_model_async_attachments(state, request);
            if !pending_attachments.is_empty() {
                messages.push(native_assistant_tool_history_message(&message));
                append_native_model_attachment_messages(
                    &mut messages,
                    pending_attachments,
                    &mut outcome,
                    turn + 1,
                    "before_final_response",
                );
                used_tool_loop = true;
                continue;
            }
            if used_tool_loop {
                apply_native_model_final_content(&mut outcome, content.as_deref());
                return Ok(NativeModelProviderOutput::ToolLoop(outcome));
            }
            return Ok(NativeModelProviderOutput::Text(NativeModelTextOutcome {
                content: content.unwrap_or_else(|| "".to_owned()),
                events: outcome.events,
            }));
        }

        used_tool_loop = true;
        outcome.events.push(HarnessRunEvent::new(
            "model.tool_turn.started",
            json!({
                "backend": "native-rust",
                "implementation": "native-model-file-write",
                "execution_mode": "tool_loop",
                "turn": turn + 1,
                "tool_call_count": tool_calls.len()
            }),
        ));
        messages.push(native_assistant_tool_history_message(&message));
        let mut finish_requested = false;
        for tool_call in tool_calls {
            let result =
                execute_native_model_tool_call(state, request, tool_call, &mut outcome).await;
            finish_requested |= result.tool_name == "finish" && !result.is_error;
            let mut event = HarnessRunEvent::new(
                "model.tool_call.completed",
                json!({
                    "backend": "native-rust",
                    "implementation": "native-model-file-write",
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
            append_native_model_attachment_messages(
                &mut messages,
                result.attachments,
                &mut outcome,
                turn + 1,
                "after_tool_result",
            );
        }
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
                return Ok(NativeModelProviderOutput::ToolLoop(outcome));
            }
        }
        if finish_requested {
            return Ok(NativeModelProviderOutput::ToolLoop(outcome));
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
    Ok(NativeModelProviderOutput::ToolLoop(outcome))
}

async fn send_native_chat_completion(
    client: &Client,
    url: &str,
    api_key: &str,
    body: &Value,
) -> Result<Value, String> {
    let response = client
        .post(url)
        .bearer_auth(api_key)
        .json(body)
        .send()
        .await
        .map_err(|error| format!("native model request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("native model returned HTTP {}", response.status()));
    }
    response
        .json()
        .await
        .map_err(|error| format!("native model response was not JSON: {error}"))
}

fn native_model_provider_usage_event(
    provider: &str,
    model: &str,
    turn: usize,
    request_body: &Value,
    response_payload: &Value,
    streaming_fallback: bool,
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
            "streaming_fallback": streaming_fallback,
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

fn native_assistant_content(payload: &Value) -> Result<Option<String>, String> {
    native_assistant_message(payload).map(|message| native_assistant_message_content(&message))
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
            "Output token limit hit (recovery {attempt}/{max_attempts}). Resume directly without apology or recap. Break the remaining work into smaller pieces. Use edit_text_file for a small exact change to an existing file; use one focused write_text_file call per new file."
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
    tools_enabled: bool,
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": messages,
        "temperature": 0.2,
        "max_tokens": max_output_tokens
    });
    if tools_enabled {
        body["tools"] = native_model_tools_schema(request);
        body["tool_choice"] = json!("auto");
    }
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
    const EXECUTION_CONTRACT: &str = "Work only after Start Work approval. Prefer tool calls for inspect -> write -> verify. Available tool calls use repo-relative paths; never include the repo root, absolute paths, or secrets. For localized changes to existing files, use edit_text_file with the smallest clearly unique exact old_string. When one file needs multiple independent changes, use one edit_text_file call with the edits array; edits apply sequentially and atomically. Use write_text_file only for new files or deliberate whole-file rewrites. Use command_background for long-running checks, then read_command_output to observe completion. Finish with a short status. If tool calls are unavailable, return strict JSON with this schema: {\"status\":\"completed|blocked\",\"summary\":\"short\",\"files\":[{\"path\":\"relative/path\",\"content\":\"full file text\"}],\"checks\":[\"short check\"],\"blockers\":[\"reason\"]}. No markdown fences. Keep the implementation dependency-free unless the task explicitly asks for dependencies. Prefer a small complete web app with index.html, style.css, and main.js for browser game tasks.";
    let agent_system = request
        .backend_context
        .pointer("/coder/agent/system")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|system| !system.is_empty())
        .unwrap_or("You are Coder native executor.");
    format!("{agent_system}\n\n{EXECUTION_CONTRACT}")
}

fn native_model_user_prompt(request: &coder_harness::HarnessRunRequest) -> String {
    let plan_context = request
        .backend_context
        .pointer("/coder/plan_context")
        .cloned()
        .unwrap_or(Value::Null);
    let selected_tools = request
        .backend_context
        .pointer("/coder/harness/selected_tools")
        .cloned()
        .unwrap_or(Value::Null);
    format!(
        "Task:\n{}\n\nRepo root is already selected and must not be repeated in file paths.\nExecution budget: at most {} provider turns. Finish early as soon as the implementation is verifier-ready.\nPlan context JSON:\n{}\n\nSelected tools JSON:\n{}\n\nReturn only strict JSON. If affected_paths are listed, write those paths unless the task clearly requires companion browser files.",
        request.task,
        native_model_max_turns_from_request(request),
        plan_context,
        selected_tools
    )
}

fn native_model_tools_schema(request: &coder_harness::HarnessRunRequest) -> Value {
    let tools = json!([
        {
            "type": "function",
            "function": {
                "name": "repo_find_files",
                "description": "List repo files by optional query and extension filters.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "extensions": {"type": "array", "items": {"type": "string"}},
                        "max_results": {"type": "integer", "minimum": 1, "maximum": 200}
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "repo_search_text",
                "description": "Search bounded repository text.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "max_matches": {"type": "integer", "minimum": 1, "maximum": 50}
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "repo_read_file",
                "description": "Read a bounded UTF-8 repo file.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "max_file_bytes": {"type": "integer", "minimum": 1, "maximum": 65536}
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "repo_read_file_range",
                "description": "Read a bounded line range from a repo file.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "start_line": {"type": "integer", "minimum": 1},
                        "max_lines": {"type": "integer", "minimum": 1, "maximum": 200},
                        "max_chars": {"type": "integer", "minimum": 1, "maximum": 100000}
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "git_status",
                "description": "Read bounded git status.",
                "parameters": {"type": "object", "properties": {}, "additionalProperties": false}
            }
        },
        {
            "type": "function",
            "function": {
                "name": "git_diff",
                "description": "Read bounded git diff.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "max_output_bytes": {"type": "integer", "minimum": 1, "maximum": 65536}
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "command_run",
                "description": "Run a bounded sandboxed command in the repo. Long commands are automatically backgrounded on timeout.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                        "cwd": {"type": "string"},
                        "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 600},
                        "foreground_timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 600},
                        "max_output_bytes": {"type": "integer", "minimum": 1, "maximum": 2097152}
                    },
                    "required": ["argv"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "command_background",
                "description": "Start a sandboxed background command in the repo.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                        "cwd": {"type": "string"},
                        "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 600},
                        "max_output_bytes": {"type": "integer", "minimum": 1, "maximum": 2097152}
                    },
                    "required": ["argv"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_command_output",
                "description": "Read or wait for a background command task.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": {"type": "string"},
                        "timeout": {"type": "integer", "minimum": 0, "maximum": 120000},
                        "block": {"type": "boolean"}
                    },
                    "required": ["task_id"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "task_stop",
                "description": "Stop a background command or subagent task.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": {"type": "string"}
                    },
                    "required": ["task_id"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "agent_subagent",
                "description": "Run a scoped child agent. It is synchronous unless run_in_background=true; a synchronous result is final and needs no status lookup.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task": {"type": "string"},
                        "subagent_type": {"type": "string"},
                        "subagent_name": {"type": "string"},
                        "run_in_background": {"type": "boolean"}
                    },
                    "required": ["task"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_subagent_status",
                "description": "Read or wait for a background subagent only. Use background_task.task_id returned by agent_subagent, never agent_id.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": {"type": "string"},
                        "timeout": {"type": "integer", "minimum": 0, "maximum": 120000},
                        "block": {"type": "boolean"}
                    },
                    "required": ["task_id"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "cancel_subagent_background",
                "description": "Cancel a background subagent task.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": {"type": "string"}
                    },
                    "required": ["task_id"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "skill",
                "description": "Invoke an installed or built-in skill through the shared skill tool runtime.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "skill": {"type": "string"},
                        "name": {"type": "string"},
                        "command": {"type": "string"}
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit_text_file",
                "description": "Replace exact strings in one existing repo-relative UTF-8 file. Provide old_string/new_string for one edit or edits for multiple sequential atomic edits. Each old_string must be unique unless replace_all=true.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "old_string": {"type": "string", "minLength": 1},
                        "new_string": {"type": "string"},
                        "replace_all": {"type": "boolean"},
                        "edits": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": NATIVE_MODEL_MAX_FILE_EDITS,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "old_string": {"type": "string", "minLength": 1},
                                    "new_string": {"type": "string"},
                                    "replace_all": {"type": "boolean"}
                                },
                                "required": ["old_string", "new_string"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_text_file",
                "description": "Write full UTF-8 text content to a new repo-relative file or deliberately replace a whole file.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "content": {"type": "string"}
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "finish",
                "description": "Finish after tool work is complete or blocked.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "status": {"type": "string", "enum": ["completed", "blocked"]},
                        "summary": {"type": "string"},
                        "checks": {"type": "array", "items": {"type": "string"}},
                        "blockers": {"type": "array", "items": {"type": "string"}}
                    },
                    "required": ["status", "summary"],
                    "additionalProperties": false
                }
            }
        }
    ]);
    Value::Array(
        tools
            .as_array()
            .into_iter()
            .flatten()
            .filter(|tool| {
                tool.pointer("/function/name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| native_model_tool_is_selected(request, name))
            })
            .cloned()
            .collect(),
    )
}

fn native_model_tool_is_selected(
    request: &coder_harness::HarnessRunRequest,
    tool_name: &str,
) -> bool {
    if tool_name == "finish" {
        return true;
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
    if tool_name == "edit_text_file" {
        return selected.contains("write_text_file") || selected.contains("patch_apply");
    }
    if canonical == "write_text_file" {
        return selected.contains("write_text_file") || selected.contains("patch_apply");
    }
    if canonical == "task_stop" {
        return selected.contains("task_stop")
            || selected.contains("cancel_command_background")
            || selected.contains("cancel_subagent_background");
    }
    canonical != "unknown" && selected.contains(canonical)
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

async fn execute_native_model_tool_call(
    state: &ApiState,
    request: &coder_harness::HarnessRunRequest,
    tool_call: NativeModelToolCall,
    outcome: &mut NativeModelToolLoopOutcome,
) -> NativeModelToolCallResult {
    outcome.tool_call_count += 1;
    let tool_call_id = tool_call.id;
    let requested_tool_name = tool_call.name.clone();
    let canonical_tool_name = canonical_model_tool_name(&tool_call.name);
    let tool_name = if canonical_tool_name == "unknown" {
        tool_call.name.clone()
    } else {
        canonical_tool_name.to_owned()
    };
    if !native_model_tool_is_selected(request, &tool_name) {
        return native_model_tool_error(
            tool_call_id,
            tool_name,
            format!("tool '{}' is not selected for this agent", tool_call.name),
        );
    }
    let arguments = match tool_call.arguments {
        Ok(arguments) => arguments,
        Err(error) => {
            return native_model_tool_error(tool_call_id, tool_name, error);
        }
    };
    match tool_name.as_str() {
        "repo_find_files"
        | "repo_search_text"
        | "repo_read_file"
        | "repo_read_file_range"
        | "git_status"
        | "git_diff"
        | "command_run"
        | "command_background"
        | "read_command_output"
        | "task_stop"
        | "agent_subagent"
        | "read_subagent_status"
        | "cancel_subagent_background"
        | "skill" => {
            execute_native_model_shared_tool(
                state,
                request,
                tool_call_id,
                tool_name,
                requested_tool_name,
                arguments,
                outcome,
            )
            .await
        }
        "write_text_file" => {
            execute_native_model_write_text_file(state, request, tool_call_id, arguments, outcome)
        }
        "edit_text_file" => {
            execute_native_model_edit_text_file(state, request, tool_call_id, arguments, outcome)
        }
        "finish" => execute_native_model_finish(tool_call_id, arguments, outcome),
        _ => native_model_tool_error(
            tool_call_id,
            tool_name,
            format!("unsupported native executor tool '{}'", tool_call.name),
        ),
    }
}

async fn execute_native_model_shared_tool(
    state: &ApiState,
    request: &coder_harness::HarnessRunRequest,
    tool_call_id: String,
    canonical_tool_name: String,
    requested_tool_name: String,
    mut arguments: Value,
    outcome: &mut NativeModelToolLoopOutcome,
) -> NativeModelToolCallResult {
    prepare_native_model_shared_tool_input(&canonical_tool_name, request, &mut arguments);
    let response = execute_model_tool_response(
        state.clone(),
        ModelToolExecuteRequest {
            tool_use_id: tool_call_id.clone(),
            tool_name: requested_tool_name,
            run_id: Some(request.run_id.to_string()),
            harness_id: Some(request.harness_id.clone()),
            agent_id: Some(request.agent_id.clone()),
            current_model: request
                .backend_context
                .pointer("/coder/model/model")
                .and_then(Value::as_str)
                .map(str::to_owned),
            current_effort: request
                .backend_context
                .pointer("/coder/agent/runtime/effort")
                .cloned(),
            skill_context_modifiers: Vec::new(),
            input: arguments,
        },
    )
    .await;
    absorb_native_model_shared_tool_report(outcome, &canonical_tool_name, &response.payload);
    let attachments =
        drain_native_model_async_attachments_after_tool(state, request, &response).await;
    NativeModelToolCallResult {
        tool_call_id: response.tool_use_id,
        tool_name: canonical_tool_name,
        status: response.status,
        is_error: response.is_error,
        content: truncate_tool_content(response.content),
        refs: response.refs,
        attachments,
    }
}

fn absorb_native_model_shared_tool_report(
    outcome: &mut NativeModelToolLoopOutcome,
    tool_name: &str,
    payload: &Value,
) {
    if !matches!(tool_name, "agent_subagent" | "read_subagent_status") {
        return;
    }
    let Some(report) = payload
        .get("report")
        .cloned()
        .and_then(|value| serde_json::from_value::<FinalReport>(value).ok())
    else {
        return;
    };
    outcome.changed_files.extend(report.changed_files);
    outcome.evidence_refs.extend(report.evidence_refs);
}

async fn drain_native_model_async_attachments_after_tool(
    state: &ApiState,
    request: &coder_harness::HarnessRunRequest,
    response: &crate::ModelToolExecuteResponse,
) -> Vec<Value> {
    let mut attachments = drain_native_model_async_attachments(state, request);
    if !attachments.is_empty() || !model_tool_response_requested_async_rewake(response) {
        return attachments;
    }
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        attachments = drain_native_model_async_attachments(state, request);
        if !attachments.is_empty() {
            break;
        }
    }
    attachments
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

fn model_tool_response_requested_async_rewake(response: &crate::ModelToolExecuteResponse) -> bool {
    response.phases.iter().any(|phase| {
        phase
            .get("hook_results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|hook_result| {
                hook_result
                    .get("async_rewake")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
    })
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
            "implementation": "native-model-file-write",
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

fn prepare_native_model_shared_tool_input(
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
    object
        .entry("repo_root".to_owned())
        .or_insert_with(|| Value::String(request.repo_root.clone()));
    object
        .entry("run_id".to_owned())
        .or_insert_with(|| Value::String(request.run_id.to_string()));
    object
        .entry("harness_id".to_owned())
        .or_insert_with(|| Value::String(request.harness_id.clone()));
    if tool_name != "agent_subagent" {
        object
            .entry("agent_id".to_owned())
            .or_insert_with(|| Value::String(request.agent_id.clone()));
    }
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
                .entry("approved".to_owned())
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
                .entry("approved".to_owned())
                .or_insert_with(|| Value::Bool(true));
            object
                .entry("backend_context".to_owned())
                .or_insert_with(|| request.backend_context.clone());
            object
                .entry("parent_agent_id".to_owned())
                .or_insert_with(|| Value::String(request.agent_id.clone()));
            object
                .entry("parent_harness_id".to_owned())
                .or_insert_with(|| Value::String(request.harness_id.clone()));
        }
        "task_stop" | "cancel_subagent_background" => {
            object
                .entry("approved".to_owned())
                .or_insert_with(|| Value::Bool(true));
        }
        _ => {}
    }
}

fn execute_native_model_write_text_file(
    state: &ApiState,
    request: &coder_harness::HarnessRunRequest,
    tool_call_id: String,
    arguments: Value,
    outcome: &mut NativeModelToolLoopOutcome,
) -> NativeModelToolCallResult {
    let Some(path) = native_tool_string(&arguments, &["path"]) else {
        return native_model_tool_error(tool_call_id, "write_text_file", "path is required");
    };
    let Some(content) = native_tool_content_string(&arguments, "content") else {
        return native_model_tool_error(tool_call_id, "write_text_file", "content is required");
    };
    if content.is_empty() {
        return native_model_tool_error(tool_call_id, "write_text_file", "content is empty");
    }
    match write_text_file(
        &request.repo_root,
        FileWriteRequest {
            path: PathBuf::from(&path),
            content,
            max_bytes: NATIVE_MODEL_MAX_FILE_BYTES,
            source: "model_tool_loop".to_owned(),
        },
    ) {
        Ok(evidence) => record_native_model_file_change(
            state,
            request,
            tool_call_id,
            "write_text_file",
            "full_write",
            evidence,
            outcome,
        ),
        Err(error) => native_model_tool_error(tool_call_id, "write_text_file", error.to_string()),
    }
}

fn execute_native_model_edit_text_file(
    state: &ApiState,
    request: &coder_harness::HarnessRunRequest,
    tool_call_id: String,
    arguments: Value,
    outcome: &mut NativeModelToolLoopOutcome,
) -> NativeModelToolCallResult {
    let Some(path) = native_tool_string(&arguments, &["path"]) else {
        return native_model_tool_error(tool_call_id, "edit_text_file", "path is required");
    };
    let edits = match native_model_file_edits(&arguments) {
        Ok(edits) => edits,
        Err(error) => {
            return native_model_tool_error(tool_call_id, "edit_text_file", error);
        }
    };
    let operation = if edits.len() > 1 {
        "exact_string_edit_batch"
    } else {
        "exact_string_edit"
    };
    match edit_text_file_batch(
        &request.repo_root,
        FileEditBatchRequest {
            path: PathBuf::from(path),
            edits,
            max_bytes: NATIVE_MODEL_MAX_FILE_BYTES,
            source: "model_tool_loop".to_owned(),
        },
    ) {
        Ok(evidence) => record_native_model_file_change(
            state,
            request,
            tool_call_id,
            "edit_text_file",
            operation,
            evidence,
            outcome,
        ),
        Err(error) => native_model_tool_error(tool_call_id, "edit_text_file", error.to_string()),
    }
}

fn native_model_file_edits(arguments: &Value) -> Result<Vec<FileEditReplacement>, &'static str> {
    if let Some(items) = arguments.get("edits") {
        let Some(items) = items.as_array() else {
            return Err("edits must be an array");
        };
        if items.is_empty() {
            return Err("edits must contain at least one edit");
        }
        if items.len() > NATIVE_MODEL_MAX_FILE_EDITS {
            return Err("edits exceeded the maximum of 32 items");
        }
        return items
            .iter()
            .map(|item| {
                let Some(old_string) = native_tool_content_string(item, "old_string") else {
                    return Err("each edit requires old_string");
                };
                let Some(new_string) = native_tool_content_string(item, "new_string") else {
                    return Err("each edit requires new_string");
                };
                Ok(FileEditReplacement {
                    old_string,
                    new_string,
                    replace_all: item
                        .get("replace_all")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
            })
            .collect();
    }
    let Some(old_string) = native_tool_content_string(arguments, "old_string") else {
        return Err("old_string is required when edits is omitted");
    };
    let Some(new_string) = native_tool_content_string(arguments, "new_string") else {
        return Err("new_string is required when edits is omitted");
    };
    Ok(vec![FileEditReplacement {
        old_string,
        new_string,
        replace_all: arguments
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }])
}

fn record_native_model_file_change(
    state: &ApiState,
    request: &coder_harness::HarnessRunRequest,
    tool_call_id: String,
    tool_name: &'static str,
    operation: &'static str,
    evidence: coder_tools::FileWriteEvidence,
    outcome: &mut NativeModelToolLoopOutcome,
) -> NativeModelToolCallResult {
    let evidence_ref = match write_file_evidence(state, &request.run_id, &evidence) {
        Ok(reference) => reference,
        Err(error) => return native_model_tool_error(tool_call_id, tool_name, error.to_string()),
    };
    if !outcome
        .changed_files
        .iter()
        .any(|file| file == &evidence.path)
    {
        outcome.changed_files.push(evidence.path.clone());
    }
    outcome.evidence_refs.push(repo_evidence_ref(&evidence_ref));
    outcome.events.push(
        HarnessRunEvent::new(
            "file.written",
            json!({
                "backend": "native-rust",
                "implementation": "native-model-file-write",
                "execution_mode": "tool_loop",
                "tool_call_id": &tool_call_id,
                "tool_name": tool_name,
                "operation": operation,
                "path": &evidence.path,
                "status": &evidence.status,
                "created": evidence.created,
                "bytes_written": evidence.bytes_written,
                "evidence_ref": &evidence_ref.ref_id
            }),
        )
        .with_ref(
            "repo_evidence",
            format!("repo-evidence://{}", evidence_ref.ref_id),
        ),
    );
    let payload = json!({
        "status": "completed",
        "tool": tool_name,
        "operation": operation,
        "result": evidence
    });
    native_model_tool_success(tool_call_id, tool_name, payload, Some(evidence_ref))
}

fn execute_native_model_finish(
    tool_call_id: String,
    arguments: Value,
    outcome: &mut NativeModelToolLoopOutcome,
) -> NativeModelToolCallResult {
    let status =
        native_tool_string(&arguments, &["status"]).unwrap_or_else(|| "completed".to_owned());
    outcome.status = if status == "blocked" {
        "blocked".to_owned()
    } else {
        "completed".to_owned()
    };
    if let Some(summary) = native_tool_string(&arguments, &["summary"]) {
        outcome.summary = summary;
    }
    outcome
        .checks
        .extend(native_tool_string_array(&arguments, "checks"));
    outcome
        .blockers
        .extend(native_tool_string_array(&arguments, "blockers"));
    let payload = json!({
        "status": outcome.status,
        "tool": "finish",
        "summary": &outcome.summary,
        "checks": &outcome.checks,
        "blockers": &outcome.blockers
    });
    native_model_tool_success(tool_call_id, "finish", payload, None)
}

fn native_model_tool_success(
    tool_call_id: impl Into<String>,
    tool_name: impl Into<String>,
    payload: Value,
    evidence_ref: Option<RepoEvidenceRef>,
) -> NativeModelToolCallResult {
    let tool_call_id = tool_call_id.into();
    let tool_name = tool_name.into();
    NativeModelToolCallResult {
        tool_call_id,
        tool_name,
        status: "completed".to_owned(),
        is_error: false,
        content: bounded_tool_content(&payload),
        refs: evidence_ref
            .map(|reference| {
                vec![HarnessRunEventRef {
                    label: "repo_evidence".to_owned(),
                    uri: format!("repo-evidence://{}", reference.ref_id),
                }]
            })
            .unwrap_or_default(),
        attachments: Vec::new(),
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
        attachments: Vec::new(),
    }
}

fn apply_native_model_final_content(
    outcome: &mut NativeModelToolLoopOutcome,
    content: Option<&str>,
) {
    let Some(content) = content.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    if let Ok(plan) = parse_native_model_plan(content) {
        if plan.status == "blocked" {
            outcome.status = "blocked".to_owned();
        }
        if !plan.summary.trim().is_empty() {
            outcome.summary = plan.summary;
        }
        outcome.checks.extend(plan.checks);
        outcome.blockers.extend(plan.blockers);
    } else if outcome.summary.trim().is_empty() {
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
            "implementation": "native-model-file-write",
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
    if outcome.status != "blocked"
        && outcome.changed_files.is_empty()
        && native_model_requires_file_writes(request)
    {
        outcome.status = "blocked".to_owned();
        outcome
            .blockers
            .push("native model tool loop produced no file writes".to_owned());
    }
    let mut changed_files = dedupe_strings(outcome.changed_files);
    let mut evidence_refs = outcome.evidence_refs;
    if outcome.status != "blocked" {
        if let Some(reference) =
            write_git_diff_evidence(state, &request.run_id, &request.repo_root)?
        {
            evidence_refs.push(repo_evidence_ref(&reference));
            events.push(
                HarnessRunEvent::new(
                    "git.diff.recorded",
                    json!({
                        "backend": "native-rust",
                        "implementation": "native-model-file-write",
                        "execution_mode": "tool_loop",
                        "evidence_ref": reference.ref_id
                    }),
                )
                .with_ref(
                    "repo_evidence",
                    format!("repo-evidence://{}", reference.ref_id),
                ),
            );
        }
    }

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
            "implementation": "native-model-file-write",
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

fn no_file_report_result(
    started: HarnessRunEvent,
    credential_source: String,
    provider: String,
    model_name: String,
    plan: NativeModelPlan,
) -> HarnessRunResult {
    let summary = if plan.summary.trim().is_empty() {
        "Native model subagent completed without file writes.".to_owned()
    } else {
        plan.summary
    };
    let mut report = FinalReport::completed(summary);
    report.checks = plan.checks;
    report.next_steps = plan.blockers;
    let events = vec![
        started,
        HarnessRunEvent::new(
            "executor.reasoning_summary",
            json!({
                "backend": "native-rust",
                "implementation": "native-model-file-write",
                "summary": "Provider returned a no-file report for a native subagent task.",
                "credential_source": credential_source,
                "provider": provider,
                "model": model_name
            }),
        ),
        HarnessRunEvent::new(
            "backend.native_rust.completed",
            json!({
                "backend": "native-rust",
                "implementation": "native-model-file-write",
                "status": "completed",
                "changed_files": [],
                "checks": &report.checks,
                "tool_call_count": 0
            }),
        ),
    ];
    HarnessRunResult {
        status: "completed".to_owned(),
        report: Some(report),
        events,
    }
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

fn native_tool_string(input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        input
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn native_tool_content_string(input: &Value, key: &str) -> Option<String> {
    input.get(key).and_then(Value::as_str).map(str::to_owned)
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

fn parse_native_model_plan(content: &str) -> Result<NativeModelPlan, serde_json::Error> {
    serde_json::from_str(content).or_else(|_| {
        let trimmed = content.trim();
        let start = trimmed.find('{').unwrap_or(0);
        let end = trimmed
            .rfind('}')
            .map(|index| index + 1)
            .unwrap_or(trimmed.len());
        serde_json::from_str(&trimmed[start..end])
    })
}

fn write_file_evidence(
    state: &ApiState,
    run_id: &RunId,
    evidence: &coder_tools::FileWriteEvidence,
) -> Result<RepoEvidenceRef, coder_store::StoreError> {
    state.store.write_repo_evidence(
        run_id,
        RepoEvidenceKind::RepoDiff,
        evidence.repo_root.clone(),
        vec![evidence.path.clone()],
        format!("Changed file '{}'.", evidence.path),
        json!({
            "evidence_kind": &evidence.evidence_kind,
            "operation": &evidence.evidence_kind,
            "files": [
                {
                    "path": &evidence.path,
                    "status": if evidence.created { "added" } else { "modified" }
                }
            ],
            "result": evidence
        }),
    )
}

fn write_git_diff_evidence(
    state: &ApiState,
    run_id: &RunId,
    repo_root: &str,
) -> Result<Option<RepoEvidenceRef>, coder_store::StoreError> {
    let Ok(diff) = git_diff(repo_root, coder_tools::DEFAULT_MAX_GIT_OUTPUT_BYTES) else {
        return Ok(None);
    };
    state
        .store
        .write_repo_evidence(
            run_id,
            RepoEvidenceKind::RepoDiff,
            diff.repo_root.clone(),
            Vec::new(),
            "Captured git diff after native model file writes.",
            json!({
                "evidence_kind": "repo_evidence",
                "operation": "git_diff",
                "diff": diff
            }),
        )
        .map(Some)
}

fn repo_evidence_ref(reference: &RepoEvidenceRef) -> EvidenceRef {
    EvidenceRef {
        kind: "repo_evidence".to_owned(),
        reference: format!("repo-evidence://{}", reference.ref_id),
    }
}

fn max_output_tokens_from_request(request: &coder_harness::HarnessRunRequest) -> u32 {
    request
        .backend_context
        .pointer("/coder/agent/runtime/max_output_tokens")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1024, 32_000) as u32)
        .unwrap_or(8_000)
}

fn native_model_max_turns_from_request(request: &coder_harness::HarnessRunRequest) -> usize {
    request
        .backend_context
        .pointer("/coder/agent/runtime/max_turns")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(NATIVE_MODEL_DEFAULT_MAX_TURNS)
}

fn native_model_max_output_recovery_attempts(request: &coder_harness::HarnessRunRequest) -> u8 {
    request
        .backend_context
        .pointer("/coder/agent/runtime/max_output_recovery_attempts")
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(AGENT_MAX_OUTPUT_RECOVERY_ATTEMPTS_DEFAULT)
}

fn native_model_response_timeout_ms(request: &coder_harness::HarnessRunRequest) -> u64 {
    request
        .backend_context
        .pointer("/coder/agent/runtime/stream_idle_timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(90_000)
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

fn native_model_requires_file_writes(request: &coder_harness::HarnessRunRequest) -> bool {
    !request_is_native_subagent(request)
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

fn native_model_should_handle(request: &coder_harness::HarnessRunRequest) -> bool {
    request
        .backend_context
        .pointer("/coder/harness/selected_tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools.iter().filter_map(Value::as_str).any(|tool| {
                matches!(
                    tool,
                    "patch_apply"
                        | "apply_patch"
                        | "apply_patch_sandbox"
                        | "patch_preview"
                        | "preview_patch"
                        | "command_run"
                        | "run_command"
                        | "run_command_sandbox"
                )
            })
        })
        .unwrap_or(false)
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
                    "implementation": "native-model-file-write",
                    "status": "blocked",
                    "reason": blocker
                }),
            ),
        ],
    }
}

fn failed_result(started: HarnessRunEvent, blocker: impl Into<String>) -> HarnessRunResult {
    let blocker = blocker.into();
    HarnessRunResult {
        status: "failed".to_owned(),
        report: Some(FinalReport::failed(
            "Native model executor failed before applying file writes.",
            blocker.clone(),
        )),
        events: vec![
            started,
            HarnessRunEvent::new(
                "backend.native_rust.failed",
                json!({
                    "backend": "native-rust",
                    "implementation": "native-model-file-write",
                    "status": "failed",
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
            false,
        );
        assert_eq!(deepseek["thinking"]["type"], "enabled");

        let generic = native_model_chat_completion_body(
            "openai-compatible",
            "reasoning-model",
            Vec::new(),
            256,
            &request,
            false,
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
            false,
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
        })
        .await
        .unwrap();
        let NativeModelProviderOutput::ToolLoop(outcome) = output else {
            panic!("exhausted budget must stop the tool loop");
        };
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
        assert!(prompt.contains("smallest clearly unique exact old_string"));
        assert!(prompt.contains("one edit_text_file call with the edits array"));
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

        let schema = native_model_tools_schema(&request);
        let names = schema
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();

        assert!(names.contains("repo_read_file"));
        assert!(names.contains("edit_text_file"));
        assert!(names.contains("write_text_file"));
        assert!(names.contains("task_stop"));
        assert!(names.contains("finish"));
        assert!(!names.contains("agent_subagent"));
        assert!(!names.contains("command_run"));
        let edit_tool = schema
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["function"]["name"] == "edit_text_file")
            .unwrap();
        assert_eq!(
            edit_tool["function"]["parameters"]["properties"]["edits"]["maxItems"],
            NATIVE_MODEL_MAX_FILE_EDITS
        );
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

        assert_eq!(
            native_model_max_turns_from_request(&request),
            NATIVE_MODEL_DEFAULT_MAX_TURNS
        );
        assert!(native_model_user_prompt(&request).contains("at most 24 provider turns"));

        request.backend_context["coder"]["agent"]["runtime"]["max_turns"] = json!(7);
        assert_eq!(native_model_max_turns_from_request(&request), 7);
        assert!(native_model_user_prompt(&request).contains("at most 7 provider turns"));

        assert_eq!(native_model_response_timeout_ms(&request), 90_000);
        request.backend_context["coder"]["agent"]["runtime"]["stream_idle_timeout_ms"] =
            json!(12_345);
        assert_eq!(native_model_response_timeout_ms(&request), 12_345);
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
            .contains("Use edit_text_file for a small exact change"));
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

    #[test]
    fn native_model_file_edits_accepts_batch_and_legacy_shapes() {
        let batch = native_model_file_edits(&json!({
            "path": "main.js",
            "edits": [
                {"old_string": "one", "new_string": "first"},
                {"old_string": "two", "new_string": "second", "replace_all": true}
            ]
        }))
        .unwrap();
        assert_eq!(batch.len(), 2);
        assert!(!batch[0].replace_all);
        assert!(batch[1].replace_all);

        let legacy = native_model_file_edits(&json!({
            "path": "main.js",
            "old_string": "one",
            "new_string": "first"
        }))
        .unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].new_string, "first");
    }
}
