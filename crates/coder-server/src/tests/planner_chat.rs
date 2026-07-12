use super::*;

#[tokio::test]
async fn planner_chat_discuss_mode_never_allows_execution() {
    let app = test_router();
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    assert_eq!(create_response.status(), StatusCode::OK);
    let create_body = response_json(create_response).await;
    let session_id = create_body["session"]["session_id"].as_str().unwrap();

    let turn_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "ready to implement",
            "confirmed": true
        }),
    )
    .await;
    assert_eq!(turn_response.status(), StatusCode::OK);
    let turn_body = response_json(turn_response).await;
    assert_ne!(
        turn_body["assistant_message"],
        "Planner Chat recorded the turn without starting execution."
    );
    assert_eq!(turn_body["ready"], false);
    assert_eq!(turn_body["readiness"], "needs_clarification");
    assert_eq!(turn_body["execution_allowed"], false);
    assert_eq!(turn_body["should_start_workflow"], false);
    assert_eq!(turn_body["run_preview"], Value::Null);
    assert!(turn_body["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["type"] == "planner.message.completed"));
}

#[tokio::test]
async fn planner_chat_writes_session_jsonl_without_raw_secret_text() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    state.provider_settings.lock().unwrap().mock_mode = true;
    let app = router(state.clone());
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    assert_eq!(create_response.status(), StatusCode::OK);
    let create_body = response_json(create_response).await;
    let session_id = create_body["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let turn_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "Do not persist this api_key: sk-secret-value",
            "confirmed": false
        }),
    )
    .await;

    assert_eq!(turn_response.status(), StatusCode::OK);
    let records = store.read_session_records(&session_id).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].kind, "session.created");
    assert_eq!(records[1].kind, "session.turn.completed");
    let text = fs::read_to_string(
        store_root
            .join("sessions")
            .join(format!("{session_id}.jsonl")),
    )
    .unwrap();
    assert!(!text.contains("sk-secret-value"));
    assert!(!text.contains("api_key"));
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn planner_chat_mock_mode_supports_two_turns_without_starting_run() {
    let app = test_router();
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let first_response = post_json(
        app.clone(),
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "Inspect crates/coder-server/src/lib.rs acceptance: cargo test planner"
        }),
    )
    .await;
    assert_eq!(first_response.status(), StatusCode::OK);
    let first = response_json(first_response).await;
    assert_eq!(first["run_preview"], Value::Null);
    assert_eq!(first["should_start_workflow"], false);

    let second_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "Also keep changes limited to planner chat behavior."
        }),
    )
    .await;
    assert_eq!(second_response.status(), StatusCode::OK);
    let second = response_json(second_response).await;
    assert_eq!(second["session"]["turns"].as_array().unwrap().len(), 4);
    assert_eq!(second["should_start_workflow"], false);
    assert_eq!(second["execution_allowed"], false);
}

#[tokio::test]
async fn planner_chat_plan_only_request_returns_plan_instead_of_ready_prompt() {
    let app = test_router();
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = post_json(
            app,
            &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
            json!({
                "message": "Do not execute yet. First give me your concrete plan for building a small browser game in frontend/src/game.js. acceptance: build passes",
                "confirmed": false
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let assistant = body["assistant_message"].as_str().unwrap();

    assert_eq!(body["ready"], false);
    assert_eq!(body["readiness"], "needs_clarification");
    assert_eq!(body["execution_allowed"], false);
    assert_eq!(body["should_start_workflow"], false);
    assert!(assistant.contains("Plan before Start Work"));
    assert!(assistant.contains("Steps:"));
    assert!(assistant.contains("I will not execute this until Start Work."));
    assert!(!assistant.starts_with("I'm ready. Click Start Work"));
}

#[tokio::test]
async fn planner_chat_confirmation_turn_preserves_existing_goal() {
    let app = test_router();
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let first_response = post_json(
            app.clone(),
            &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
            json!({
                "message": "Build a very rough browser game from scratch in F:/ccc/coder-pvz-from-zero. You decide the approach.",
                "confirmed": false
            }),
        )
        .await;
    assert_eq!(first_response.status(), StatusCode::OK);
    let first = response_json(first_response).await;
    assert_eq!(first["ready"], true);

    let second_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "Proceed with your own plan after Start Work. Keep it simple and playable.",
            "confirmed": true,
            "mode": "work"
        }),
    )
    .await;
    assert_eq!(second_response.status(), StatusCode::OK);
    let second = response_json(second_response).await;
    let goal = second["plan_draft"]["goal"].as_str().unwrap();

    assert!(goal.contains("Build a very rough browser game from scratch"));
    assert!(!goal.contains("Proceed with your own plan"));
    assert_eq!(second["ready"], true);
}

#[tokio::test]
async fn planner_chat_current_repository_root_scope_is_ready() {
    let app = test_router();
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let turn_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "In the current repository root, make a rough but playable browser mini-game. Decide the details yourself.",
            "confirmed": false
        }),
    )
    .await;
    assert_eq!(turn_response.status(), StatusCode::OK);
    let body = response_json(turn_response).await;

    assert_eq!(body["ready"], true);
    assert_eq!(body["readiness"], "ready");
    assert_eq!(body["open_questions"], json!([]));
    assert_eq!(body["plan_draft"]["scope"], json!(["."]));
}

#[tokio::test]
async fn planner_chat_turn_never_allows_execution() {
    let app = test_router();
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "work"
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let unready_response = post_json(
        app.clone(),
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "please inspect this first",
            "confirmed": true
        }),
    )
    .await;
    let unready = response_json(unready_response).await;
    assert_eq!(unready["execution_allowed"], false);
    assert_eq!(unready["should_start_workflow"], false);
    assert_eq!(unready["run_preview"], Value::Null);
    assert!(unready.get("run_id").is_none());
    assert!(unready.get("events_url").is_none());
    assert!(unready.get("timeline_url").is_none());
    assert!(!unready["open_questions"].as_array().unwrap().is_empty());

    let unconfirmed_response = post_json(
            app.clone(),
            &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
            json!({
                "message": "ready to run for crates/coder-server/src/lib.rs acceptance: cargo test passes",
                "confirmed": false
            }),
        )
        .await;
    let unconfirmed = response_json(unconfirmed_response).await;
    assert_eq!(unconfirmed["ready"], true);
    assert_eq!(unconfirmed["readiness"], "ready");
    assert_eq!(unconfirmed["execution_allowed"], false);
    assert_eq!(unconfirmed["should_start_workflow"], false);
    assert_eq!(unconfirmed["run_preview"], Value::Null);
    assert!(unconfirmed.get("run_id").is_none());
    assert!(unconfirmed.get("events_url").is_none());
    assert!(unconfirmed.get("timeline_url").is_none());
    let deprecated_confirmation_event = ["work", "confirmation", "requested"].join(".");
    assert!(!unconfirmed["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["type"] == deprecated_confirmation_event));

    let confirmed_response = post_json(
            app,
            &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
            json!({
                "message": "ready and confirmed for crates/coder-server/src/lib.rs acceptance: cargo test passes",
                "confirmed": true
            }),
        )
        .await;
    let confirmed = response_json(confirmed_response).await;
    assert_eq!(confirmed["ready"], true);
    assert_eq!(confirmed["execution_allowed"], false);
    assert_eq!(confirmed["should_start_workflow"], false);
    assert_eq!(confirmed["run_preview"], Value::Null);
    assert!(confirmed.get("run_id").is_none());
    assert!(confirmed.get("events_url").is_none());
    assert!(confirmed.get("timeline_url").is_none());
    assert_eq!(
        confirmed["plan_draft"]["affected_paths"][0],
        "crates/coder-server/src/lib.rs"
    );
}

#[tokio::test]
async fn planner_chat_rejects_invalid_chat_harness_policy() {
    let app = test_router();
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("planner-conversation")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;

    let response = post_json(
        app,
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss",
            "config": config
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    let error = body["error"].as_str().unwrap();
    assert!(
        error.contains("planner_model_side_effect_permission_not_denied")
            || error.contains("run_commands")
    );
}

#[tokio::test]
async fn planner_chat_product_mode_requires_configured_model_provider() {
    let store_root = temp_root();
    let state = ApiState::new(RunStore::new(&store_root));
    state.provider_settings.lock().unwrap().mock_mode = false;
    let app = router(state);
    let mut config = default_project_config();
    let model = config.models.get_mut("default").unwrap();
    model.provider = "missing-test-provider".to_owned();
    model.model = "missing-test-model".to_owned();
    model.base_url_env = Some("CODER_TEST_PLANNER_MISSING_BASE_URL".to_owned());
    model.api_key_env = Some("CODER_TEST_PLANNER_MISSING_API_KEY".to_owned());
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss",
            "config": config
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let turn_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "hello"
        }),
    )
    .await;

    assert_eq!(turn_response.status(), StatusCode::OK);
    let body = response_json(turn_response).await;
    assert!(body["assistant_message"]
        .as_str()
        .unwrap()
        .contains("Configure a provider in Settings before I can plan or execute work."));
    assert_eq!(body["readiness"], "blocked");
    assert_eq!(body["ready"], false);
    assert_eq!(body["execution_allowed"], false);
    assert_eq!(body["should_start_workflow"], false);
    assert_eq!(body["plan_draft"], Value::Null);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn planner_chat_product_mode_calls_configured_provider() {
    let store_root = temp_root();
    let provider_base_url = spawn_openai_compatible_test_server().await;
    let state = ApiState::new(RunStore::new(&store_root));
    {
        let mut settings = state.provider_settings.lock().unwrap();
        settings.mock_mode = false;
        settings.default_provider = "openai-compatible".to_owned();
        settings.default_model = "test-model".to_owned();
        settings
            .base_urls
            .insert("openai-compatible".to_owned(), provider_base_url);
        settings.api_keys.insert(
            "openai-compatible".to_owned(),
            ProviderKeyState {
                configured: true,
                source: "settings".to_owned(),
                secret: Some("sk-test-secret".to_owned()),
            },
        );
    }
    let app = router(state);
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let turn_response = post_json(
        app.clone(),
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "hello planner"
        }),
    )
    .await;

    let turn_status = turn_response.status();
    let body = response_json(turn_response).await;
    assert_eq!(turn_status, StatusCode::OK, "{body}");
    assert_eq!(body["assistant_message"], "Live provider response.");
    assert_eq!(body["should_start_workflow"], false);
    assert_eq!(body["execution_allowed"], false);

    let second_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "second provider-backed turn"
        }),
    )
    .await;

    let second_status = second_response.status();
    let second_body = response_json(second_response).await;
    assert_eq!(second_status, StatusCode::OK, "{second_body}");
    assert_eq!(second_body["assistant_message"], "Live provider response.");
    assert_eq!(second_body["should_start_workflow"], false);
    assert_eq!(second_body["execution_allowed"], false);
    assert_eq!(second_body["session"]["turns"].as_array().unwrap().len(), 4);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn planner_chat_product_mode_consumes_openai_compatible_streaming_response() {
    let store_root = temp_root();
    let (provider_base_url, captured) =
        spawn_openai_compatible_streaming_capture_test_server().await;
    let state = ApiState::new(RunStore::new(&store_root));
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);
    let mut config = default_project_config();
    config.agents.get_mut("planner").unwrap().runtime.effort = Some("medium".to_owned());
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss",
            "config": config
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let turn_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "hello streaming planner"
        }),
    )
    .await;

    let turn_status = turn_response.status();
    let body = response_json(turn_response).await;
    assert_eq!(turn_status, StatusCode::OK, "{body}");
    assert_eq!(body["assistant_message"], "Streamed provider response.");
    assert_eq!(body["response_truncated"], false);
    assert_eq!(body["should_start_workflow"], false);
    assert_eq!(body["provider_trace"]["requested_stream"], true);
    assert_eq!(body["provider_trace"]["response_transport"], "event_stream");
    assert_eq!(body["provider_trace"]["streaming_fallback"], false);
    assert_eq!(body["provider_trace"]["finish_reason"], "stop");
    assert_eq!(body["provider_trace"]["provider_turns"], 1);
    assert_eq!(body["provider_trace"]["input_tokens"], 41);
    assert_eq!(body["provider_trace"]["output_tokens"], 7);
    assert_eq!(body["provider_trace"]["total_tokens"], 48);
    assert_eq!(body["provider_trace"]["cache_read_tokens"], 11);
    assert_eq!(body["provider_trace"]["usage_reported"], true);
    assert!(body["provider_trace"]["estimated_input_tokens"]
        .as_u64()
        .is_some_and(|value| value > 0));
    assert!(body["events"].as_array().unwrap().iter().any(|event| {
        event["type"] == "planner.provider.completed"
            && event["requested_stream"] == true
            && event["response_transport"] == "event_stream"
            && event["streaming_fallback"] == false
    }));

    let captured_body = captured.lock().unwrap().clone().unwrap();
    assert_eq!(captured_body["model"], "test-model");
    assert_eq!(captured_body["stream"], true);
    assert_eq!(captured_body["stream_options"]["include_usage"], true);
    assert_eq!(captured_body["temperature"], 0.2);
    assert_eq!(captured_body["reasoning_effort"], "medium");
    assert!(captured_body["messages"].as_array().unwrap().len() >= 2);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn planner_chat_surfaces_provider_failure_as_blocked_turn() {
    let store_root = temp_root();
    let (provider_base_url, _) = spawn_openai_compatible_status_sequence_capture_test_server(vec![
        OpenAiCompatibleStatusResponse {
            status: StatusCode::PAYMENT_REQUIRED,
            content_type: "application/json",
            body: json!({"error": {"message": "account balance is insufficient"}}).to_string(),
        },
    ])
    .await;
    let state = ApiState::new(RunStore::new(&store_root));
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({"workflow_id": "planner-led", "mode": "discuss"}),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({"message": "Plan a small README update."}),
    )
    .await;
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["readiness"], "blocked");
    assert_eq!(body["session"]["turns"].as_array().unwrap().len(), 2);
    assert!(body["assistant_message"]
        .as_str()
        .is_some_and(|message| message.contains("HTTP 402")));
    assert_eq!(body["execution_allowed"], false);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn planner_stream_idle_timeout_applies_between_response_chunks() {
    let provider_base_url = spawn_delayed_openai_compatible_streaming_test_server().await;
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{provider_base_url}/chat/completions"))
        .send()
        .await
        .unwrap();

    let error = parse_live_planner_response_with_idle_timeout(
        response,
        &[],
        planner_provider_trace(true, "unknown", false, None),
        Duration::from_millis(20),
    )
    .await
    .unwrap_err();

    assert!(error.contains("no data for 20 ms"), "{error}");
}

#[tokio::test]
async fn planner_chat_provider_trace_records_streaming_fallback_to_json() {
    let store_root = temp_root();
    let (provider_base_url, captured) =
        spawn_openai_compatible_status_sequence_capture_test_server(vec![
            OpenAiCompatibleStatusResponse {
                status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                content_type: "application/json",
                body: json!({
                    "error": {
                        "message": "streaming is not supported"
                    }
                })
                .to_string(),
            },
            OpenAiCompatibleStatusResponse {
                status: StatusCode::OK,
                content_type: "application/json",
                body: json!({
                    "choices": [
                        {
                            "finish_reason": "stop",
                            "message": {
                                "content": "Fallback JSON response."
                            }
                        }
                    ],
                    "usage": {
                        "prompt_tokens": 29,
                        "completion_tokens": 5,
                        "total_tokens": 34,
                        "prompt_tokens_details": {"cached_tokens": 9}
                    }
                })
                .to_string(),
            },
        ])
        .await;
    let app = provider_backed_test_app(&store_root, provider_base_url);
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let turn_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "hello planner"
        }),
    )
    .await;

    assert_eq!(turn_response.status(), StatusCode::OK);
    let body = response_json(turn_response).await;
    assert_eq!(body["assistant_message"], "Fallback JSON response.");
    assert_eq!(body["provider_trace"]["requested_stream"], true);
    assert_eq!(body["provider_trace"]["response_transport"], "json");
    assert_eq!(body["provider_trace"]["streaming_fallback"], true);
    assert_eq!(body["provider_trace"]["fallback_status"], 415);
    assert_eq!(body["provider_trace"]["finish_reason"], "stop");
    assert_eq!(body["provider_trace"]["provider_turns"], 2);
    assert_eq!(body["provider_trace"]["input_tokens"], 29);
    assert_eq!(body["provider_trace"]["output_tokens"], 5);
    assert_eq!(body["provider_trace"]["total_tokens"], 34);
    assert_eq!(body["provider_trace"]["cache_read_tokens"], 9);
    assert!(body["events"].as_array().unwrap().iter().any(|event| {
        event["type"] == "planner.provider.completed"
            && event["response_transport"] == "json"
            && event["streaming_fallback"] == true
            && event["fallback_status"] == 415
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 2);
    assert_eq!(captured_requests[0]["stream"], true);
    assert!(captured_requests[1].get("stream").is_none());
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn planner_ready_plan_confirmation_is_local_and_preserves_the_plan() {
    let store_root = temp_root();
    let (provider_base_url, captured) =
        spawn_openai_compatible_sequence_capture_test_server(vec![json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": "The scoped plan is ready."}
            }],
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 5,
                "total_tokens": 25
            }
        })])
        .await;
    let app = provider_backed_test_app(&store_root, provider_base_url);
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({"workflow_id": "planner-led", "mode": "discuss"}),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let first_response = post_json(
        app.clone(),
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "Create README.md. Acceptance: README.md exists.",
            "mode": "discuss"
        }),
    )
    .await;
    let first_body = response_json(first_response).await;
    assert_eq!(first_body["ready"], true, "{first_body}");
    let first_goal = first_body["plan_draft"]["goal"].clone();

    let confirm_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "The plan looks good. Keep it simple.",
            "confirmed": true,
            "mode": "work"
        }),
    )
    .await;
    let confirm_body = response_json(confirm_response).await;

    assert_eq!(confirm_body["ready"], true);
    assert_eq!(confirm_body["session"]["mode"], "work");
    assert_eq!(confirm_body["plan_draft"]["goal"], first_goal);
    assert!(confirm_body["provider_trace"].is_null());
    assert!(confirm_body["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| { event["type"] == "planner.confirmation.local" }));
    assert_eq!(captured.lock().unwrap().len(), 1);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn planner_ready_response_mentions_start_work_and_native_executor_boundary() {
    let app = test_router();
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let turn_response = post_json(
            app,
            &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
            json!({
                "message": "Create a minimal Snake game in F:\\ccc\\coder-snake-game. Acceptance: index.html exists; main.js passes node --check.",
                "confirmed": true
            }),
        )
        .await;

    assert_eq!(turn_response.status(), StatusCode::OK);
    let body = response_json(turn_response).await;
    let assistant = body["assistant_message"].as_str().unwrap();
    assert_eq!(body["ready"], true);
    assert_eq!(body["ready_for_start_work"], true);
    assert!(body["missing_information"].as_array().unwrap().is_empty());
    assert!(body["concise_plan_summary"]
        .as_str()
        .unwrap()
        .contains("Snake game"));
    assert!(body["structured_artifacts"].as_array().unwrap().is_empty());
    assert!(assistant.contains("Click Start Work"));
    assert!(assistant.contains("native executor"));
    assert!(!assistant.contains("Discuss mode"));
    assert!(!assistant.contains("Work mode"));
    let lower = assistant.to_ascii_lowercase();
    assert!(!lower.contains("i edited"));
    assert!(!lower.contains("i ran "));
    assert_eq!(body["should_start_workflow"], false);
    assert_eq!(body["execution_allowed"], false);
}

#[test]
fn planner_chat_runtime_uses_explicit_surface_binding() {
    let config = default_project_config();

    let runtime = resolve_planner_runtime(&config, "planner-led", None).unwrap();

    assert_eq!(runtime.node_id, "planner");
    assert_eq!(runtime.agent_id, "planner");
    assert_eq!(runtime.harness_id, "planner-conversation");
    assert_eq!(runtime.agent.role, "planner");
    assert_eq!(runtime.harness.backend, "planner-model");
}

#[test]
fn planner_chat_rejects_workflow_decision_planner_agent() {
    let config = default_project_config();

    let error =
        resolve_planner_runtime(&config, "planner-led", Some("workflow-planner")).unwrap_err();

    assert!(error
        .message
        .contains("surface_bindings.planner_chat selects 'planner'"));
}

#[test]
fn native_planner_context_adapter_exposes_only_bounded_read_tools() {
    let mut config = default_project_config();
    let model = config.models.get_mut("planner_chat").unwrap();
    model.provider = "deepseek".to_owned();
    model.model = "deepseek-v4-flash".to_owned();
    model.base_url_env = Some("SHOULD_NOT_BE_USED_BASE_URL".to_owned());
    model.api_key_env = Some("SHOULD_NOT_BE_USED_API_KEY".to_owned());
    let runtime = resolve_planner_runtime(&config, "planner-led", Some("planner")).unwrap();
    let request = PlannerConversationRequest {
        session_id: "pcs-native-context".to_owned(),
        workflow_id: "planner-led".to_owned(),
        repo_root: Some("F:/ccc".to_owned()),
        runtime,
        mode: "discuss".to_owned(),
        message: "Create a Snake game in F:\\ccc.".to_owned(),
        confirmed: false,
        history: Vec::new(),
        current_plan: None,
        provider_settings: ProviderSettings::default(),
    };
    let adapter = NativePlannerContextAdapter::new();

    let context = adapter.context_payload(&request, PlannerProviderRequestMode::Normal);
    let text = context.to_string();

    assert_eq!(context["adapter"], "native-planner-context");
    assert_eq!(context["contract"], "coder.native_planner_context.v1");
    assert_eq!(context["planner_tool_policy"]["tool_count"], 4);
    assert_eq!(context["planner_tool_policy"]["access"], "read_only");
    assert_eq!(context["planner_tool_policy"]["repo_bound"], true);
    assert_eq!(context["runtime"]["model"]["model"], "deepseek-v4-flash");
    assert_eq!(context["runtime"]["model"]["provider"], "deepseek");
    assert!(
        context["planner_context"]["strict_output_contract"]["plan_draft"]["acceptance_criteria"]
            .as_str()
            .is_some_and(|value| value.contains("every material goal/scope behavior"))
    );
    let recovery =
        adapter.context_payload(&request, PlannerProviderRequestMode::PromptOverflowRecovery);
    assert_eq!(
        recovery["strict_output_contract"],
        context["planner_context"]["strict_output_contract"]
    );
    assert!(!text.contains("\"name\":\"terminal\""));
    assert!(!text.contains("\"name\":\"file_editor\""));
    assert!(!text.contains("\"name\":\"task_tracker\""));
    assert!(context["planner_tool_policy"]["command_execution"]
        .as_bool()
        .is_some_and(|allowed| !allowed));

    let events = adapter.message_events(&request, PlannerProviderRequestMode::Normal);
    assert!(events.iter().all(|event| event["run"] == false));
    let system_text = events.first().unwrap()["content"]
        .as_array()
        .unwrap()
        .first()
        .unwrap()["text"]
        .as_str()
        .unwrap();
    assert!(system_text.contains("Native Coder planner context"));
    assert!(system_text.contains("read-only repository tool snapshot"));
    assert!(system_text.contains("native executor"));
    assert!(system_text.contains("traceable to an observable acceptance criterion"));
    assert!(!system_text.contains("command_run"));
}

#[test]
fn native_planner_context_adapter_compacts_long_history() {
    let config = default_project_config();
    let runtime = resolve_planner_runtime(&config, "planner-led", Some("planner")).unwrap();
    let old_secret_sized_text = format!("old-user-0 {}", "sensitive-context ".repeat(80));
    let mut history = Vec::new();
    for index in 0..14 {
        history.push(PlannerChatTurn {
            role: if index % 2 == 0 { "user" } else { "assistant" }.to_owned(),
            content: if index == 0 {
                old_secret_sized_text.clone()
            } else {
                format!("history-turn-{index}")
            },
            artifacts: if index == 1 {
                vec![PlannerArtifact::Notes {
                    title: "old-notes".to_owned(),
                    items: vec!["one".to_owned()],
                    collapsed: false,
                }]
            } else {
                Vec::new()
            },
            response_truncated: false,
        });
    }
    let request = PlannerConversationRequest {
        session_id: "pcs-long-history".to_owned(),
        workflow_id: "planner-led".to_owned(),
        repo_root: Some("F:/repo".to_owned()),
        runtime,
        mode: "discuss".to_owned(),
        message: "Continue with the current plan.".to_owned(),
        confirmed: false,
        history,
        current_plan: None,
        provider_settings: ProviderSettings::default(),
    };
    let adapter = NativePlannerContextAdapter::new();

    let context = adapter.context_payload(&request, PlannerProviderRequestMode::Normal);
    let history_compaction = &context["planner_context"]["history_compaction"];
    assert_eq!(
        history_compaction["contract"],
        "coder.planner_history_compaction.v1"
    );
    assert_eq!(history_compaction["status"], "completed");
    assert_eq!(history_compaction["omitted_turns"], 4);
    assert_eq!(history_compaction["recent_turns"], 10);
    assert!(
        history_compaction["token_savings_estimate"]
            .as_u64()
            .unwrap()
            > 0
    );

    let events = adapter.message_events(&request, PlannerProviderRequestMode::Normal);
    let event_texts = events.iter().map(planner_event_text).collect::<Vec<_>>();

    assert_eq!(event_texts.len(), 13);
    assert!(event_texts[1].contains("Compacted earlier planner chat history"));
    assert!(event_texts[1].contains("omitted_turns=4"));
    assert!(event_texts[1].contains("user_turns=2"));
    assert!(event_texts[1].contains("assistant_turns=2"));
    assert!(event_texts[1].contains("artifact_count=1"));
    assert!(event_texts[1].contains("old-user-0 sensitive-context"));
    assert!(event_texts[1].len() < old_secret_sized_text.len());
    assert!(!event_texts[1].contains(&"sensitive-context ".repeat(30)));
    assert!(!event_texts
        .iter()
        .any(|text| text == &old_secret_sized_text));
    assert_eq!(event_texts[2], "history-turn-4");
    assert_eq!(event_texts[11], "history-turn-13");
    assert_eq!(event_texts[12], "Continue with the current plan.");

    let provider_messages = adapter.provider_messages(&request, PlannerProviderRequestMode::Normal);
    assert_eq!(provider_messages.len(), events.len());
    assert!(provider_messages[1]["content"]
        .as_str()
        .unwrap()
        .contains("Compacted earlier planner chat history"));
    assert_eq!(provider_messages[2]["content"], "history-turn-4");
}

#[tokio::test]
async fn planner_chat_reads_bound_repository_through_frozen_tool_snapshot() {
    let store_root = temp_root();
    let repo_root = store_root.join("repo");
    fs::create_dir_all(&repo_root).unwrap();
    fs::write(
        repo_root.join("README.md"),
        "planner-read-marker: bounded-read-ok\n",
    )
    .unwrap();
    let first_payload = json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": [
                    {
                        "id": "planner-read-1",
                        "type": "function",
                        "function": {
                            "name": "repo_read_file_range",
                            "arguments": "{\"path\":\"README.md\",\"max_lines\":20}"
                        }
                    },
                    {
                        "id": "planner-command-bypass",
                        "type": "function",
                        "function": {
                            "name": "command_run",
                            "arguments": "{\"argv\":[\"cmd\",\"/c\",\"echo bypass>planner-bypass.txt\"],\"approved\":true,\"sandbox\":true}"
                        }
                    }
                ]
            }
        }]
    });
    let second_payload = json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "content": "The repository marker is bounded-read-ok."
            }
        }]
    });
    let (provider_base_url, captured) =
        spawn_openai_compatible_sequence_capture_test_server(vec![first_payload, second_payload])
            .await;
    let app = provider_backed_test_app(&store_root, provider_base_url);
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "repo": repo_root.to_string_lossy(),
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    let create_body = response_json(create_response).await;
    assert_eq!(
        create_body["session"]["repo_root"],
        repo_root.to_string_lossy().as_ref()
    );
    let session_id = create_body["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let turn_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({"message": "What marker is recorded in README.md?"}),
    )
    .await;

    assert_eq!(turn_response.status(), StatusCode::OK);
    let body = response_json(turn_response).await;
    assert!(body["assistant_message"]
        .as_str()
        .unwrap()
        .contains("bounded-read-ok"));
    assert_eq!(body["provider_trace"]["tool_turns"], 1);
    assert_eq!(body["provider_trace"]["tool_calls"], 2);
    assert!(body["provider_trace"]["tool_result_bytes"]
        .as_u64()
        .is_some_and(|bytes| bytes > 0));
    let captured = captured.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    let tool_names = captured[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(
        tool_names,
        vec![
            "repo_find_files",
            "repo_search_text",
            "repo_read_file_range",
            "git_status"
        ]
    );
    assert!(!captured[0].to_string().contains("command_run"));
    assert!(!captured[0].to_string().contains("write_text_file"));
    assert!(captured[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "planner-read-1"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("bounded-read-ok"))
        }));
    assert!(captured[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "planner-command-bypass"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("not selected for this turn"))
        }));
    assert!(!repo_root.join("planner-bypass.txt").exists());
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn planner_chat_streaming_tool_call_reuses_shared_stream_adapter() {
    let store_root = temp_root();
    let repo_root = store_root.join("stream-repo");
    fs::create_dir_all(&repo_root).unwrap();
    fs::write(repo_root.join("README.md"), "stream-read-ok\n").unwrap();
    let (provider_base_url, captured) =
        spawn_openai_compatible_streaming_tool_sequence_test_server().await;
    let app = provider_backed_test_app(&store_root, provider_base_url);
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "repo": repo_root.to_string_lossy(),
            "workflow_id": "planner-led"
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({"message": "What marker is recorded in README.md?"}),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert!(
        body["assistant_message"]
            .as_str()
            .unwrap()
            .contains("stream-read-ok"),
        "unexpected Planner response: {body}"
    );
    assert_eq!(body["provider_trace"]["response_transport"], "event_stream");
    assert_eq!(body["provider_trace"]["tool_turns"], 1);
    assert_eq!(body["provider_trace"]["tool_calls"], 1);
    let captured = captured.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    assert!(captured[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("stream-read-ok"))
        }));
    let _ = fs::remove_dir_all(store_root);
}

#[test]
fn planner_history_compaction_records_circuit_success() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let config = default_project_config();
    let runtime = resolve_planner_runtime(&config, "planner-led", Some("planner")).unwrap();
    let history = (0..14)
        .map(|index| PlannerChatTurn {
            role: if index % 2 == 0 { "user" } else { "assistant" }.to_owned(),
            content: format!("history-turn-{index}"),
            artifacts: Vec::new(),
            response_truncated: false,
        })
        .collect::<Vec<_>>();
    let request = PlannerConversationRequest {
        session_id: "session-history-compact".to_owned(),
        workflow_id: "planner-led".to_owned(),
        repo_root: Some("F:/repo".to_owned()),
        runtime,
        mode: "discuss".to_owned(),
        message: "Continue.".to_owned(),
        confirmed: false,
        history,
        current_plan: None,
        provider_settings: ProviderSettings::default(),
    };

    let attempt = planner_history_compaction_attempt(&request).unwrap();
    let outcome = record_planner_history_compaction_outcome(&store, attempt).unwrap();
    let circuit = store
        .read_compaction_circuit_state("planner-chat-session-history-compact")
        .unwrap()
        .unwrap();

    assert_eq!(outcome["contract"], "coder.planner_history_compaction.v1");
    assert_eq!(outcome["success"], true);
    assert_eq!(outcome["omitted_turns"], 4);
    assert_eq!(circuit.consecutive_failures, 0);
    assert!(!circuit.circuit_breaker_open);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn planner_provider_length_finish_reason_uses_controlled_message() {
    let store_root = temp_root();
    let long_content = (0..900)
        .map(|index| format!("word{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let provider_base_url = spawn_openai_compatible_test_server_with_payload(json!({
        "choices": [
            {
                "finish_reason": "length",
                "message": {
                    "content": long_content
                }
            }
        ]
    }))
    .await;
    let app = provider_backed_test_app(&store_root, provider_base_url);
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let turn_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "hello planner"
        }),
    )
    .await;

    assert_eq!(turn_response.status(), StatusCode::OK);
    let body = response_json(turn_response).await;
    let assistant = body["assistant_message"].as_str().unwrap();
    assert_eq!(body["response_truncated"], true);
    assert!(assistant.contains(PLANNER_TRUNCATED_NOTICE));
    assert!(assistant.split_whitespace().count() <= PLANNER_NORMAL_WORD_LIMIT + 20);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn planner_provider_length_finish_reason_recovers_with_meta_prompt() {
    let store_root = temp_root();
    let first_payload = json!({
        "choices": [
            {
                "finish_reason": "length",
                "message": {
                    "content": "Partial provider plan"
                }
            }
        ]
    });
    let second_payload = json!({
        "choices": [
            {
                "finish_reason": "stop",
                "message": {
                    "content": "Recovered provider continuation"
                }
            }
        ]
    });
    let (provider_base_url, captured) =
        spawn_openai_compatible_sequence_capture_test_server(vec![first_payload, second_payload])
            .await;
    let app = provider_backed_test_app(&store_root, provider_base_url);
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let turn_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "hello planner"
        }),
    )
    .await;

    assert_eq!(turn_response.status(), StatusCode::OK);
    let body = response_json(turn_response).await;
    let assistant = body["assistant_message"].as_str().unwrap();
    assert_eq!(body["response_truncated"], false);
    assert!(assistant.contains("Partial provider plan"));
    assert!(assistant.contains("Recovered provider continuation"));
    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 2);
    let second_messages = captured_requests[1]["messages"].as_array().unwrap();
    assert!(second_messages.iter().any(|message| {
        message["role"] == "assistant" && message["content"] == "Partial provider plan"
    }));
    assert!(second_messages.iter().any(|message| {
        message["role"] == "user"
            && message["content"]
                .as_str()
                .unwrap()
                .contains("Output token limit hit. Resume directly")
    }));
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn planner_provider_length_recovery_honors_runtime_attempt_limit() {
    let store_root = temp_root();
    let length_payload = json!({
        "choices": [
            {
                "finish_reason": "length",
                "message": {
                    "content": "Still too long"
                }
            }
        ]
    });
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        length_payload.clone(),
        length_payload,
        json!({
            "choices": [
                {
                    "finish_reason": "stop",
                    "message": {
                        "content": "Should not be requested"
                    }
                }
            ]
        }),
    ])
    .await;
    let state = ApiState::new(RunStore::new(&store_root));
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);
    let mut config = default_project_config();
    config
        .agents
        .get_mut("planner")
        .unwrap()
        .runtime
        .max_output_recovery_attempts = 1;
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss",
            "config": config
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let turn_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "hello planner"
        }),
    )
    .await;

    assert_eq!(turn_response.status(), StatusCode::OK);
    let body = response_json(turn_response).await;
    assert_eq!(body["response_truncated"], true);
    assert!(body["assistant_message"]
        .as_str()
        .unwrap()
        .contains(PLANNER_TRUNCATED_NOTICE));
    assert_eq!(captured.lock().unwrap().len(), 2);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn planner_provider_prompt_too_long_retries_with_compact_context() {
    let store_root = temp_root();
    let (provider_base_url, captured) =
        spawn_openai_compatible_status_sequence_capture_test_server(vec![
            OpenAiCompatibleStatusResponse {
                status: StatusCode::BAD_REQUEST,
                content_type: "application/json",
                body: json!({
                    "error": {
                        "message": "prompt is too long: 300000 tokens > 200000 maximum"
                    }
                })
                .to_string(),
            },
            OpenAiCompatibleStatusResponse {
                status: StatusCode::OK,
                content_type: "application/json",
                body: json!({
                    "choices": [
                        {
                            "finish_reason": "stop",
                            "message": {
                                "content": "Recovered after compact planner context"
                            }
                        }
                    ]
                })
                .to_string(),
            },
        ])
        .await;
    let app = provider_backed_test_app(&store_root, provider_base_url);
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let turn_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "hello planner"
        }),
    )
    .await;

    assert_eq!(turn_response.status(), StatusCode::OK);
    let body = response_json(turn_response).await;
    assert!(body["assistant_message"]
        .as_str()
        .unwrap()
        .contains("Recovered after compact planner context"));
    assert_eq!(body["response_truncated"], false);
    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 2);
    assert_eq!(captured_requests[0]["stream"], true);
    assert_eq!(captured_requests[1]["stream"], true);
    let first_system = captured_requests[0]["messages"][0]["content"]
        .as_str()
        .unwrap();
    let second_system = captured_requests[1]["messages"][0]["content"]
        .as_str()
        .unwrap();
    assert!(first_system.contains("coder.native_planner_context.v1"));
    assert!(second_system.contains("coder.planner_chat.prompt_overflow_recovery.v1"));
    assert!(second_system.contains("prompt_overflow_recovery"));
    assert!(!second_system.contains("coder.native_planner_context.v1"));
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn planner_markdown_table_response_becomes_structured_artifact() {
    let store_root = temp_root();
    let provider_base_url = spawn_openai_compatible_test_server_with_payload(json!({
            "choices": [
                {
                    "finish_reason": "stop",
                    "message": {
                        "content": "Here is the compact plan:\n\n| File | Change |\n| --- | --- |\n| index.html | add shell |\n| main.js | add game loop |\n"
                    }
                }
            ]
        }))
        .await;
    let app = provider_backed_test_app(&store_root, provider_base_url);
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "mode": "discuss"
        }),
    )
    .await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let turn_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "please compare options"
        }),
    )
    .await;

    assert_eq!(turn_response.status(), StatusCode::OK);
    let body = response_json(turn_response).await;
    let assistant = body["assistant_message"].as_str().unwrap();
    assert!(!assistant.contains("| File | Change |"));
    let artifacts = body["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0]["type"], "table");
    assert_eq!(artifacts[0]["columns"][0], "File");
    assert_eq!(artifacts[0]["rows"][1][1], "add game loop");
    let session_turns = body["session"]["turns"].as_array().unwrap();
    let latest = session_turns.last().unwrap();
    assert_eq!(latest["artifacts"][0]["type"], "table");
    let _ = fs::remove_dir_all(store_root);
}
