use std::time::Duration;

use async_trait::async_trait;
use coder_config::ModelSpec as ConfigModelSpec;
use reqwest::Client;
use serde_json::{json, Value};

use crate::api_types::{
    PlannerConversationEngine, PlannerConversationRequest, PlannerConversationResponse,
    PlannerProviderTrace, ProviderSettings,
};
use crate::planner_conversation::{
    deterministic_planner_response, live_message_was_length_truncated,
    planner_provider_setup_required_response, planner_provider_unavailable_response,
    planner_system_prompt, DeterministicPlannerConversationEngine,
};
use crate::planner_history::{compact_planner_history, CompactedPlannerHistory};
use crate::planner_provider_recovery::{
    planner_provider_error_is_prompt_too_long, read_planner_provider_error_body,
    PlannerPromptTooLongError, PlannerProviderRequestMode,
    PLANNER_PROMPT_OVERFLOW_RECOVERY_ATTEMPTS,
};
use crate::planner_provider_runtime::{
    merge_planner_provider_trace, parse_live_planner_response_with_idle_timeout,
    planner_chat_completion_body, planner_chat_completion_streaming_body, planner_provider_trace,
    planner_runtime_effort, planner_streaming_fallback_status, LivePlannerMessage,
};
use crate::provider_runtime::{
    model_provider_base_url, model_provider_for_settings, provider_api_key,
    provider_chat_completions_endpoint, provider_http_client_builder, provider_proxy_url_for_url,
    redact_provider_error,
};

#[derive(Debug, Clone, Default)]
pub(crate) struct ModelPlannerConversationEngine {
    fallback: DeterministicPlannerConversationEngine,
}

#[derive(Debug, Clone)]
enum LivePlannerProviderOutcome {
    Message(Option<LivePlannerMessage>),
    PromptTooLong {
        error: PlannerPromptTooLongError,
        provider_trace: PlannerProviderTrace,
    },
}

struct LivePlannerProviderContext<'a> {
    client: &'a Client,
    url: &'a str,
    api_key: &'a str,
    provider: &'a str,
    model_name: &'a str,
    adapter: &'a NativePlannerContextAdapter,
    request: &'a PlannerConversationRequest,
    max_output_tokens: u32,
    effort: Option<&'a str>,
    stream_idle_timeout: Duration,
    redaction_values: &'a [&'a str],
}

impl ModelPlannerConversationEngine {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn live_assistant_message(
        &self,
        request: &PlannerConversationRequest,
    ) -> Result<Option<LivePlannerMessage>, String> {
        if request.provider_settings.mock_mode {
            return Ok(None);
        }
        let model = planner_model_profile(request);
        let provider = planner_model_provider(request, model);
        let (api_key, _) = provider_api_key(
            &request.provider_settings,
            &provider,
            model.api_key_env.as_deref(),
        )
        .ok_or_else(planner_model_config_error)?;
        let base_url = planner_model_base_url(request, &provider, model)
            .ok_or_else(planner_model_config_error)?;
        let url = provider_chat_completions_endpoint(&base_url);
        let model_name = planner_model_name(request, model);
        let proxy_url =
            provider_proxy_url_for_url(&request.provider_settings, &provider, Some(&url));
        let adapter = NativePlannerContextAdapter::new();
        let client = provider_http_client_builder(&url, proxy_url.as_deref())
            .map_err(|error| {
                redact_provider_error(
                    &error,
                    &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
                )
            })?
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(|error| {
                redact_provider_error(
                    &error.to_string(),
                    &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
                )
            })?;
        let max_output_tokens = request
            .runtime
            .agent
            .runtime
            .max_output_tokens
            .unwrap_or(crate::PLANNER_CHAT_MAX_OUTPUT_TOKENS_DEFAULT);
        let redaction_values = [
            api_key.as_str(),
            base_url.as_str(),
            proxy_url.as_deref().unwrap_or(""),
        ];
        let max_recovery_attempts = request.runtime.agent.runtime.max_output_recovery_attempts;
        let effort = planner_runtime_effort(&request.runtime.agent.runtime);
        let provider_context = LivePlannerProviderContext {
            client: &client,
            url: &url,
            api_key: &api_key,
            provider: &provider,
            model_name: &model_name,
            adapter: &adapter,
            request,
            max_output_tokens,
            effort: effort.as_deref(),
            stream_idle_timeout: Duration::from_millis(
                request.runtime.agent.runtime.stream_idle_timeout_ms,
            ),
            redaction_values: &redaction_values,
        };
        let mut recovered_assistant_messages = Vec::new();
        let mut accumulated_provider_trace: Option<PlannerProviderTrace> = None;
        let mut request_mode = PlannerProviderRequestMode::Normal;
        let mut prompt_overflow_recovery_attempts = 0u8;

        loop {
            let outcome = send_live_planner_provider_message(
                &provider_context,
                &recovered_assistant_messages,
                request_mode,
            )
            .await?;
            let Some(mut message) = (match outcome {
                LivePlannerProviderOutcome::Message(message) => message,
                LivePlannerProviderOutcome::PromptTooLong {
                    error,
                    provider_trace,
                } => {
                    accumulate_planner_provider_trace(
                        &mut accumulated_provider_trace,
                        provider_trace,
                    );
                    if prompt_overflow_recovery_attempts < PLANNER_PROMPT_OVERFLOW_RECOVERY_ATTEMPTS
                    {
                        prompt_overflow_recovery_attempts += 1;
                        request_mode = PlannerProviderRequestMode::PromptOverflowRecovery;
                        continue;
                    }
                    return Err(format!(
                        "planner model prompt is too long after compact retry (HTTP {}): {}",
                        error.status, error.message
                    ));
                }
            }) else {
                return Ok(None);
            };

            if let Some(mut accumulated) = accumulated_provider_trace.take() {
                merge_planner_provider_trace(&mut accumulated, message.provider_trace);
                message.provider_trace = accumulated;
            }

            if live_message_was_length_truncated(Some(&message))
                && (recovered_assistant_messages.len() as u8) < max_recovery_attempts
            {
                accumulated_provider_trace = Some(message.provider_trace.clone());
                recovered_assistant_messages.push(message.content);
                continue;
            }

            if recovered_assistant_messages.is_empty() {
                return Ok(Some(message));
            }

            let mut content_parts = recovered_assistant_messages;
            content_parts.push(message.content);
            let mut provider_trace = message.provider_trace;
            provider_trace.finish_reason = message.finish_reason.clone();
            return Ok(Some(LivePlannerMessage {
                content: content_parts.join("\n\n"),
                finish_reason: message.finish_reason,
                provider_trace,
            }));
        }
    }
}

fn accumulate_planner_provider_trace(
    accumulated: &mut Option<PlannerProviderTrace>,
    current: PlannerProviderTrace,
) {
    if let Some(accumulated) = accumulated {
        merge_planner_provider_trace(accumulated, current);
    } else {
        *accumulated = Some(current);
    }
}

#[async_trait]
impl PlannerConversationEngine for ModelPlannerConversationEngine {
    async fn respond(
        &self,
        request: PlannerConversationRequest,
    ) -> Result<PlannerConversationResponse, String> {
        let model_message = match self.live_assistant_message(&request).await {
            Ok(message) => message,
            Err(error) if is_planner_model_config_error(&error) => {
                return Ok(planner_provider_setup_required_response(error));
            }
            Err(error) => return Ok(planner_provider_unavailable_response(&request, &error)),
        };
        if model_message.is_some() {
            return Ok(deterministic_planner_response(&request, model_message));
        }
        self.fallback.respond(request).await
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct NativePlannerContextAdapter;

impl NativePlannerContextAdapter {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn provider_messages(
        &self,
        request: &PlannerConversationRequest,
        mode: PlannerProviderRequestMode,
    ) -> Vec<Value> {
        self.message_events(request, mode)
            .into_iter()
            .map(|event| {
                json!({
                    "role": event.get("role").and_then(Value::as_str).unwrap_or("user"),
                    "content": planner_event_text(&event)
                })
            })
            .collect()
    }

    pub(crate) fn message_events(
        &self,
        request: &PlannerConversationRequest,
        mode: PlannerProviderRequestMode,
    ) -> Vec<Value> {
        let context = self.context_payload(request, mode);
        let context_text = serde_json::to_string(&context).unwrap_or_else(|_| "{}".to_owned());
        let mut events = vec![planner_message_event(
            "system",
            &format!(
                "{}\n\nNative Coder planner context follows. It is redacted and tool-disabled; use it for session/context shape only, not execution.\n{}",
                planner_system_prompt(&request.runtime),
                context_text
            ),
        )];
        let CompactedPlannerHistory {
            summary,
            recent_turns,
            ..
        } = compact_planner_history(
            &request.history,
            crate::PLANNER_CHAT_HISTORY_RECENT_TURN_LIMIT,
        );
        if let Some(summary) = summary {
            events.push(planner_message_event("user", &summary));
        }
        for turn in recent_turns {
            let role = if turn.role == "assistant" {
                "assistant"
            } else {
                "user"
            };
            events.push(planner_message_event(role, &turn.content));
        }
        events.push(planner_message_event("user", &request.message));
        events
    }

    pub(crate) fn context_payload(
        &self,
        request: &PlannerConversationRequest,
        mode: PlannerProviderRequestMode,
    ) -> Value {
        if mode == PlannerProviderRequestMode::PromptOverflowRecovery {
            let planner_history_compaction = compact_planner_history(
                &request.history,
                crate::PLANNER_CHAT_HISTORY_RECENT_TURN_LIMIT,
            )
            .report;
            return json!({
                "adapter": "native-planner-context",
                "contract": "coder.planner_chat.prompt_overflow_recovery.v1",
                "prompt_overflow_recovery": true,
                "recovery_reason": "provider_prompt_too_long",
                "strategy": "minimal_native_planner_context",
                "session_id": &request.session_id,
                "workflow_id": &request.workflow_id,
                "mode": &request.mode,
                "current_plan": &request.current_plan,
                "history_compaction": planner_history_compaction,
                "strict_output_contract": planner_strict_output_contract(),
                "omitted": {
                    "large_runtime_context": true,
                    "reason": "previous provider attempt reported prompt/context overflow"
                },
                "side_effect_free": true,
                "execution_requires": "Start Work -> native executor"
            });
        }

        let planner_history_compaction = compact_planner_history(
            &request.history,
            crate::PLANNER_CHAT_HISTORY_RECENT_TURN_LIMIT,
        )
        .report;
        let plan_context = json!({
            "contract": "coder.planner_chat.request.v1",
            "session_id": &request.session_id,
            "mode": &request.mode,
            "current_plan": &request.current_plan,
            "history_compaction": planner_history_compaction,
            "strict_output_contract": planner_strict_output_contract(),
            "side_effect_free": true,
            "execution_requires": "Start Work -> native executor"
        });
        json!({
            "adapter": "native-planner-context",
            "contract": "coder.native_planner_context.v1",
            "reason": "Planner Chat must not run tools or create executor-side conversations.",
            "runtime": {
                "workflow_id": &request.workflow_id,
                "workflow_name": &request.runtime.workflow_name,
                "node_id": &request.runtime.node_id,
                "agent_id": &request.runtime.agent_id,
                "harness_id": &request.runtime.harness_id,
                "model": {
                    "provider": &request.runtime.model.provider,
                    "model": &request.runtime.model.model
                }
            },
            "planner_tool_policy": {
                "tools": [],
                "terminal": false,
                "file_editor": false,
                "command_execution": false,
                "network_tools": false
            },
            "planner_context": plan_context
        })
    }
}

fn planner_strict_output_contract() -> Value {
    json!({
        "assistant_message": "concise user-facing natural language",
        "ready_for_start_work": "boolean",
        "plan_draft": {
            "goal": "string",
            "scope": "string[]",
            "non_goals": "string[]",
            "assumptions": "string[]",
            "steps": "string[]",
            "affected_paths": "string[]; empty when repository inspection is required",
            "acceptance_criteria": "observable string[] covering every material goal/scope behavior",
            "risks": "string[]",
            "open_questions": "only scope or safety blockers; [] when the user delegated optional decisions"
        }
    })
}

fn planner_message_event(role: &str, text: &str) -> Value {
    json!({
        "role": role,
        "content": [
            {
                "type": "text",
                "text": text,
                "cache_prompt": false
            }
        ],
        "run": false,
        "source": "coder-planner"
    })
}

pub(crate) fn planner_event_text(event: &Value) -> String {
    event
        .get("content")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
            })
        })
        .or_else(|| event.get("message").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

fn planner_provider_messages_with_output_recovery(
    adapter: &NativePlannerContextAdapter,
    request: &PlannerConversationRequest,
    recovered_assistant_messages: &[String],
    mode: PlannerProviderRequestMode,
) -> Vec<Value> {
    let mut messages = adapter.provider_messages(request, mode);
    for assistant_message in recovered_assistant_messages {
        messages.push(json!({
            "role": "assistant",
            "content": assistant_message
        }));
        messages.push(json!({
            "role": "user",
            "content": crate::PLANNER_MAX_OUTPUT_RECOVERY_MESSAGE
        }));
    }
    messages
}

async fn send_live_planner_provider_message(
    context: &LivePlannerProviderContext<'_>,
    recovered_assistant_messages: &[String],
    mode: PlannerProviderRequestMode,
) -> Result<LivePlannerProviderOutcome, String> {
    let messages = planner_provider_messages_with_output_recovery(
        context.adapter,
        context.request,
        recovered_assistant_messages,
        mode,
    );
    let request_body = planner_chat_completion_streaming_body(
        context.provider,
        context.model_name,
        messages,
        context.max_output_tokens,
        context.effort,
    );
    let streaming_estimated_input_tokens =
        u64::from(crate::estimate_text_tokens(&request_body.to_string()));
    let response = send_planner_chat_completion_request(
        context.client,
        context.url,
        context.api_key,
        request_body,
        context.stream_idle_timeout,
        context.redaction_values,
    )
    .await?;
    if !response.status().is_success() {
        let status = response.status();
        let error_body =
            read_planner_provider_error_body(response, context.redaction_values).await?;
        if planner_provider_error_is_prompt_too_long(status, &error_body.raw) {
            let mut provider_trace = planner_provider_trace(true, "error", false, None);
            provider_trace.estimated_input_tokens = streaming_estimated_input_tokens;
            return Ok(LivePlannerProviderOutcome::PromptTooLong {
                error: PlannerPromptTooLongError {
                    status,
                    message: error_body.redacted,
                },
                provider_trace,
            });
        }
        if planner_streaming_fallback_status(status.as_u16()) {
            let fallback_messages = planner_provider_messages_with_output_recovery(
                context.adapter,
                context.request,
                recovered_assistant_messages,
                mode,
            );
            let fallback_body = planner_chat_completion_body(
                context.provider,
                context.model_name,
                fallback_messages,
                context.max_output_tokens,
                context.effort,
            );
            let fallback_estimated_input_tokens =
                u64::from(crate::estimate_text_tokens(&fallback_body.to_string()));
            let fallback_response = send_planner_chat_completion_request(
                context.client,
                context.url,
                context.api_key,
                fallback_body,
                context.stream_idle_timeout,
                context.redaction_values,
            )
            .await?;
            if !fallback_response.status().is_success() {
                let fallback_status = fallback_response.status();
                let fallback_error_body =
                    read_planner_provider_error_body(fallback_response, context.redaction_values)
                        .await?;
                if planner_provider_error_is_prompt_too_long(
                    fallback_status,
                    &fallback_error_body.raw,
                ) {
                    let mut provider_trace =
                        planner_provider_trace(true, "error", true, Some(status.as_u16()));
                    provider_trace.provider_turns = 2;
                    provider_trace.estimated_input_tokens = streaming_estimated_input_tokens
                        .saturating_add(fallback_estimated_input_tokens);
                    return Ok(LivePlannerProviderOutcome::PromptTooLong {
                        error: PlannerPromptTooLongError {
                            status: fallback_status,
                            message: fallback_error_body.redacted,
                        },
                        provider_trace,
                    });
                }
                return Err(format!(
                    "planner model returned HTTP {fallback_status} after streaming fallback"
                ));
            }
            let mut provider_trace =
                planner_provider_trace(true, "unknown", true, Some(status.as_u16()));
            provider_trace.provider_turns = 2;
            provider_trace.estimated_input_tokens =
                streaming_estimated_input_tokens.saturating_add(fallback_estimated_input_tokens);
            return parse_live_planner_response_with_idle_timeout(
                fallback_response,
                context.redaction_values,
                provider_trace,
                context.stream_idle_timeout,
            )
            .await
            .map(LivePlannerProviderOutcome::Message);
        }
        return Err(format!("planner model returned HTTP {status}"));
    }
    let mut provider_trace = planner_provider_trace(true, "unknown", false, None);
    provider_trace.estimated_input_tokens = streaming_estimated_input_tokens;
    parse_live_planner_response_with_idle_timeout(
        response,
        context.redaction_values,
        provider_trace,
        context.stream_idle_timeout,
    )
    .await
    .map(LivePlannerProviderOutcome::Message)
}

async fn send_planner_chat_completion_request(
    client: &Client,
    url: &str,
    api_key: &str,
    request_body: Value,
    idle_timeout: Duration,
    redaction_values: &[&str],
) -> Result<reqwest::Response, String> {
    tokio::time::timeout(
        idle_timeout,
        client
            .post(url)
            .bearer_auth(api_key)
            .json(&request_body)
            .send(),
    )
    .await
    .map_err(|_| {
        format!(
            "planner provider returned no response data for {} ms",
            idle_timeout.as_millis()
        )
    })?
    .map_err(|error| {
        redact_provider_error(
            &format!("planner model request failed: {error}"),
            redaction_values,
        )
    })
}

fn planner_model_profile(request: &PlannerConversationRequest) -> &ConfigModelSpec {
    &request.runtime.model
}

fn planner_model_name(request: &PlannerConversationRequest, model: &ConfigModelSpec) -> String {
    if matches!(model.model.as_str(), "best" | "standard" | "economy") {
        request.provider_settings.default_model.clone()
    } else {
        model.model.clone()
    }
}

fn planner_model_provider(request: &PlannerConversationRequest, model: &ConfigModelSpec) -> String {
    model_provider_for_settings(&request.provider_settings, model)
}

fn planner_model_base_url(
    request: &PlannerConversationRequest,
    provider: &str,
    model: &ConfigModelSpec,
) -> Option<String> {
    model_provider_base_url(&request.provider_settings, provider, model)
}

pub(crate) fn model_provider_config_error(
    settings: &ProviderSettings,
    model: &ConfigModelSpec,
) -> Option<String> {
    if settings.mock_mode {
        return None;
    }
    let provider = model_provider_for_settings(settings, model);
    if model_provider_base_url(settings, &provider, model).is_none() {
        return Some(planner_model_config_error());
    }
    if provider != "ollama"
        && provider_api_key(settings, &provider, model.api_key_env.as_deref()).is_none()
    {
        return Some(planner_model_config_error());
    }
    None
}

const PLANNER_MODEL_CONFIG_ERROR: &str =
    "Configure a provider in Settings before I can plan or execute work.";

fn planner_model_config_error() -> String {
    PLANNER_MODEL_CONFIG_ERROR.to_owned()
}

fn is_planner_model_config_error(error: &str) -> bool {
    error == PLANNER_MODEL_CONFIG_ERROR
}
