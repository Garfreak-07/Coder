use super::*;

#[tokio::test]
async fn provider_settings_endpoints_store_secret_refs_without_returning_keys() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let credentials = crate::credential_store::tests::MemoryKeyringStore::default();
    let app = router(ApiState::new_with_credential_store(
        store.clone(),
        std::sync::Arc::new(credentials.clone()),
    ));
    let initial = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/providers/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let initial_body = response_json(initial).await;
    assert_eq!(initial_body["settings"]["default_provider"], "deepseek");
    assert_eq!(
        initial_body["settings"]["default_model"],
        "deepseek-v4-flash"
    );
    assert_eq!(initial_body["settings"]["mock_mode"], false);
    assert_eq!(
        initial_body["settings"]["proxy_modes"]["deepseek"],
        Value::Null
    );

    let save = post_json(
        app.clone(),
        "/api/v3/providers/settings",
        json!({
            "default_provider": "deepseek",
            "default_model": "deepseek-chat",
            "base_urls": {"deepseek": "https://api.deepseek.com"},
            "proxy_urls": {"deepseek": "http://127.0.0.1:7890"},
            "network": {"deepseek": {
                "request_max_retries": 8,
                "stream_max_retries": 9,
                "stream_idle_timeout_ms": 45000,
                "websocket_connect_timeout_ms": 12000,
                "supports_websockets": false
            }},
            "api_keys": {"deepseek": "sk-secret-value"},
            "mock_mode": false
        }),
    )
    .await;
    assert_eq!(save.status(), StatusCode::OK);
    let save_body = response_json(save).await;
    assert_eq!(save_body["settings"]["default_provider"], "deepseek");
    assert_eq!(
        save_body["settings"]["api_keys"]["deepseek"]["configured"],
        true
    );
    assert_eq!(
        save_body["settings"]["api_keys"]["deepseek"]["source"],
        "keyring"
    );
    assert!(!save_body.to_string().contains("sk-secret-value"));
    assert_eq!(save_body["status"]["default_model"], "deepseek-chat");
    assert_eq!(
        save_body["status"]["default_status"]["base_url"],
        "https://api.deepseek.com"
    );
    assert_eq!(
        save_body["status"]["default_status"]["proxy_url"],
        "http://127.0.0.1:7890/"
    );
    assert_eq!(
        save_body["status"]["default_status"]["proxy_mode"],
        "explicit"
    );
    assert_eq!(
        save_body["status"]["default_status"]["request_max_retries"],
        8
    );
    assert_eq!(
        save_body["status"]["default_status"]["stream_max_retries"],
        9
    );
    assert_eq!(
        save_body["status"]["default_status"]["stream_idle_timeout_ms"],
        45_000
    );
    assert_eq!(
        credentials.value("deepseek").as_deref(),
        Some("sk-secret-value")
    );
    let persisted = fs::read_to_string(store_root.join("settings").join("providers.json")).unwrap();
    assert!(!persisted.contains("sk-secret-value"));
    assert!(!persisted.contains("api_key"));

    let missing_credential_app = router(ApiState::new_with_credential_store(
        store.clone(),
        std::sync::Arc::new(crate::credential_store::tests::MemoryKeyringStore::default()),
    ));
    let missing_credential = missing_credential_app
        .oneshot(
            Request::builder()
                .uri("/api/v3/providers/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let missing_credential_body = response_json(missing_credential).await;
    assert_eq!(
        missing_credential_body["settings"]["default_model"],
        "deepseek-chat"
    );
    assert!(missing_credential_body["settings"]["api_keys"]["deepseek"].is_null());

    let recovered_app = router(ApiState::new_with_credential_store(
        store,
        std::sync::Arc::new(credentials.clone()),
    ));
    let recovered = recovered_app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/providers/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let recovered_body = response_json(recovered).await;
    assert_eq!(recovered_body["settings"]["default_model"], "deepseek-chat");
    assert_eq!(
        recovered_body["settings"]["api_keys"]["deepseek"]["configured"],
        true
    );
    assert_eq!(
        recovered_body["settings"]["api_keys"]["deepseek"]["source"],
        "keyring"
    );

    let update = post_json(
        recovered_app.clone(),
        "/api/v3/providers/settings",
        json!({
            "api_keys": {"deepseek": "updated-secret-value"}
        }),
    )
    .await;
    assert_eq!(update.status(), StatusCode::OK);
    assert_eq!(
        credentials.value("deepseek").as_deref(),
        Some("updated-secret-value")
    );
    assert!(!response_json(update)
        .await
        .to_string()
        .contains("updated-secret-value"));

    let test = post_json(
        recovered_app.clone(),
        "/api/v3/providers/test",
        json!({"provider": "deepseek", "mock": true}),
    )
    .await;
    let test_body = response_json(test).await;
    assert_eq!(test_body["status"]["providers"][0]["provider"], "deepseek");
    assert_eq!(
        test_body["status"]["providers"][0]["credential_configured"],
        true
    );
    assert_eq!(test_body["test"]["ok"], true);
    assert_eq!(test_body["test"]["mode"], "mock");
    assert_eq!(test_body["test"]["model"], "deepseek-chat");
    assert_eq!(test_body["test"]["endpoint"], Value::Null);
    assert!(!test_body.to_string().contains("updated-secret-value"));

    let remove = post_json(
        recovered_app,
        "/api/v3/providers/settings",
        json!({
            "api_keys": {"deepseek": null}
        }),
    )
    .await;
    let remove_body = response_json(remove).await;
    assert!(remove_body["settings"]["api_keys"]["deepseek"].is_null());
    assert_eq!(credentials.value("deepseek"), None);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn provider_settings_keyring_failure_does_not_commit_or_persist() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let credentials = crate::credential_store::tests::MemoryKeyringStore::default();
    credentials.fail_with("injected keyring failure");
    let state =
        ApiState::new_with_credential_store(store, std::sync::Arc::new(credentials.clone()));
    let app = router(state.clone());

    let response = post_json(
        app,
        "/api/v3/providers/settings",
        json!({
            "default_model": "should-not-commit",
            "api_keys": {"deepseek": "must-not-leak"}
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = response_json(response).await;
    assert!(!body.to_string().contains("must-not-leak"));
    assert_eq!(
        state.provider_settings.lock().unwrap().default_model,
        "deepseek-v4-flash"
    );
    assert!(!store_root.join("settings").join("providers.json").exists());
    credentials.clear_failure();
    assert_eq!(credentials.value("deepseek"), None);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn provider_settings_store_failure_rolls_back_keyring_change() {
    let store_root = temp_root();
    fs::create_dir_all(&store_root).unwrap();
    fs::write(store_root.join("settings"), "blocks settings directory").unwrap();
    let credentials = crate::credential_store::tests::MemoryKeyringStore::default();
    let state = ApiState::new_with_credential_store(
        RunStore::new(&store_root),
        std::sync::Arc::new(credentials.clone()),
    );
    let app = router(state.clone());

    let response = post_json(
        app,
        "/api/v3/providers/settings",
        json!({"api_keys": {"deepseek": "must-roll-back"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!response_json(response)
        .await
        .to_string()
        .contains("must-roll-back"));
    assert_eq!(credentials.value("deepseek"), None);
    assert!(state.provider_settings.lock().unwrap().api_keys.is_empty());
    let _ = fs::remove_dir_all(store_root);
}

#[test]
fn provider_settings_patch_updates_clears_and_overrides_env_fallback() {
    let env_name = "CODER_TEST_PROVIDER_KEY_OVERRIDE";
    let previous = env::var_os(env_name);
    env::set_var(env_name, "env-key-value");
    let mut settings = ProviderSettings::default();

    apply_provider_settings_patch(
        &mut settings,
        ProviderSettingsPatch {
            default_provider: Some("openai-compatible".to_owned()),
            default_model: None,
            base_urls: None,
            proxy_urls: None,
            proxy_modes: None,
            network: None,
            api_keys: Some(BTreeMap::from([(
                "openai-compatible".to_owned(),
                json!("settings-key-value"),
            )])),
            mock_mode: None,
        },
    );
    assert_eq!(
        provider_api_key(&settings, "openai-compatible", Some(env_name)),
        Some(("settings-key-value".to_owned(), "settings".to_owned()))
    );

    apply_provider_settings_patch(
        &mut settings,
        ProviderSettingsPatch {
            default_provider: None,
            default_model: None,
            base_urls: None,
            proxy_urls: None,
            proxy_modes: None,
            network: None,
            api_keys: Some(BTreeMap::from([(
                "openai-compatible".to_owned(),
                json!("updated-settings-key"),
            )])),
            mock_mode: None,
        },
    );
    assert_eq!(
        provider_api_key(&settings, "openai-compatible", Some(env_name)),
        Some(("updated-settings-key".to_owned(), "settings".to_owned()))
    );

    apply_provider_settings_patch(
        &mut settings,
        ProviderSettingsPatch {
            default_provider: None,
            default_model: None,
            base_urls: None,
            proxy_urls: None,
            proxy_modes: None,
            network: None,
            api_keys: Some(BTreeMap::from([(
                "openai-compatible".to_owned(),
                Value::Null,
            )])),
            mock_mode: None,
        },
    );
    assert_eq!(
        provider_api_key(&settings, "openai-compatible", Some(env_name)),
        Some(("env-key-value".to_owned(), "environment".to_owned()))
    );
    if let Some(previous) = previous {
        env::set_var(env_name, previous);
    } else {
        env::remove_var(env_name);
    }
}

#[test]
fn provider_proxy_modes_isolate_deepseek_and_allow_explicit_proxy() {
    let mut settings = ProviderSettings::default();
    let deepseek_url = "https://api.deepseek.com/chat/completions";
    let generic_url = "https://api.example.invalid/chat/completions";
    let proxy_env_name = "CODER_PROXY_TEST_PROVIDER_PROXY_URL";
    let previous_proxy = env::var_os(proxy_env_name);
    let previous_no_proxy = env::var_os("no_proxy");
    env::set_var(proxy_env_name, "http://env-proxy.invalid:8080");
    env::remove_var("no_proxy");

    assert_eq!(provider_proxy_mode(&settings, "deepseek"), "direct");
    assert_eq!(
        provider_proxy_url_for_url(&settings, "deepseek", Some(deepseek_url)),
        None
    );
    assert_eq!(
        provider_proxy_mode(&settings, "openai-compatible"),
        "environment"
    );

    settings
        .proxy_urls
        .insert("deepseek".to_owned(), "http://127.0.0.1:7890".to_owned());
    assert_eq!(provider_proxy_mode(&settings, "deepseek"), "explicit");
    assert_eq!(
        provider_proxy_url_for_url(&settings, "deepseek", Some(deepseek_url)),
        Some("http://127.0.0.1:7890".to_owned())
    );

    settings
        .proxy_modes
        .insert("deepseek".to_owned(), "direct".to_owned());
    assert_eq!(provider_proxy_mode(&settings, "deepseek"), "direct");
    assert_eq!(
        provider_proxy_url_for_url(&settings, "deepseek", Some(deepseek_url)),
        None
    );

    settings.proxy_urls.insert(
        "openai-compatible".to_owned(),
        "http://127.0.0.1:8080".to_owned(),
    );
    assert_eq!(
        provider_proxy_url_for_url(&settings, "openai-compatible", Some(generic_url)),
        Some("http://127.0.0.1:8080".to_owned())
    );

    assert_eq!(
        provider_proxy_mode(&settings, "proxy-test-provider"),
        "environment"
    );
    assert_eq!(
        provider_proxy_url_for_url(
            &settings,
            "proxy-test-provider",
            Some("https://api.proxy-test.invalid/v1/chat/completions")
        ),
        Some("http://env-proxy.invalid:8080".to_owned())
    );
    env::set_var("no_proxy", "api.proxy-test.invalid");
    assert_eq!(
        provider_proxy_url_for_url(
            &settings,
            "proxy-test-provider",
            Some("https://api.proxy-test.invalid/v1/chat/completions")
        ),
        None
    );
    restore_env_var(proxy_env_name, previous_proxy);
    restore_env_var("no_proxy", previous_no_proxy);
}

#[test]
fn provider_network_parameters_follow_codex_defaults_and_bounds() {
    let mut settings = ProviderSettings::default();
    assert_eq!(
        provider_request_max_retries(&settings, "deepseek"),
        PROVIDER_REQUEST_MAX_RETRIES
    );
    assert_eq!(
        provider_stream_max_retries(&settings, "deepseek"),
        PROVIDER_STREAM_MAX_RETRIES
    );
    assert_eq!(
        provider_stream_idle_timeout_ms(&settings, "deepseek"),
        PROVIDER_STREAM_IDLE_TIMEOUT_MS
    );
    assert_eq!(
        provider_websocket_connect_timeout_ms(&settings, "deepseek"),
        PROVIDER_WEBSOCKET_CONNECT_TIMEOUT_MS
    );
    assert!(!provider_supports_websockets(&settings, "deepseek"));

    settings.network.insert(
        "deepseek".to_owned(),
        ProviderNetworkSettings {
            request_max_retries: Some(500),
            stream_max_retries: Some(250),
            stream_idle_timeout_ms: Some(45_000),
            websocket_connect_timeout_ms: Some(8_000),
            supports_websockets: true,
        },
    );

    assert_eq!(provider_request_max_retries(&settings, "deepseek"), 100);
    assert_eq!(provider_stream_max_retries(&settings, "deepseek"), 100);
    assert_eq!(
        provider_stream_idle_timeout_ms(&settings, "deepseek"),
        45_000
    );
    assert_eq!(
        provider_websocket_connect_timeout_ms(&settings, "deepseek"),
        8_000
    );
    assert!(provider_supports_websockets(&settings, "deepseek"));
}

#[test]
fn provider_no_proxy_matching_uses_shared_route_rules() {
    assert!(url_matches_no_proxy(
        "https://api.deepseek.com/chat/completions",
        Some("api.deepseek.com")
    ));
    assert!(url_matches_no_proxy(
        "https://sub.internal.example/v1",
        Some(".internal.example")
    ));
    assert!(url_matches_no_proxy(
        "https://api.example.com:8443/v1",
        Some("api.example.com:8443")
    ));
    assert!(!url_matches_no_proxy(
        "https://notinternal.example/v1",
        Some(".internal.example")
    ));
}

#[test]
fn provider_key_state_serialization_redacts_secret() {
    let mut settings = ProviderSettings::default();
    settings.api_keys.insert(
        "openai-compatible".to_owned(),
        ProviderKeyState {
            configured: true,
            source: "settings".to_owned(),
            secret: Some("sk-secret-value".to_owned()),
        },
    );

    let serialized = serde_json::to_string(&settings).unwrap();

    assert!(serialized.contains("\"configured\":true"));
    assert!(serialized.contains("\"source\":\"settings\""));
    assert!(!serialized.contains("sk-secret-value"));
    assert!(!serialized.contains("secret"));
}

#[test]
fn provider_test_endpoint_display_redacts_url_credentials() {
    assert_eq!(
        provider_chat_completions_endpoint(
            "https://user:secret@api.deepseek.com/v1?token=secret#fragment",
        ),
        "https://api.deepseek.com/v1/chat/completions"
    );
    assert_eq!(
        provider_chat_completions_endpoint_for_display(
            "https://user:secret@api.deepseek.com/v1?token=secret#fragment",
        ),
        "https://api.deepseek.com/v1/chat/completions"
    );
}

#[test]
fn provider_test_body_disables_deepseek_thinking_for_short_probe() {
    let deepseek = provider_test_chat_completion_body("deepseek", "deepseek-v4-flash");
    assert_eq!(deepseek["model"], "deepseek-v4-flash");
    assert_eq!(deepseek["max_tokens"], 32);
    assert_eq!(deepseek["thinking"]["type"], "disabled");

    let generic = provider_test_chat_completion_body("openai-compatible", "gpt-compatible-test");
    assert_eq!(generic["model"], "gpt-compatible-test");
    assert!(generic.get("thinking").is_none());
}

#[test]
fn provider_settings_resolve_alias_models_without_overwriting_explicit_models_or_secrets() {
    let mut config = default_project_config();
    config.models.insert(
        "economy_alias".to_owned(),
        ConfigModelSpec {
            provider: "openai-compatible".to_owned(),
            model: "economy".to_owned(),
            base_url_env: Some("SECONDARY_BASE_URL".to_owned()),
            api_key_env: Some("SECONDARY_API_KEY".to_owned()),
            capabilities: coder_config::ModelCapabilities::default(),
        },
    );
    config.models.insert(
        "hook_agent".to_owned(),
        ConfigModelSpec {
            provider: "openai-compatible".to_owned(),
            model: "agent-hook-model".to_owned(),
            base_url_env: None,
            api_key_env: None,
            capabilities: coder_config::ModelCapabilities::default(),
        },
    );
    let mut settings = ProviderSettings {
        default_provider: "deepseek".to_owned(),
        default_model: "deepseek-v4-flash".to_owned(),
        ..ProviderSettings::default()
    };
    settings.api_keys.insert(
        "deepseek".to_owned(),
        ProviderKeyState {
            configured: true,
            source: "settings".to_owned(),
            secret: Some("sk-secret-value".to_owned()),
        },
    );

    apply_provider_settings_to_project_config(&mut config, &settings);

    for model_id in ["default", "economy_alias"] {
        let model = config.models.get(model_id).unwrap();
        assert_eq!(model.provider, "deepseek", "{model_id}");
        assert_eq!(model.model, "deepseek-v4-flash", "{model_id}");
    }
    let hook_agent = config.models.get("hook_agent").unwrap();
    assert_eq!(hook_agent.provider, "openai-compatible");
    assert_eq!(hook_agent.model, "agent-hook-model");
    assert!(!serde_json::to_string(&config)
        .unwrap()
        .contains("sk-secret-value"));
}
