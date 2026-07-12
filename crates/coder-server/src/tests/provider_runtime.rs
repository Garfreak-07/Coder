use super::*;

#[tokio::test]
async fn provider_settings_endpoints_store_secret_refs_without_returning_keys() {
    let app = router(ApiState::new(RunStore::new(temp_root())));
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
        "settings"
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

    let test = post_json(
        app.clone(),
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
    assert!(!test_body.to_string().contains("sk-secret-value"));

    let remove = post_json(
        app,
        "/api/v3/providers/settings",
        json!({
            "api_keys": {"deepseek": null}
        }),
    )
    .await;
    let remove_body = response_json(remove).await;
    assert!(remove_body["settings"]["api_keys"]["deepseek"].is_null());
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
fn planner_chat_body_bounds_tokens_and_disables_deepseek_thinking() {
    let messages = vec![json!({
        "role": "user",
        "content": "challenge question"
    })];
    let deepseek = planner_chat_completion_body(
        "deepseek",
        "deepseek-v4-flash",
        messages.clone(),
        PLANNER_CHAT_MAX_OUTPUT_TOKENS_DEFAULT,
        None,
    );
    assert_eq!(deepseek["model"], "deepseek-v4-flash");
    assert_eq!(deepseek["temperature"], 0.2);
    assert_eq!(deepseek["max_tokens"], 900);
    assert_eq!(deepseek["thinking"]["type"], "disabled");
    assert_eq!(deepseek["response_format"]["type"], "json_object");
    assert_eq!(deepseek["messages"], json!(messages));

    let generic = planner_chat_completion_body(
        "openai-compatible",
        "gpt-compatible-test",
        Vec::new(),
        1_200,
        Some("max"),
    );
    assert_eq!(generic["model"], "gpt-compatible-test");
    assert_eq!(generic["max_tokens"], 1_200);
    assert_eq!(generic["reasoning_effort"], "xhigh");
    assert!(generic.get("thinking").is_none());
    assert!(generic.get("response_format").is_none());

    let deepseek_effort = planner_chat_completion_body(
        "deepseek",
        "deepseek-v4-flash",
        Vec::new(),
        1_200,
        Some("medium"),
    );
    assert_eq!(deepseek_effort["thinking"]["type"], "enabled");
    assert!(deepseek_effort.get("reasoning_effort").is_none());
}

#[test]
fn planner_chat_tool_body_uses_frozen_snapshot_without_conflicting_json_mode() {
    let tools = json!([{
        "type": "function",
        "function": {
            "name": "repo_read_file_range",
            "parameters": {"type": "object"}
        }
    }]);
    let body = planner_chat_completion_body_with_tools(
        "deepseek",
        "deepseek-chat",
        Vec::new(),
        900,
        None,
        tools.clone(),
        true,
    );

    assert_eq!(body["tools"], tools);
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], true);
    assert!(body.get("response_format").is_none());
    assert_eq!(body["thinking"]["type"], "disabled");
}

#[test]
fn planner_provider_retention_policy_matches_claude_prompt_dump_bounds() {
    assert_eq!(CLAUDE_DUMP_PROMPTS_MAX_CACHED_REQUESTS, 5);
    assert_eq!(PLANNER_PROVIDER_RETAINED_FULL_REQUEST_BODIES, 1);
    assert_eq!(PLANNER_PROVIDER_RESPONSE_MAX_BYTES, 2 * 1024 * 1024);
    assert_eq!(
        PLANNER_PROVIDER_STREAM_PENDING_MAX_BYTES,
        PLANNER_PROVIDER_RESPONSE_MAX_BYTES
    );
}

#[tokio::test]
async fn planner_provider_rejects_oversized_non_streaming_response() {
    let oversized_content = "x".repeat(PLANNER_PROVIDER_RESPONSE_MAX_BYTES + 1);
    let provider_base_url = spawn_openai_compatible_test_server_with_payload(json!({
        "choices": [
            {
                "message": {
                    "content": oversized_content
                }
            }
        ]
    }))
    .await;
    let url = provider_chat_completions_endpoint(&provider_base_url);
    let response = crate::outbound_http::HttpClientFactory::new(
        crate::outbound_http::OutboundProxyRoute::Direct,
    )
    .builder(&url, crate::outbound_http::ClientRouteClass::ProviderApi)
    .unwrap()
    .build()
    .unwrap()
    .post(url)
    .json(&json!({}))
    .send()
    .await
    .unwrap();

    let error = parse_live_planner_response_with_idle_timeout(
        response,
        &[],
        planner_provider_trace(true, "unknown", false, None),
        Duration::from_secs(90),
    )
    .await
    .unwrap_err();

    assert!(
        error.contains("planner model response exceeded"),
        "unexpected planner response error: {error}"
    );
    assert!(error.contains(&PLANNER_PROVIDER_RESPONSE_MAX_BYTES.to_string()));
}

#[tokio::test]
async fn planner_provider_rejects_oversized_streaming_pending_line() {
    let provider_base_url = spawn_raw_openai_compatible_test_server(
        StatusCode::OK,
        "text/event-stream",
        format!(
            "data: {}",
            "x".repeat(PLANNER_PROVIDER_STREAM_PENDING_MAX_BYTES + 1)
        ),
    )
    .await;
    let url = provider_chat_completions_endpoint(&provider_base_url);
    let response = crate::outbound_http::HttpClientFactory::new(
        crate::outbound_http::OutboundProxyRoute::Direct,
    )
    .builder(&url, crate::outbound_http::ClientRouteClass::ProviderApi)
    .unwrap()
    .build()
    .unwrap()
    .post(url)
    .json(&json!({}))
    .send()
    .await
    .unwrap();

    let error = parse_live_planner_response_with_idle_timeout(
        response,
        &[],
        planner_provider_trace(true, "unknown", false, None),
        Duration::from_secs(90),
    )
    .await
    .unwrap_err();

    assert!(
        error.contains("planner streaming response pending line exceeded"),
        "unexpected planner streaming error: {error}"
    );
    assert!(error.contains(&PLANNER_PROVIDER_STREAM_PENDING_MAX_BYTES.to_string()));
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

    for model_id in ["default", "planner_chat", "economy_alias"] {
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
