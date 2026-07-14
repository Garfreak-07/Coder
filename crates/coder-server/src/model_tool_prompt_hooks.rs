use coder_config::{HookCommandSpec, HookEvent, ModelSpec};
use coder_core::RunId;
use coder_workflow::TurnContext;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use crate::model_tool_hook_output::{bounded_hook_output_preview, ModelToolHookEffects};
use crate::model_tool_hook_phase::{hook_command_kind, hook_event_name, ModelToolHookContext};
use crate::model_tool_hook_runtime::ModelToolHookExecution;
use crate::provider_runtime::{
    model_provider_base_url, model_provider_for_settings, provider_api_key,
    provider_chat_completions_endpoint, provider_http_client_builder, provider_proxy_url_for_url,
    provider_request_max_retries, redact_provider_error, send_provider_request_with_retry,
};
use crate::run_token_budget::{
    check_existing_run_token_budget, provider_token_usage, record_existing_run_token_usage,
};
use crate::{ApiState, ProviderSettings};

pub(crate) const CLAUDE_PROMPT_HOOK_EXECUTION_TIMEOUT_SECONDS: u64 = 30;
pub(crate) const PROMPT_HOOK_MAX_OUTPUT_TOKENS: u32 = 256;

struct PromptHookReportContext<'a> {
    prompt: &'a str,
    event: HookEvent,
    requested_tool_name: &'a str,
    timeout_seconds: u64,
    started: Instant,
    provider: &'a str,
    model_name: &'a str,
    model_source: &'static str,
}

pub(crate) async fn execute_prompt_model_tool_hook(
    state: &ApiState,
    hook: &HookCommandSpec,
    event: HookEvent,
    requested_tool_name: &str,
    hook_input: &Value,
    host_context: &TurnContext,
    context: &ModelToolHookContext,
) -> ModelToolHookExecution {
    let HookCommandSpec::Prompt {
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
    let timeout_seconds = timeout.unwrap_or(CLAUDE_PROMPT_HOOK_EXECUTION_TIMEOUT_SECONDS);
    let provider_settings = state.provider_settings.lock().unwrap().clone();
    let (model_spec, model_source) =
        prompt_hook_model_spec(context, &provider_settings, model.as_deref());
    let provider = model_provider_for_settings(&provider_settings, &model_spec);
    let model_name = prompt_hook_model_name(&provider_settings, &model_spec);
    let report_context = PromptHookReportContext {
        prompt,
        event,
        requested_tool_name,
        timeout_seconds,
        started,
        provider: &provider,
        model_name: &model_name,
        model_source,
    };
    let base_url = match model_provider_base_url(&provider_settings, &provider, &model_spec) {
        Some(base_url) => base_url,
        None => {
            return prompt_hook_non_blocking_error(
                &report_context,
                "provider_base_url_missing",
                "Prompt hook provider base URL is not configured.",
                None,
            );
        }
    };
    let api_key = match provider_api_key(
        &provider_settings,
        &provider,
        model_spec.api_key_env.as_deref(),
    ) {
        Some((api_key, _source)) => api_key,
        None => {
            return prompt_hook_non_blocking_error(
                &report_context,
                "provider_api_key_missing",
                "Prompt hook provider API key is not configured.",
                None,
            );
        }
    };
    if provider_settings.mock_mode {
        return prompt_hook_non_blocking_error(
            &report_context,
            "provider_mock_mode",
            "Prompt hook live evaluation is skipped while provider mock mode is enabled.",
            None,
        );
    }
    let run_id = host_context.run_id.as_deref().map(RunId::from_string);
    if run_id
        .as_ref()
        .and_then(|run_id| check_existing_run_token_budget(state, run_id))
        .is_some_and(|budget| budget.exhausted())
    {
        let blocking_error =
            "Prompt hook was not evaluated because the workflow token budget was exhausted."
                .to_owned();
        return ModelToolHookExecution {
            payload: json!({
                "type": "prompt",
                "outcome": "blocking",
                "error_kind": "workflow_token_budget_exhausted",
                "blocking_error": blocking_error
            }),
            blocking_error: Some(blocking_error),
            effects: ModelToolHookEffects::default(),
        };
    }

    let url = provider_chat_completions_endpoint(&base_url);
    let proxy_url = provider_proxy_url_for_url(&provider_settings, &provider, Some(&url));
    let client = match provider_http_client_builder(&provider_settings, &provider, &url).and_then(
        |builder| {
            builder
                .timeout(Duration::from_secs(timeout_seconds))
                .build()
                .map_err(|error| error.to_string())
        },
    ) {
        Ok(client) => client,
        Err(error) => {
            let error = redact_provider_error(
                &error,
                &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
            );
            return prompt_hook_non_blocking_error(
                &report_context,
                "client_build_error",
                &error,
                None,
            );
        }
    };

    let processed_prompt = prompt_hook_prompt_with_arguments(prompt, hook_input);
    let request_body = prompt_hook_completion_body(&provider, &model_name, &processed_prompt);
    let response = match tokio::time::timeout(
        Duration::from_secs(timeout_seconds),
        send_provider_request_with_retry(
            || client.post(&url).bearer_auth(&api_key).json(&request_body),
            None,
            provider_request_max_retries(&provider_settings, &provider),
        ),
    )
    .await
    {
        Ok(Ok(outcome)) => outcome.response,
        Ok(Err(error)) => {
            let error = redact_provider_error(
                &format!("prompt hook model request failed: {error}"),
                &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
            );
            return prompt_hook_non_blocking_error(&report_context, "request_error", &error, None);
        }
        Err(_) => {
            return prompt_hook_non_blocking_error(
                &report_context,
                "request_timeout",
                &format!("Prompt hook timed out after {timeout_seconds} second(s)."),
                None,
            );
        }
    };
    let status_code = response.status().as_u16();
    if !response.status().is_success() {
        return prompt_hook_non_blocking_error(
            &report_context,
            "http_status_error",
            &format!("Prompt hook provider returned HTTP {}.", response.status()),
            Some(status_code),
        );
    }
    let payload = match response.json::<Value>().await {
        Ok(payload) => payload,
        Err(error) => {
            let error = redact_provider_error(
                &error.to_string(),
                &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
            );
            return prompt_hook_non_blocking_error(
                &report_context,
                "response_json_error",
                &error,
                Some(status_code),
            );
        }
    };
    let token_budget = run_id.as_ref().and_then(|run_id| {
        record_existing_run_token_usage(
            state,
            run_id,
            provider_token_usage(&request_body, &payload),
        )
    });
    let content = provider_assistant_content(&payload);
    let (output_preview, output_truncated) = bounded_hook_output_preview(&content);
    let parsed = match serde_json::from_str::<Value>(content.trim()) {
        Ok(value) => value,
        Err(error) => {
            return prompt_hook_non_blocking_error(
                &report_context,
                "invalid_prompt_hook_json",
                &format!("JSON validation failed: {error}"),
                Some(status_code),
            )
            .with_output_preview(output_preview, output_truncated);
        }
    };
    let Some(ok) = parsed.get("ok").and_then(Value::as_bool) else {
        return prompt_hook_non_blocking_error(
            &report_context,
            "invalid_prompt_hook_schema",
            "Prompt hook model response must include boolean field 'ok'.",
            Some(status_code),
        )
        .with_hook_json(parsed)
        .with_output_preview(output_preview, output_truncated);
    };
    let reason = parsed
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let blocking_error = if ok {
        None
    } else {
        Some(format!(
            "Prompt hook condition was not met: {}",
            if reason.is_empty() {
                "No reason provided"
            } else {
                reason.as_str()
            }
        ))
    };
    ModelToolHookExecution {
        payload: json!({
            "type": "prompt",
            "prompt": prompt,
            "hook_event": hook_event_name(event),
            "tool_name": requested_tool_name,
            "outcome": if blocking_error.is_some() { "blocking" } else { "success" },
            "provider": provider,
            "model": model_name,
            "model_source": model_source,
            "duration_ms": started.elapsed().as_millis() as u64,
            "timeout_seconds": timeout_seconds,
            "default_timeout_seconds": CLAUDE_PROMPT_HOOK_EXECUTION_TIMEOUT_SECONDS,
            "request_protocol": "claude.hook_input.v1",
            "prompt_argument_protocol": "claude.addArgumentsToPrompt",
            "hook_output_kind": "prompt_hook_json",
            "hook_json_output": parsed,
            "hook_output_validation_error": Value::Null,
            "blocking_error": blocking_error.clone(),
            "output_preview": output_preview,
            "output_truncated": output_truncated,
            "token_budget": token_budget.map(|budget| budget.as_json())
        }),
        blocking_error,
        effects: ModelToolHookEffects::default(),
    }
}

fn prompt_hook_non_blocking_error(
    context: &PromptHookReportContext<'_>,
    error_kind: &'static str,
    error: &str,
    status_code: Option<u16>,
) -> ModelToolHookExecution {
    let mut payload = json!({
        "type": "prompt",
        "prompt": context.prompt,
        "hook_event": hook_event_name(context.event),
        "tool_name": context.requested_tool_name,
        "outcome": "non_blocking_error",
        "provider": context.provider,
        "model": context.model_name,
        "model_source": context.model_source,
        "duration_ms": context.started.elapsed().as_millis() as u64,
        "timeout_seconds": context.timeout_seconds,
        "default_timeout_seconds": CLAUDE_PROMPT_HOOK_EXECUTION_TIMEOUT_SECONDS,
        "request_protocol": "claude.hook_input.v1",
        "prompt_argument_protocol": "claude.addArgumentsToPrompt",
        "hook_output_kind": error_kind,
        "hook_json_output": Value::Null,
        "hook_output_validation_error": error
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

pub(crate) fn prompt_hook_model_spec(
    context: &ModelToolHookContext,
    provider_settings: &ProviderSettings,
    hook_model: Option<&str>,
) -> (ModelSpec, &'static str) {
    if let Some(model) = hook_model.map(str::trim).filter(|model| !model.is_empty()) {
        if let Some(spec) = context.models.get(model) {
            return (spec.clone(), "hook_config_model");
        }
        return (
            ModelSpec {
                provider: provider_settings.default_provider.clone(),
                model: model.to_owned(),
                base_url_env: None,
                api_key_env: None,
                capabilities: coder_config::ModelCapabilities::default(),
            },
            "hook_literal_model",
        );
    }
    if let Some(spec) = context.models.get("default") {
        return (spec.clone(), "config_default_model");
    }
    (
        ModelSpec {
            provider: provider_settings.default_provider.clone(),
            model: "best".to_owned(),
            base_url_env: None,
            api_key_env: None,
            capabilities: coder_config::ModelCapabilities::default(),
        },
        "provider_default_model",
    )
}

pub(crate) fn prompt_hook_model_name(
    provider_settings: &ProviderSettings,
    model: &ModelSpec,
) -> String {
    if matches!(model.model.as_str(), "best" | "standard" | "economy")
        && !provider_settings.default_model.trim().is_empty()
    {
        provider_settings.default_model.clone()
    } else {
        model.model.clone()
    }
}

pub(crate) fn prompt_hook_prompt_with_arguments(prompt: &str, hook_input: &Value) -> String {
    let arguments = hook_input.to_string();
    let replaced = prompt.replace("$ARGUMENTS", &arguments);
    if replaced == prompt && !arguments.is_empty() {
        format!("{prompt}\n\nARGUMENTS: {arguments}")
    } else {
        replaced
    }
}

fn prompt_hook_completion_body(provider: &str, model_name: &str, prompt: &str) -> Value {
    let schema = json!({
        "type": "object",
        "properties": {
            "ok": { "type": "boolean" },
            "reason": { "type": "string" }
        },
        "required": ["ok"],
        "additionalProperties": false
    });
    let mut body = json!({
        "model": model_name,
        "messages": [
            {
                "role": "system",
                "content": "You are evaluating a hook in Claude Code.\n\nYour response must be a JSON object matching one of the following schemas:\n1. If the condition is met, return: {\"ok\": true}\n2. If the condition is not met, return: {\"ok\": false, \"reason\": \"Reason for why it is not met\"}"
            },
            {
                "role": "user",
                "content": prompt
            }
        ],
        "temperature": 0,
        "max_tokens": PROMPT_HOOK_MAX_OUTPUT_TOKENS
    });
    if provider == "deepseek" {
        body["thinking"] = json!({"type": "disabled"});
        body["response_format"] = json!({"type": "json_object"});
    } else {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {
                "name": "hook_response",
                "strict": true,
                "schema": schema
            }
        });
    }
    body
}

fn provider_assistant_content(payload: &Value) -> String {
    let Some(content) = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
    else {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use coder_config::{HookSettings, ModelSpec};

    use super::*;

    fn hook_context(models: BTreeMap<String, ModelSpec>) -> ModelToolHookContext {
        ModelToolHookContext {
            source: "test",
            disable_all_hooks: false,
            hooks: HookSettings::default(),
            models,
            allowed_webhook_urls: None,
            webhook_allowed_env_vars: None,
        }
    }

    #[test]
    fn unspecified_hook_model_does_not_select_an_unrelated_config_entry() {
        let unrelated = ModelSpec {
            provider: "deepseek".to_owned(),
            model: "unrelated-model".to_owned(),
            base_url_env: None,
            api_key_env: None,
            capabilities: coder_config::ModelCapabilities::default(),
        };
        let context = hook_context(BTreeMap::from([("unrelated".to_owned(), unrelated)]));
        let settings = ProviderSettings::default();

        let (model, source) = prompt_hook_model_spec(&context, &settings, None);

        assert_eq!(source, "provider_default_model");
        assert_eq!(model.model, "best");
    }
}
