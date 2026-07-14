use std::{collections::BTreeMap, collections::BTreeSet, env, fmt, time::Duration};

use coder_config::{
    resolve_agent_runtime_policy, AgentRuntimePolicy, ModelSpec as ConfigModelSpec,
    ResolvedAgentRuntimePolicy,
};
use coder_harness::HarnessRunRequest;
use reqwest::{RequestBuilder, Response, StatusCode};

use crate::outbound_http::{
    environment_proxy_route, ClientRouteClass, HttpClientFactory, OutboundProxyRoute,
};
use crate::ProviderSettings;

pub(crate) const PROVIDER_REQUEST_MAX_RETRIES: u64 = 4;
pub(crate) const PROVIDER_RETRY_BASE_DELAY_MS: u64 = 200;
pub(crate) const PROVIDER_STREAM_MAX_RETRIES: u64 = 5;
pub(crate) const PROVIDER_STREAM_IDLE_TIMEOUT_MS: u64 = 300_000;
pub(crate) const PROVIDER_WEBSOCKET_CONNECT_TIMEOUT_MS: u64 = 15_000;
const PROVIDER_MAX_RETRIES_LIMIT: u64 = 100;

#[derive(Debug)]
pub(crate) enum ProviderSendError {
    Timeout(Duration),
    Transport(reqwest::Error),
}

impl fmt::Display for ProviderSendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout(timeout) => write!(
                formatter,
                "provider returned no response data for {} ms",
                timeout.as_millis()
            ),
            Self::Transport(error) => error.fmt(formatter),
        }
    }
}

pub(crate) struct ProviderSendOutcome {
    pub response: Response,
    pub attempts: u32,
}

pub(crate) async fn send_provider_request_with_retry(
    make_request: impl FnMut() -> RequestBuilder,
    attempt_timeout: Option<Duration>,
    max_retries: u64,
) -> Result<ProviderSendOutcome, ProviderSendError> {
    send_provider_request_with_policy(
        make_request,
        attempt_timeout,
        max_retries.min(PROVIDER_MAX_RETRIES_LIMIT),
        Duration::from_millis(PROVIDER_RETRY_BASE_DELAY_MS),
    )
    .await
}

async fn send_provider_request_with_policy(
    mut make_request: impl FnMut() -> RequestBuilder,
    attempt_timeout: Option<Duration>,
    max_retries: u64,
    base_delay: Duration,
) -> Result<ProviderSendOutcome, ProviderSendError> {
    for attempt in 0..=max_retries {
        let result = if let Some(timeout) = attempt_timeout {
            match tokio::time::timeout(timeout, make_request().send()).await {
                Ok(result) => result.map_err(ProviderSendError::Transport),
                Err(_) => Err(ProviderSendError::Timeout(timeout)),
            }
        } else {
            make_request()
                .send()
                .await
                .map_err(ProviderSendError::Transport)
        };

        match result {
            Ok(response)
                if provider_status_is_retryable(response.status()) && attempt < max_retries =>
            {
                tokio::time::sleep(provider_retry_backoff(base_delay, attempt + 1)).await;
            }
            Ok(response) => {
                return Ok(ProviderSendOutcome {
                    response,
                    attempts: (attempt + 1) as u32,
                });
            }
            Err(error) if provider_send_error_is_retryable(&error) && attempt < max_retries => {
                tokio::time::sleep(provider_retry_backoff(base_delay, attempt + 1)).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("provider retry loop always returns on its final attempt")
}

fn provider_send_error_is_retryable(error: &ProviderSendError) -> bool {
    match error {
        ProviderSendError::Timeout(_) => true,
        ProviderSendError::Transport(error) => {
            error.is_timeout()
                || error.is_connect()
                || error.is_request() && !error.is_builder()
                || error.is_body()
        }
    }
}

fn provider_retry_backoff(base_delay: Duration, attempt: u64) -> Duration {
    let exponent = 2_u64.saturating_pow(attempt.saturating_sub(1) as u32);
    let raw_ms = (base_delay.as_millis() as u64).saturating_mul(exponent);
    let jitter_per_mille = fastrand::u64(900..1100);
    Duration::from_millis(raw_ms.saturating_mul(jitter_per_mille) / 1000)
}

pub(crate) fn provider_status_is_retryable(status: StatusCode) -> bool {
    status.is_server_error()
}

pub(crate) fn redact_provider_error(message: &str, secrets: &[&str]) -> String {
    let mut redacted = coder_events::redact_secret_text(message);
    for secret in secrets {
        let secret = secret.trim();
        if secret.len() >= 4 {
            redacted = redacted.replace(secret, "[REDACTED]");
        }
    }
    redacted
}

pub(crate) fn provider_base_url(settings: &ProviderSettings, provider: &str) -> Option<String> {
    if let Some(value) = settings_provider_base_url(settings, provider) {
        return Some(value);
    }
    provider_base_url_from_env(None)
        .or_else(|| default_provider_base_url(provider).map(str::to_owned))
}

fn settings_provider_base_url(settings: &ProviderSettings, provider: &str) -> Option<String> {
    settings.base_urls.get(provider).cloned()
}

pub(crate) fn provider_proxy_url_for_url(
    settings: &ProviderSettings,
    provider: &str,
    request_url: Option<&str>,
) -> Option<String> {
    match provider_proxy_mode(settings, provider).as_str() {
        "explicit" => settings.proxy_urls.get(provider).cloned(),
        "environment" => provider_proxy_url_from_env(provider, request_url),
        _ => None,
    }
}

fn provider_outbound_proxy_route(
    settings: &ProviderSettings,
    provider: &str,
    request_url: &str,
) -> OutboundProxyRoute {
    match provider_proxy_mode(settings, provider).as_str() {
        "explicit" => OutboundProxyRoute::from_optional_url(
            settings
                .proxy_urls
                .get(&normalize_provider(provider))
                .map(String::as_str),
        ),
        "environment" => environment_proxy_route(request_url, &provider_proxy_env_keys(provider))
            .outbound_route(),
        _ => OutboundProxyRoute::Direct,
    }
}

pub(crate) fn provider_proxy_mode(settings: &ProviderSettings, provider: &str) -> String {
    let provider = normalize_provider(provider);
    if let Some(mode) = settings
        .proxy_modes
        .get(&provider)
        .and_then(|mode| normalize_provider_proxy_mode(mode))
    {
        return mode;
    }
    if settings
        .proxy_urls
        .get(&provider)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        return "explicit".to_owned();
    }
    default_provider_proxy_mode(&provider).to_owned()
}

pub(crate) fn provider_request_max_retries(settings: &ProviderSettings, provider: &str) -> u64 {
    provider_network_settings(settings, provider)
        .and_then(|network| network.request_max_retries)
        .unwrap_or(PROVIDER_REQUEST_MAX_RETRIES)
        .min(PROVIDER_MAX_RETRIES_LIMIT)
}

pub(crate) fn provider_stream_max_retries(settings: &ProviderSettings, provider: &str) -> u64 {
    provider_network_settings(settings, provider)
        .and_then(|network| network.stream_max_retries)
        .unwrap_or(PROVIDER_STREAM_MAX_RETRIES)
        .min(PROVIDER_MAX_RETRIES_LIMIT)
}

pub(crate) fn provider_stream_idle_timeout_ms(settings: &ProviderSettings, provider: &str) -> u64 {
    provider_network_settings(settings, provider)
        .and_then(|network| network.stream_idle_timeout_ms)
        .unwrap_or(PROVIDER_STREAM_IDLE_TIMEOUT_MS)
}

pub(crate) fn provider_websocket_connect_timeout_ms(
    settings: &ProviderSettings,
    provider: &str,
) -> u64 {
    provider_network_settings(settings, provider)
        .and_then(|network| network.websocket_connect_timeout_ms)
        .unwrap_or(PROVIDER_WEBSOCKET_CONNECT_TIMEOUT_MS)
}

pub(crate) fn provider_supports_websockets(settings: &ProviderSettings, provider: &str) -> bool {
    provider_network_settings(settings, provider).is_some_and(|network| network.supports_websockets)
}

fn provider_network_settings<'a>(
    settings: &'a ProviderSettings,
    provider: &str,
) -> Option<&'a crate::ProviderNetworkSettings> {
    settings.network.get(&normalize_provider(provider))
}

fn default_provider_proxy_mode(provider: &str) -> &'static str {
    match normalize_provider(provider).as_str() {
        "deepseek" | "ollama" => "direct",
        _ => "environment",
    }
}

pub(crate) fn normalize_provider_proxy_mode(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "direct" | "none" | "off" => Some("direct".to_owned()),
        "explicit" | "proxy" | "configured" => Some("explicit".to_owned()),
        "environment" | "env" => Some("environment".to_owned()),
        _ => None,
    }
}

fn provider_proxy_url_from_env(provider: &str, request_url: Option<&str>) -> Option<String> {
    environment_proxy_route(
        request_url.unwrap_or_default(),
        &provider_proxy_env_keys(provider),
    )
    .proxy_url
}

fn provider_proxy_env_keys(provider: &str) -> Vec<String> {
    let provider_key = provider
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    vec![
        format!("CODER_{}_PROXY_URL", provider_key),
        "CODER_PROVIDER_PROXY_URL".to_owned(),
    ]
}

fn provider_base_url_from_env(model_base_url_env: Option<&str>) -> Option<String> {
    let candidates = [
        model_base_url_env,
        Some("CODER_BASE_URL"),
        Some("LLM_BASE_URL"),
    ];
    for env_name in candidates.into_iter().flatten() {
        if let Some(value) = env::var_os(env_name).and_then(|value| value.into_string().ok()) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

pub(crate) fn provider_api_key(
    settings: &ProviderSettings,
    provider: &str,
    model_api_key_env: Option<&str>,
) -> Option<(String, String)> {
    settings
        .api_keys
        .get(provider)
        .and_then(|state| state.secret.as_deref())
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
        .map(|secret| {
            let source = settings
                .api_keys
                .get(provider)
                .map(|state| state.source.clone())
                .unwrap_or_else(|| "settings".to_owned());
            (secret.to_owned(), source)
        })
        .or_else(|| {
            provider_api_key_from_env(provider, model_api_key_env)
                .map(|secret| (secret, "environment".to_owned()))
        })
}

pub(crate) fn provider_api_key_from_env(
    provider: &str,
    model_api_key_env: Option<&str>,
) -> Option<String> {
    let env_keys = provider_env_keys();
    let provider_env_name = env_keys
        .get(provider)
        .map(String::as_str)
        .unwrap_or("CODER_API_KEY");
    let candidates = [
        model_api_key_env,
        Some(provider_env_name),
        Some("CODER_API_KEY"),
        Some("LLM_API_KEY"),
    ];
    let mut seen = BTreeSet::new();
    for env_name in candidates.into_iter().flatten() {
        if !seen.insert(env_name.to_owned()) {
            continue;
        }
        if let Some(value) = env::var_os(env_name).and_then(|value| value.into_string().ok()) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

pub(crate) fn provider_env_keys() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("openai".to_owned(), "OPENAI_API_KEY".to_owned()),
        ("openai-compatible".to_owned(), "CODER_API_KEY".to_owned()),
        ("deepseek".to_owned(), "DEEPSEEK_API_KEY".to_owned()),
        ("moonshot".to_owned(), "MOONSHOT_API_KEY".to_owned()),
        ("kimi".to_owned(), "MOONSHOT_API_KEY".to_owned()),
        ("qwen".to_owned(), "DASHSCOPE_API_KEY".to_owned()),
        ("dashscope".to_owned(), "DASHSCOPE_API_KEY".to_owned()),
        ("groq".to_owned(), "GROQ_API_KEY".to_owned()),
        ("openrouter".to_owned(), "OPENROUTER_API_KEY".to_owned()),
        ("together".to_owned(), "TOGETHER_API_KEY".to_owned()),
        ("mistral".to_owned(), "MISTRAL_API_KEY".to_owned()),
        ("perplexity".to_owned(), "PERPLEXITY_API_KEY".to_owned()),
        ("xai".to_owned(), "XAI_API_KEY".to_owned()),
        ("gemini".to_owned(), "GEMINI_API_KEY".to_owned()),
        ("ollama".to_owned(), "OLLAMA_API_KEY".to_owned()),
    ])
}

fn default_provider_base_url(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some("https://api.openai.com/v1"),
        "deepseek" => Some("https://api.deepseek.com"),
        "moonshot" | "kimi" => Some("https://api.moonshot.cn/v1"),
        "qwen" | "dashscope" => Some("https://dashscope.aliyuncs.com/compatible-mode/v1"),
        "groq" => Some("https://api.groq.com/openai/v1"),
        "openrouter" => Some("https://openrouter.ai/api/v1"),
        "together" => Some("https://api.together.xyz/v1"),
        "mistral" => Some("https://api.mistral.ai/v1"),
        "perplexity" => Some("https://api.perplexity.ai"),
        "xai" => Some("https://api.x.ai/v1"),
        "gemini" => Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        "ollama" => Some("http://localhost:11434/v1"),
        _ => None,
    }
}

pub(crate) fn normalize_provider(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

pub(crate) fn normalize_provider_effort(effort: &str) -> Option<&'static str> {
    match effort.trim().to_ascii_lowercase().as_str() {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" => Some("max"),
        _ => None,
    }
}

pub(crate) fn provider_reasoning_effort(effort: Option<&str>) -> Option<&'static str> {
    match effort.and_then(normalize_provider_effort) {
        Some("low") => Some("low"),
        Some("medium") => Some("medium"),
        Some("high") => Some("high"),
        Some("xhigh") | Some("max") => Some("xhigh"),
        _ => None,
    }
}

pub(crate) fn provider_http_client_builder(
    settings: &ProviderSettings,
    provider: &str,
    url: &str,
) -> Result<reqwest::ClientBuilder, String> {
    HttpClientFactory::new(provider_outbound_proxy_route(settings, provider, url))
        .builder(url, ClientRouteClass::ProviderApi)
}

pub(crate) fn provider_chat_completions_endpoint(base_url: &str) -> String {
    let base_url = base_url.trim();
    if let Ok(mut url) = reqwest::Url::parse(base_url) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        let path = format!("{}/chat/completions", url.path().trim_end_matches('/'));
        url.set_path(&path);
        return url.to_string();
    }
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

pub(crate) fn provider_chat_completions_endpoint_for_display(base_url: &str) -> String {
    sanitize_provider_endpoint(&provider_chat_completions_endpoint(base_url))
}

pub(crate) fn sanitize_provider_endpoint(endpoint: &str) -> String {
    if let Ok(mut url) = reqwest::Url::parse(endpoint) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string();
    }
    endpoint
        .split('?')
        .next()
        .unwrap_or(endpoint)
        .split('#')
        .next()
        .unwrap_or(endpoint)
        .to_owned()
}

pub(crate) fn model_provider_for_settings(
    settings: &ProviderSettings,
    model: &ConfigModelSpec,
) -> String {
    if matches!(model.model.as_str(), "best" | "standard" | "economy")
        && !settings.default_provider.trim().is_empty()
    {
        normalize_provider(&settings.default_provider)
    } else {
        normalize_provider(&model.provider)
    }
}

pub(crate) fn harness_model_spec(request: &HarnessRunRequest) -> ConfigModelSpec {
    let model = request.backend_context.pointer("/coder/model");
    ConfigModelSpec {
        provider: model
            .and_then(|value| value.get("provider"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("deepseek")
            .to_owned(),
        model: model
            .and_then(|value| value.get("model"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("best")
            .to_owned(),
        base_url_env: model
            .and_then(|value| value.get("base_url_env"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        api_key_env: model
            .and_then(|value| value.get("api_key_env"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        capabilities: model
            .and_then(|value| value.get("capabilities"))
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default(),
    }
}

pub(crate) fn harness_agent_runtime(request: &HarnessRunRequest) -> ResolvedAgentRuntimePolicy {
    let runtime = request
        .backend_context
        .pointer("/coder/agent/runtime")
        .cloned()
        .and_then(|value| serde_json::from_value::<AgentRuntimePolicy>(value).ok())
        .unwrap_or_default();
    resolve_agent_runtime_policy(&harness_model_spec(request), &runtime)
}

pub(crate) fn model_name_for_settings(
    settings: &ProviderSettings,
    model: &ConfigModelSpec,
) -> String {
    if matches!(model.model.as_str(), "best" | "standard" | "economy") {
        settings.default_model.clone()
    } else {
        model.model.clone()
    }
}

pub(crate) fn model_provider_base_url(
    settings: &ProviderSettings,
    provider: &str,
    model: &ConfigModelSpec,
) -> Option<String> {
    settings_provider_base_url(settings, provider)
        .or_else(|| provider_base_url_from_env(model.base_url_env.as_deref()))
        .or_else(|| default_provider_base_url(provider).map(str::to_owned))
}

#[cfg(test)]
mod retry_tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use axum::{routing::post, Router};
    use reqwest::Client;

    use super::*;

    async fn spawn_status_server(
        statuses: Vec<StatusCode>,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let attempts = Arc::new(AtomicUsize::new(0));
        let handler_attempts = Arc::clone(&attempts);
        let statuses = Arc::new(statuses);
        let app = Router::new().route(
            "/",
            post(move || {
                let attempts = Arc::clone(&handler_attempts);
                let statuses = Arc::clone(&statuses);
                async move {
                    let index = attempts.fetch_add(1, Ordering::SeqCst);
                    statuses
                        .get(index)
                        .copied()
                        .or_else(|| statuses.last().copied())
                        .unwrap_or(StatusCode::OK)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}/"), attempts, task)
    }

    #[tokio::test]
    async fn provider_request_retries_5xx_then_returns_success() {
        let (url, attempts, server) = spawn_status_server(vec![
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::OK,
        ])
        .await;
        let client = Client::builder().no_proxy().build().unwrap();

        let outcome = send_provider_request_with_policy(
            || client.post(&url),
            None,
            PROVIDER_REQUEST_MAX_RETRIES,
            Duration::from_millis(1),
        )
        .await
        .unwrap();

        assert_eq!(outcome.response.status(), StatusCode::OK);
        assert_eq!(outcome.attempts, 3);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        server.abort();
    }

    #[tokio::test]
    async fn provider_request_does_not_retry_429() {
        let (url, attempts, server) =
            spawn_status_server(vec![StatusCode::TOO_MANY_REQUESTS, StatusCode::OK]).await;
        let client = Client::builder().no_proxy().build().unwrap();

        let outcome = send_provider_request_with_policy(
            || client.post(&url),
            None,
            PROVIDER_REQUEST_MAX_RETRIES,
            Duration::from_millis(1),
        )
        .await
        .unwrap();

        assert_eq!(outcome.response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(outcome.attempts, 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn provider_request_stops_after_four_retries() {
        let (url, attempts, server) =
            spawn_status_server(vec![StatusCode::SERVICE_UNAVAILABLE]).await;
        let client = Client::builder().no_proxy().build().unwrap();

        let outcome = send_provider_request_with_policy(
            || client.post(&url),
            None,
            PROVIDER_REQUEST_MAX_RETRIES,
            Duration::from_millis(1),
        )
        .await
        .unwrap();

        assert_eq!(outcome.response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(outcome.attempts, 5);
        assert_eq!(attempts.load(Ordering::SeqCst), 5);
        server.abort();
    }

    #[tokio::test]
    async fn provider_request_retries_transport_errors() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let client = Client::builder().no_proxy().build().unwrap();
        let attempts = AtomicUsize::new(0);

        let result = send_provider_request_with_policy(
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                client.post(&url)
            },
            None,
            2,
            Duration::from_millis(1),
        )
        .await;

        assert!(matches!(result, Err(ProviderSendError::Transport(_))));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn provider_retry_backoff_matches_codex_range() {
        for attempt in 1..=4 {
            let raw = PROVIDER_RETRY_BASE_DELAY_MS * 2_u64.pow(attempt - 1);
            let delay = provider_retry_backoff(
                Duration::from_millis(PROVIDER_RETRY_BASE_DELAY_MS),
                u64::from(attempt),
            )
            .as_millis() as u64;
            assert!(delay >= raw * 9 / 10);
            assert!(delay < raw * 11 / 10);
        }
    }
}
