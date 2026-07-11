use std::{collections::BTreeMap, collections::BTreeSet, env};

use coder_config::ModelSpec as ConfigModelSpec;
use coder_harness::HarnessRunRequest;
use reqwest::{Client, Proxy};

use crate::ProviderSettings;

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
    if let Some(request_url) = request_url {
        if provider_should_bypass_proxy(request_url, provider_no_proxy_from_env().as_deref()) {
            return None;
        }
    }
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
    let candidates = [
        format!("CODER_{}_PROXY_URL", provider_key),
        "CODER_PROVIDER_PROXY_URL".to_owned(),
        "https_proxy".to_owned(),
        "HTTPS_PROXY".to_owned(),
        "http_proxy".to_owned(),
        "HTTP_PROXY".to_owned(),
    ];
    for env_name in candidates {
        if let Some(value) = env::var_os(env_name).and_then(|value| value.into_string().ok()) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

fn provider_no_proxy_from_env() -> Option<String> {
    ["no_proxy", "NO_PROXY"].into_iter().find_map(|env_name| {
        env::var_os(env_name)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

pub(crate) fn provider_should_bypass_proxy(url: &str, no_proxy: Option<&str>) -> bool {
    let Some(no_proxy) = no_proxy.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    if no_proxy == "*" {
        return true;
    }
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let hostname = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    let port = parsed
        .port_or_known_default()
        .map(|port| port.to_string())
        .unwrap_or_default();
    let host_with_port = if port.is_empty() {
        hostname.clone()
    } else {
        format!("{hostname}:{port}")
    };
    no_proxy
        .split([',', ' ', '\t', '\n', '\r'])
        .filter_map(|entry| {
            let entry = entry.trim().to_ascii_lowercase();
            (!entry.is_empty()).then_some(entry)
        })
        .any(|pattern| {
            if pattern.contains(':') {
                return host_with_port == pattern;
            }
            if pattern.starts_with('.') {
                let suffix = pattern.as_str();
                return hostname == suffix.trim_start_matches('.') || hostname.ends_with(suffix);
            }
            hostname == pattern
        })
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
        .map(|secret| (secret.to_owned(), "settings".to_owned()))
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
    url: &str,
    proxy_url: Option<&str>,
) -> Result<reqwest::ClientBuilder, String> {
    if url.contains("://127.0.0.1") || url.contains("://localhost") || url.contains("://[::1]") {
        return Ok(Client::builder().no_proxy());
    }
    if let Some(proxy_url) = proxy_url.map(str::trim).filter(|value| !value.is_empty()) {
        let proxy = Proxy::all(proxy_url)
            .map_err(|error| format!("Provider proxy URL is invalid: {error}"))?;
        Ok(Client::builder().proxy(proxy))
    } else {
        Ok(Client::builder().no_proxy())
    }
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
    }
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
