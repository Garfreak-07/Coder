use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use axum::{extract::State, Json};
use coder_config::ProjectConfig;
use coder_store::RunStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::credential_store::{KeyringStore, PROVIDER_KEYRING_SERVICE};
use crate::provider_runtime::{
    normalize_provider, provider_api_key, provider_api_key_from_env, provider_base_url,
    provider_chat_completions_endpoint, provider_chat_completions_endpoint_for_display,
    provider_env_keys, provider_http_client_builder, provider_proxy_mode,
    provider_proxy_url_for_url, provider_request_max_retries, provider_stream_idle_timeout_ms,
    provider_stream_max_retries, provider_supports_websockets,
    provider_websocket_connect_timeout_ms, redact_provider_error, sanitize_provider_endpoint,
    send_provider_request_with_retry,
};
use crate::{
    ApiError, ApiState, ProviderKeyState, ProviderNetworkSettings, ProviderSettings,
    ProviderSettingsPatch, ProviderSettingsResponse, ProviderSettingsSaveResponse, ProviderStatus,
    ProviderStatusItem, ProviderTestRequest, ProviderTestResponse, ProviderTestResult,
};

const PROVIDER_SETTINGS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct StoredProviderSettings {
    schema_version: u32,
    default_provider: String,
    default_model: String,
    base_urls: BTreeMap<String, String>,
    proxy_urls: BTreeMap<String, String>,
    proxy_modes: BTreeMap<String, String>,
    network: BTreeMap<String, ProviderNetworkSettings>,
    configured_providers: BTreeSet<String>,
    mock_mode: bool,
}

impl StoredProviderSettings {
    fn from_runtime(settings: &ProviderSettings) -> Self {
        Self {
            schema_version: PROVIDER_SETTINGS_SCHEMA_VERSION,
            default_provider: settings.default_provider.clone(),
            default_model: settings.default_model.clone(),
            base_urls: settings.base_urls.clone(),
            proxy_urls: settings.proxy_urls.clone(),
            proxy_modes: settings.proxy_modes.clone(),
            network: settings.network.clone(),
            configured_providers: settings
                .api_keys
                .iter()
                .filter(|(_, state)| {
                    state.configured
                        && !state
                            .secret
                            .as_deref()
                            .unwrap_or_default()
                            .trim()
                            .is_empty()
                })
                .map(|(provider, _)| provider.clone())
                .collect(),
            mock_mode: settings.mock_mode,
        }
    }

    fn into_runtime(self, credential_store: &dyn KeyringStore) -> ProviderSettings {
        if self.schema_version != PROVIDER_SETTINGS_SCHEMA_VERSION {
            return ProviderSettings::default();
        }
        let mut settings = ProviderSettings::default();
        apply_provider_settings_patch(
            &mut settings,
            ProviderSettingsPatch {
                default_provider: Some(self.default_provider),
                default_model: Some(self.default_model),
                base_urls: Some(self.base_urls),
                proxy_urls: Some(self.proxy_urls),
                proxy_modes: Some(self.proxy_modes),
                network: Some(self.network),
                api_keys: None,
                mock_mode: Some(self.mock_mode),
            },
        );
        for provider in self.configured_providers {
            let provider = normalize_provider(&provider);
            if provider.is_empty() {
                continue;
            }
            let Ok(Some(secret)) = credential_store.load(PROVIDER_KEYRING_SERVICE, &provider)
            else {
                continue;
            };
            let secret = secret.trim();
            if secret.is_empty() {
                continue;
            }
            settings.api_keys.insert(
                provider,
                ProviderKeyState {
                    configured: true,
                    source: "keyring".to_owned(),
                    secret: Some(secret.to_owned()),
                },
            );
        }
        settings
    }
}

pub(crate) fn load_provider_settings(
    store: &RunStore,
    credential_store: &dyn KeyringStore,
) -> ProviderSettings {
    store
        .read_provider_settings::<StoredProviderSettings>()
        .ok()
        .flatten()
        .map(|settings| settings.into_runtime(credential_store))
        .unwrap_or_default()
}

pub(crate) async fn get_provider_settings(
    State(state): State<ApiState>,
) -> Json<ProviderSettingsResponse> {
    Json(ProviderSettingsResponse {
        settings: state.provider_settings.lock().unwrap().clone(),
    })
}

pub(crate) async fn save_provider_settings(
    State(state): State<ApiState>,
    Json(request): Json<ProviderSettingsPatch>,
) -> Result<Json<ProviderSettingsSaveResponse>, ApiError> {
    let mut current = state.provider_settings.lock().unwrap();
    let mut candidate = current.clone();
    apply_provider_settings_patch(&mut candidate, request);
    for key_state in candidate.api_keys.values_mut() {
        if !key_state
            .secret
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            key_state.configured = true;
            key_state.source = "keyring".to_owned();
        }
    }
    let changes = credential_changes(&current, &candidate);
    apply_credential_changes(state.credential_store.as_ref(), &changes)?;
    if let Err(error) = state
        .store
        .write_provider_settings(&StoredProviderSettings::from_runtime(&candidate))
    {
        rollback_credential_changes(state.credential_store.as_ref(), &changes);
        return Err(ApiError::internal(format!(
            "failed to persist provider settings: {error}"
        )));
    }
    *current = candidate.clone();
    let status = provider_status(&candidate, None);
    Ok(Json(ProviderSettingsSaveResponse {
        settings: candidate,
        status,
    }))
}

#[derive(Debug)]
struct CredentialChange {
    provider: String,
    previous: Option<String>,
    next: Option<String>,
}

fn credential_changes(
    current: &ProviderSettings,
    candidate: &ProviderSettings,
) -> Vec<CredentialChange> {
    let providers = current
        .api_keys
        .keys()
        .chain(candidate.api_keys.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    providers
        .into_iter()
        .filter_map(|provider| {
            let previous = provider_secret(current, &provider);
            let next = provider_secret(candidate, &provider);
            (previous != next).then_some(CredentialChange {
                provider,
                previous,
                next,
            })
        })
        .collect()
}

fn provider_secret(settings: &ProviderSettings, provider: &str) -> Option<String> {
    settings
        .api_keys
        .get(provider)
        .and_then(|state| state.secret.as_deref())
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
        .map(str::to_owned)
}

fn apply_credential_changes(
    store: &dyn KeyringStore,
    changes: &[CredentialChange],
) -> Result<(), ApiError> {
    for (index, change) in changes.iter().enumerate() {
        if let Err(error) = write_credential(store, &change.provider, change.next.as_deref()) {
            rollback_credential_changes(store, &changes[..index]);
            return Err(ApiError::internal(format!(
                "failed to update provider credential for '{}': {error}",
                change.provider
            )));
        }
    }
    Ok(())
}

fn rollback_credential_changes(store: &dyn KeyringStore, changes: &[CredentialChange]) {
    for change in changes.iter().rev() {
        let _ = write_credential(store, &change.provider, change.previous.as_deref());
    }
}

fn write_credential(
    store: &dyn KeyringStore,
    provider: &str,
    secret: Option<&str>,
) -> Result<(), crate::credential_store::CredentialStoreError> {
    match secret {
        Some(secret) => store.save(PROVIDER_KEYRING_SERVICE, provider, secret),
        None => store.delete(PROVIDER_KEYRING_SERVICE, provider).map(|_| ()),
    }
}

pub(crate) async fn get_provider_status(State(state): State<ApiState>) -> Json<ProviderStatus> {
    Json(provider_status(
        &state.provider_settings.lock().unwrap(),
        None,
    ))
}

pub(crate) async fn test_provider_status(
    State(state): State<ApiState>,
    Json(request): Json<ProviderTestRequest>,
) -> Json<ProviderTestResponse> {
    let settings = state.provider_settings.lock().unwrap().clone();
    let provider = request
        .provider
        .as_deref()
        .map(normalize_provider)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| settings.default_provider.clone());
    let test = test_provider_chat_completion(&settings, &provider, request.mock.unwrap_or(false))
        .await
        .unwrap_or_else(|message| ProviderTestResult {
            provider: provider.clone(),
            ok: false,
            mode: "live".to_owned(),
            model: settings.default_model.clone(),
            endpoint: provider_base_url(&settings, &provider)
                .map(|base_url| provider_chat_completions_endpoint_for_display(&base_url)),
            message,
        });
    Json(ProviderTestResponse {
        status: provider_status(&settings, Some(vec![provider])),
        test,
    })
}

pub(crate) fn apply_provider_settings_patch(
    settings: &mut ProviderSettings,
    patch: ProviderSettingsPatch,
) {
    if let Some(provider) = patch.default_provider {
        let provider = normalize_provider(&provider);
        if !provider.is_empty() {
            settings.default_provider = provider;
        }
    }
    if let Some(model) = patch.default_model {
        let model = model.trim();
        if !model.is_empty() {
            settings.default_model = model.to_owned();
        }
    }
    if let Some(mock_mode) = patch.mock_mode {
        settings.mock_mode = mock_mode;
    }
    if let Some(base_urls) = patch.base_urls {
        settings.base_urls = clean_provider_string_map(base_urls);
    }
    if let Some(proxy_urls) = patch.proxy_urls {
        settings.proxy_urls = clean_provider_string_map(proxy_urls);
    }
    if let Some(proxy_modes) = patch.proxy_modes {
        settings.proxy_modes = clean_provider_proxy_mode_map(proxy_modes);
    }
    if let Some(network) = patch.network {
        settings.network = network
            .into_iter()
            .filter_map(|(provider, network)| {
                let provider = normalize_provider(&provider);
                (!provider.is_empty()).then_some((provider, network))
            })
            .collect();
    }
    if let Some(api_keys) = patch.api_keys {
        for (provider, value) in api_keys {
            let provider = normalize_provider(&provider);
            if provider.is_empty() {
                continue;
            }
            if value.is_null() {
                settings.api_keys.remove(&provider);
                continue;
            }
            let text = value.as_str().map(str::trim).unwrap_or_default();
            if text.is_empty() || text.chars().all(|ch| ch == '*') {
                continue;
            }
            settings.api_keys.insert(
                provider,
                ProviderKeyState {
                    configured: true,
                    source: "settings".to_owned(),
                    secret: Some(text.to_owned()),
                },
            );
        }
    }
}

pub(crate) fn apply_provider_settings_to_project_config(
    config: &mut ProjectConfig,
    settings: &ProviderSettings,
) {
    if settings.mock_mode {
        return;
    }
    let provider = normalize_provider(&settings.default_provider);
    let model = settings.default_model.trim();
    if provider.is_empty() || model.is_empty() {
        return;
    }
    for model_spec in config
        .models
        .values_mut()
        .filter(|model_spec| provider_settings_should_resolve_model_alias(&model_spec.model))
    {
        model_spec.provider = provider.clone();
        model_spec.model = model.to_owned();
    }
}

fn provider_settings_should_resolve_model_alias(model: &str) -> bool {
    matches!(model.trim(), "" | "best" | "standard" | "economy")
}

pub(crate) fn provider_status(
    settings: &ProviderSettings,
    providers: Option<Vec<String>>,
) -> ProviderStatus {
    let selected = providers.unwrap_or_else(|| {
        let mut names = provider_env_keys().keys().cloned().collect::<BTreeSet<_>>();
        names.insert(settings.default_provider.clone());
        names.extend(settings.api_keys.keys().cloned());
        names.extend(settings.proxy_urls.keys().cloned());
        names.extend(settings.proxy_modes.keys().cloned());
        names.extend(settings.network.keys().cloned());
        names.into_iter().collect()
    });
    let providers = selected
        .into_iter()
        .map(|provider| provider_status_item(settings, &normalize_provider(&provider)))
        .collect::<Vec<_>>();
    ProviderStatus {
        default_provider: settings.default_provider.clone(),
        default_model: settings.default_model.clone(),
        mock_mode: settings.mock_mode,
        default_status: provider_status_item(settings, &settings.default_provider),
        providers,
    }
}

fn provider_status_item(settings: &ProviderSettings, provider: &str) -> ProviderStatusItem {
    let provider = if provider.trim().is_empty() {
        "openai"
    } else {
        provider.trim()
    };
    let (credential_configured, credential_source) = provider_credential_state(settings, provider);
    let base_url = provider_base_url(settings, provider);
    let request_url = base_url.as_deref().map(provider_chat_completions_endpoint);
    let proxy_mode = provider_proxy_mode(settings, provider);
    let proxy_url = provider_proxy_url_for_url(settings, provider, request_url.as_deref())
        .map(|proxy_url| sanitize_provider_endpoint(&proxy_url));
    let configured = provider == "ollama" || credential_configured || settings.mock_mode;
    ProviderStatusItem {
        provider: provider.to_owned(),
        configured,
        credential_configured: provider == "ollama" || credential_configured,
        credential_source: if provider == "ollama" {
            "ollama".to_owned()
        } else {
            credential_source
        },
        base_url,
        proxy_url,
        proxy_mode,
        request_max_retries: provider_request_max_retries(settings, provider),
        stream_max_retries: provider_stream_max_retries(settings, provider),
        stream_idle_timeout_ms: provider_stream_idle_timeout_ms(settings, provider),
        websocket_connect_timeout_ms: provider_websocket_connect_timeout_ms(settings, provider),
        supports_websockets: provider_supports_websockets(settings, provider),
        mode: if settings.mock_mode && !credential_configured && provider != "ollama" {
            "mock"
        } else {
            "live"
        }
        .to_owned(),
    }
}

fn provider_credential_state(settings: &ProviderSettings, provider: &str) -> (bool, String) {
    if let Some(state) = settings.api_keys.get(provider).filter(|state| {
        state.configured && !state.secret.as_deref().unwrap_or("").trim().is_empty()
    }) {
        return (true, state.source.clone());
    }
    if provider_api_key_from_env(provider, None).is_some() {
        return (true, "environment".to_owned());
    }
    (false, "missing".to_owned())
}

async fn test_provider_chat_completion(
    settings: &ProviderSettings,
    provider: &str,
    mock: bool,
) -> Result<ProviderTestResult, String> {
    let provider = normalize_provider(provider);
    let model = settings.default_model.clone();
    if mock {
        return Ok(ProviderTestResult {
            provider,
            ok: true,
            mode: "mock".to_owned(),
            model,
            endpoint: None,
            message: "Mock provider test passed without a live request.".to_owned(),
        });
    }
    let status = provider_status_item(settings, &provider);
    if settings.mock_mode && !status.credential_configured {
        return Ok(ProviderTestResult {
            provider,
            ok: true,
            mode: "mock".to_owned(),
            model,
            endpoint: None,
            message: "Mock mode is enabled; no live provider request was sent.".to_owned(),
        });
    }
    let (api_key, source) = provider_api_key(settings, &provider, None).ok_or_else(|| {
        "Provider test requires an API key from Provider Settings or developer/headless environment fallback."
            .to_owned()
    })?;
    let base_url = provider_base_url(settings, &provider)
        .ok_or_else(|| "Provider test requires a base URL.".to_owned())?;
    let url = provider_chat_completions_endpoint(&base_url);
    let endpoint = provider_chat_completions_endpoint_for_display(&base_url);
    let proxy_url = provider_proxy_url_for_url(settings, &provider, Some(&url));
    let client = provider_http_client_builder(settings, &provider, &url)
        .map_err(|error| {
            redact_provider_error(
                &error,
                &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
            )
        })?
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| {
            redact_provider_error(
                &error.to_string(),
                &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
            )
        })?;
    let request_body = provider_test_chat_completion_body(&provider, &settings.default_model);
    let response = send_provider_request_with_retry(
        || client.post(&url).bearer_auth(&api_key).json(&request_body),
        None,
        provider_request_max_retries(settings, &provider),
    )
    .await
    .map_err(|error| {
        redact_provider_error(
            &format!("Provider test request failed: {}", error),
            &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
        )
    })?
    .response;
    if !response.status().is_success() {
        return Ok(ProviderTestResult {
            provider,
            ok: false,
            mode: "live".to_owned(),
            model,
            endpoint: Some(endpoint),
            message: format!("Provider returned HTTP {}.", response.status()),
        });
    }
    let payload: Value = response.json().await.map_err(|error| {
        redact_provider_error(
            &error.to_string(),
            &[&api_key, &base_url, proxy_url.as_deref().unwrap_or("")],
        )
    })?;
    let content = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if content.is_empty() {
        return Ok(ProviderTestResult {
            provider,
            ok: false,
            mode: "live".to_owned(),
            model,
            endpoint: Some(endpoint),
            message: "Provider response did not include assistant content.".to_owned(),
        });
    }
    Ok(ProviderTestResult {
        provider,
        ok: true,
        mode: "live".to_owned(),
        model,
        endpoint: Some(endpoint),
        message: format!("Live provider test succeeded using {source} credentials."),
    })
}

pub(crate) fn provider_test_chat_completion_body(provider: &str, model: &str) -> Value {
    let mut body = json!({
        "model": model,
        "messages": [
            {"role": "user", "content": "Reply with OK."}
        ],
        "temperature": 0,
        "max_tokens": 32
    });
    if normalize_provider(provider) == "deepseek" {
        body["thinking"] = json!({"type": "disabled"});
    }
    body
}

fn clean_provider_string_map(values: BTreeMap<String, String>) -> BTreeMap<String, String> {
    values
        .into_iter()
        .filter_map(|(provider, value)| {
            let provider = normalize_provider(&provider);
            let value = value.trim().to_owned();
            (!provider.is_empty() && !value.is_empty()).then_some((provider, value))
        })
        .collect()
}

fn clean_provider_proxy_mode_map(values: BTreeMap<String, String>) -> BTreeMap<String, String> {
    values
        .into_iter()
        .filter_map(|(provider, value)| {
            let provider = normalize_provider(&provider);
            let value = crate::provider_runtime::normalize_provider_proxy_mode(&value)?;
            (!provider.is_empty()).then_some((provider, value))
        })
        .collect()
}
