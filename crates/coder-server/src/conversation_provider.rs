use std::time::Duration;

use coder_config::ModelSpec;
use coder_workflow::{OpenAiCompatibleStreamAdapter, ProviderStreamEventKind};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::api_types::ConversationSession;
use crate::provider_runtime::{
    model_name_for_settings, model_provider_base_url, model_provider_for_settings,
    provider_api_key, provider_chat_completions_endpoint, provider_http_client_builder,
    provider_request_max_retries, redact_provider_error, send_provider_request_with_retry,
};
use crate::{default_project_config, ApiError, ApiState};

const PROVIDER_HISTORY_TURNS: usize = 20;
const PROVIDER_RESPONSE_MAX_BYTES: usize = 2 * 1024 * 1024;
const PROVIDER_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(300);
const PROVIDER_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 1_400;
const SYSTEM_PROMPT: &str = "You are Coder. Respond directly to the user. This conversation has no command or file-editing tools, so never claim that you executed commands or changed files.";

pub(crate) async fn conversation_reply(
    state: &ApiState,
    session: &ConversationSession,
    on_delta: impl FnMut(&str),
) -> Result<String, ApiError> {
    let settings = state.provider_settings.lock().unwrap().clone();
    if settings.mock_mode {
        let message = session
            .turns
            .last()
            .map(|turn| turn.content.as_str())
            .unwrap_or_default();
        let reply = format!("Mock conversation response: {message}");
        let mut on_delta = on_delta;
        on_delta(&reply);
        return Ok(reply);
    }

    let config = default_project_config();
    let model = config
        .models
        .get("default")
        .or_else(|| config.models.values().next())
        .ok_or_else(|| ApiError::internal("default configuration has no model"))?;
    send_chat_completion(&settings, model, session, on_delta).await
}

async fn send_chat_completion(
    settings: &crate::ProviderSettings,
    model: &ModelSpec,
    session: &ConversationSession,
    mut on_delta: impl FnMut(&str),
) -> Result<String, ApiError> {
    let provider = model_provider_for_settings(settings, model);
    let model_name = model_name_for_settings(settings, model);
    let (api_key, _) = provider_api_key(settings, &provider, model.api_key_env.as_deref())
        .ok_or_else(|| ApiError::bad_request(format!("provider '{provider}' has no API key")))?;
    let base_url = model_provider_base_url(settings, &provider, model)
        .ok_or_else(|| ApiError::bad_request(format!("provider '{provider}' has no base URL")))?;
    let url = provider_chat_completions_endpoint(&base_url);
    let client = provider_http_client_builder(settings, &provider, &url)
        .map_err(ApiError::internal)?
        .connect_timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let max_output_tokens = model
        .capabilities
        .max_output_tokens
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
        .min(DEFAULT_MAX_OUTPUT_TOKENS);
    let mut body = json!({
        "model": model_name,
        "messages": provider_messages(session),
        "temperature": 0.3,
        "max_tokens": max_output_tokens,
        "stream": true
    });
    if provider == "deepseek" {
        body["thinking"] = json!({"type": "disabled"});
    }
    let redaction_values = [api_key.as_str(), base_url.as_str()];
    let outcome = send_provider_request_with_retry(
        || client.post(&url).bearer_auth(&api_key).json(&body),
        Some(PROVIDER_ATTEMPT_TIMEOUT),
        provider_request_max_retries(settings, &provider),
    )
    .await
    .map_err(|error| {
        ApiError::internal(redact_provider_error(&error.to_string(), &redaction_values))
    })?;
    let status = outcome.response.status();
    if !status.is_success() {
        let bytes = bounded_response_bytes(outcome.response).await?;
        let detail = redact_provider_error(&String::from_utf8_lossy(&bytes), &redaction_values);
        return Err(ApiError::internal(format!(
            "provider request failed with status {status}: {detail}"
        )));
    }

    let is_event_stream = outcome
        .response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"));
    if !is_event_stream {
        let bytes = bounded_response_bytes(outcome.response).await?;
        let reply = parse_non_streaming_reply(&bytes)?;
        on_delta(&reply);
        return Ok(reply);
    }

    let mut stream = outcome.response.bytes_stream().eventsource();
    let mut adapter = OpenAiCompatibleStreamAdapter::new();
    let mut saw_terminal = false;
    while !saw_terminal {
        let next = tokio::time::timeout(PROVIDER_STREAM_IDLE_TIMEOUT, stream.next())
            .await
            .map_err(|_| ApiError::internal("idle timeout waiting for provider stream"))?;
        let event = match next {
            Some(Ok(event)) => event,
            Some(Err(error)) => {
                return Err(ApiError::internal(redact_provider_error(
                    &error.to_string(),
                    &redaction_values,
                )))
            }
            None => break,
        };
        let data = event.data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }
        let chunk: Value = serde_json::from_str(data).map_err(|error| {
            ApiError::internal(format!("provider stream returned invalid JSON: {error}"))
        })?;
        for event in adapter.apply_chunk(&chunk) {
            match event.kind {
                ProviderStreamEventKind::ContentDelta => {
                    if let Some(delta) = event.content_delta.as_deref() {
                        on_delta(delta);
                    }
                }
                ProviderStreamEventKind::Finished => saw_terminal = true,
                ProviderStreamEventKind::MalformedModelOutput => {
                    if let Some(issue) = event.issue {
                        return Err(ApiError::internal(format!(
                            "provider stream was malformed: {}",
                            issue.message
                        )));
                    }
                }
                ProviderStreamEventKind::Aborted
                | ProviderStreamEventKind::Discarded
                | ProviderStreamEventKind::ToolCallDelta
                | ProviderStreamEventKind::ToolCallReady => {}
            }
        }
        if adapter.final_state().assistant_content.len() > PROVIDER_RESPONSE_MAX_BYTES {
            return Err(ApiError::internal(
                "provider response exceeded retention limit",
            ));
        }
    }
    let final_state = adapter.final_state();
    let reply = final_state.assistant_content.trim();
    if reply.is_empty() {
        return Err(ApiError::internal("provider returned no assistant message"));
    }
    if !saw_terminal && final_state.finish_reason.is_none() {
        return Err(ApiError::internal(
            "provider stream closed before a terminal event",
        ));
    }
    Ok(reply.to_owned())
}

async fn bounded_response_bytes(response: reqwest::Response) -> Result<Vec<u8>, ApiError> {
    if response.content_length().is_some_and(|length| {
        length > u64::try_from(PROVIDER_RESPONSE_MAX_BYTES).unwrap_or(u64::MAX)
    }) {
        return Err(ApiError::internal(
            "provider response exceeded retention limit",
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    if bytes.len() > PROVIDER_RESPONSE_MAX_BYTES {
        return Err(ApiError::internal(
            "provider response exceeded retention limit",
        ));
    }
    Ok(bytes.to_vec())
}

fn parse_non_streaming_reply(bytes: &[u8]) -> Result<String, ApiError> {
    let payload: Value = serde_json::from_slice(bytes)
        .map_err(|error| ApiError::internal(format!("provider returned invalid JSON: {error}")))?;
    payload
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ApiError::internal("provider returned no assistant message"))
}

fn provider_messages(session: &ConversationSession) -> Vec<Value> {
    let mut messages = vec![json!({"role": "system", "content": SYSTEM_PROMPT})];
    messages.extend(
        session
            .turns
            .iter()
            .rev()
            .take(PROVIDER_HISTORY_TURNS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|turn| json!({"role": turn.role, "content": turn.content})),
    );
    messages
}
