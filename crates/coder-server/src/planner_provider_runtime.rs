use std::time::Duration;

use coder_config::AgentRuntimePolicy;
use coder_workflow::{OpenAiCompatibleStreamAdapter, ProviderStreamEventKind};
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::api_types::PlannerProviderTrace;
use crate::provider_runtime::{
    normalize_provider, normalize_provider_effort, provider_reasoning_effort, redact_provider_error,
};

#[derive(Debug, Clone)]
pub(crate) struct LivePlannerMessage {
    pub(crate) content: String,
    pub(crate) finish_reason: Option<String>,
    pub(crate) provider_trace: PlannerProviderTrace,
}

pub(crate) async fn parse_live_planner_response_with_idle_timeout(
    response: reqwest::Response,
    redaction_values: &[&str],
    mut provider_trace: PlannerProviderTrace,
    idle_timeout: Duration,
) -> Result<Option<LivePlannerMessage>, String> {
    if provider_response_is_event_stream(&response) {
        provider_trace.response_transport = "event_stream".to_owned();
        parse_live_planner_streaming_response(
            response,
            redaction_values,
            provider_trace,
            idle_timeout,
        )
        .await
    } else {
        provider_trace.response_transport = "json".to_owned();
        let bytes = tokio::time::timeout(idle_timeout, response.bytes())
            .await
            .map_err(|_| planner_idle_timeout_error(idle_timeout))?
            .map_err(|error| redact_provider_error(&error.to_string(), redaction_values))?;
        if bytes.len() > crate::PLANNER_PROVIDER_RESPONSE_MAX_BYTES {
            return Err(format!(
                "planner model response exceeded {} byte retention limit",
                crate::PLANNER_PROVIDER_RESPONSE_MAX_BYTES
            ));
        }
        let payload: Value = serde_json::from_slice(&bytes)
            .map_err(|error| redact_provider_error(&error.to_string(), redaction_values))?;
        apply_planner_provider_usage(&mut provider_trace, payload.get("usage"));
        Ok(live_planner_message_from_payload(&payload, provider_trace))
    }
}

fn provider_response_is_event_stream(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("text/event-stream")
        })
        .unwrap_or(false)
}

async fn parse_live_planner_streaming_response(
    response: reqwest::Response,
    redaction_values: &[&str],
    mut provider_trace: PlannerProviderTrace,
    idle_timeout: Duration,
) -> Result<Option<LivePlannerMessage>, String> {
    let mut stream = response.bytes_stream();
    let mut adapter = OpenAiCompatibleStreamAdapter::new();
    let mut pending = String::new();
    loop {
        let Some(chunk) = tokio::time::timeout(idle_timeout, stream.next())
            .await
            .map_err(|_| planner_idle_timeout_error(idle_timeout))?
        else {
            break;
        };
        let chunk = chunk.map_err(|error| {
            redact_provider_error(
                &format!("planner streaming response failed: {error}"),
                redaction_values,
            )
        })?;
        pending.push_str(&String::from_utf8_lossy(&chunk));
        if pending.len() > crate::PLANNER_PROVIDER_STREAM_PENDING_MAX_BYTES {
            return Err(format!(
                "planner streaming response pending line exceeded {} byte retention limit",
                crate::PLANNER_PROVIDER_STREAM_PENDING_MAX_BYTES
            ));
        }
        while let Some(line_end) = pending.find('\n') {
            let line = pending[..line_end].to_owned();
            pending = pending[line_end + 1..].to_owned();
            apply_openai_compatible_stream_line(&mut adapter, &line, &mut provider_trace)?;
        }
    }
    if !pending.trim().is_empty() {
        apply_openai_compatible_stream_line(&mut adapter, &pending, &mut provider_trace)?;
    }
    let final_state = adapter.final_state();
    if let Some(issue) = final_state.issues.first() {
        return Err(format!(
            "planner streaming response contained malformed model output ({}): {}",
            issue.code, issue.message
        ));
    }
    let content = final_state.assistant_content.trim();
    if content.is_empty() {
        Ok(None)
    } else {
        provider_trace.finish_reason = final_state.finish_reason.clone();
        provider_trace.estimated_output_tokens = crate::estimate_text_tokens(content).into();
        Ok(Some(LivePlannerMessage {
            content: content.to_owned(),
            finish_reason: final_state.finish_reason,
            provider_trace,
        }))
    }
}

fn planner_idle_timeout_error(idle_timeout: Duration) -> String {
    format!(
        "planner provider stream received no data for {} ms",
        idle_timeout.as_millis()
    )
}

fn apply_openai_compatible_stream_line(
    adapter: &mut OpenAiCompatibleStreamAdapter,
    line: &str,
    provider_trace: &mut PlannerProviderTrace,
) -> Result<(), String> {
    let line = line.trim_end_matches('\r').trim();
    if line.is_empty() || line.starts_with(':') {
        return Ok(());
    }
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(());
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }
    let chunk: Value = serde_json::from_str(data)
        .map_err(|error| format!("planner streaming response chunk was not valid JSON: {error}"))?;
    apply_planner_provider_usage(provider_trace, chunk.get("usage"));
    if chunk
        .get("choices")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
        && chunk.get("usage").is_some_and(Value::is_object)
    {
        return Ok(());
    }
    let events = adapter.apply_chunk(&chunk);
    if events
        .iter()
        .any(|event| event.kind == ProviderStreamEventKind::MalformedModelOutput)
    {
        let issue = events
            .iter()
            .find_map(|event| event.issue.as_ref())
            .map(|issue| format!("{}: {}", issue.code, issue.message))
            .unwrap_or_else(|| "malformed model output".to_owned());
        return Err(format!("planner streaming response contained {issue}"));
    }
    Ok(())
}

fn live_planner_message_from_payload(
    payload: &Value,
    mut provider_trace: PlannerProviderTrace,
) -> Option<LivePlannerMessage> {
    let choice = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .cloned();
    let finish_reason = choice
        .as_ref()
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    provider_trace.finish_reason = finish_reason.clone();
    choice
        .as_ref()
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(|content| LivePlannerMessage {
            content: content.to_owned(),
            finish_reason,
            provider_trace: PlannerProviderTrace {
                estimated_output_tokens: crate::estimate_text_tokens(content).into(),
                ..provider_trace
            },
        })
}

pub(crate) fn planner_provider_trace(
    requested_stream: bool,
    response_transport: impl Into<String>,
    streaming_fallback: bool,
    fallback_status: Option<u16>,
) -> PlannerProviderTrace {
    PlannerProviderTrace {
        requested_stream,
        response_transport: response_transport.into(),
        streaming_fallback,
        fallback_status,
        finish_reason: None,
        provider_turns: 1,
        estimated_input_tokens: 0,
        estimated_output_tokens: 0,
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        cache_read_tokens: None,
        usage_reported: false,
    }
}

pub(crate) fn merge_planner_provider_trace(
    accumulated: &mut PlannerProviderTrace,
    current: PlannerProviderTrace,
) {
    accumulated.requested_stream |= current.requested_stream;
    accumulated.response_transport = current.response_transport;
    accumulated.streaming_fallback |= current.streaming_fallback;
    if current.fallback_status.is_some() {
        accumulated.fallback_status = current.fallback_status;
    }
    accumulated.finish_reason = current.finish_reason;
    accumulated.provider_turns = accumulated
        .provider_turns
        .saturating_add(current.provider_turns);
    accumulated.estimated_input_tokens = accumulated
        .estimated_input_tokens
        .saturating_add(current.estimated_input_tokens);
    accumulated.estimated_output_tokens = accumulated
        .estimated_output_tokens
        .saturating_add(current.estimated_output_tokens);
    accumulated.input_tokens = sum_optional_tokens(accumulated.input_tokens, current.input_tokens);
    accumulated.output_tokens =
        sum_optional_tokens(accumulated.output_tokens, current.output_tokens);
    accumulated.total_tokens = sum_optional_tokens(accumulated.total_tokens, current.total_tokens);
    accumulated.cache_read_tokens =
        sum_optional_tokens(accumulated.cache_read_tokens, current.cache_read_tokens);
    accumulated.usage_reported |= current.usage_reported;
}

fn apply_planner_provider_usage(trace: &mut PlannerProviderTrace, usage: Option<&Value>) {
    let Some(usage) = usage.filter(|value| value.is_object()) else {
        return;
    };
    trace.input_tokens = provider_usage_u64(usage, &["prompt_tokens", "input_tokens"]);
    trace.output_tokens = provider_usage_u64(usage, &["completion_tokens", "output_tokens"]);
    trace.total_tokens = provider_usage_u64(usage, &["total_tokens"]);
    trace.cache_read_tokens = provider_usage_u64(
        usage,
        &[
            "prompt_cache_hit_tokens",
            "cache_read_input_tokens",
            "cached_tokens",
        ],
    )
    .or_else(|| {
        usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
    });
    trace.usage_reported = true;
}

fn provider_usage_u64(usage: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| usage.get(key).and_then(Value::as_u64))
}

fn sum_optional_tokens(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

pub(crate) fn planner_streaming_fallback_status(status: u16) -> bool {
    matches!(status, 400 | 404 | 405 | 406 | 415 | 422)
}

pub(crate) fn planner_chat_completion_body(
    provider: &str,
    model_name: &str,
    messages: Vec<Value>,
    max_output_tokens: u32,
    effort: Option<&str>,
) -> Value {
    let mut body = json!({
        "model": model_name,
        "messages": messages,
        "temperature": 0.2,
        "max_tokens": max_output_tokens
    });
    let reasoning_effort = provider_reasoning_effort(effort);
    if normalize_provider(provider) == "deepseek" {
        body["response_format"] = json!({"type": "json_object"});
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

pub(crate) fn planner_chat_completion_streaming_body(
    provider: &str,
    model_name: &str,
    messages: Vec<Value>,
    max_output_tokens: u32,
    effort: Option<&str>,
) -> Value {
    let mut body =
        planner_chat_completion_body(provider, model_name, messages, max_output_tokens, effort);
    body["stream"] = json!(true);
    body["stream_options"] = json!({"include_usage": true});
    body
}

pub(crate) fn planner_runtime_effort(runtime: &AgentRuntimePolicy) -> Option<String> {
    runtime
        .effort
        .as_deref()
        .and_then(normalize_provider_effort)
        .map(str::to_owned)
}
