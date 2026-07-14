use super::*;
use coder_events::OutputEvent;

type DelayedSequenceState = Arc<(Duration, Mutex<VecDeque<Value>>, Arc<Mutex<Vec<Value>>>)>;

#[tokio::test]
async fn conversation_mock_mode_keeps_plain_message_history() {
    let app = test_router();
    let create_response =
        post_json(app.clone(), "/api/v3/conversations", json!({"repo": "."})).await;
    assert_eq!(create_response.status(), StatusCode::OK);
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = post_json(
        app,
        &format!("/api/v3/conversations/{session_id}/turn"),
        json!({"message": "Hello"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["session"]["turns"].as_array().unwrap().len(), 2);
    assert_eq!(body["session"]["turns"][0]["role"], "user");
    assert_eq!(body["session"]["turns"][1]["role"], "assistant");
    assert_eq!(
        body["assistant_message"],
        "Mock conversation response: Hello"
    );
    assert_eq!(body["status"], "completed");
    assert!(body["turn_id"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
}

#[tokio::test]
async fn conversation_rejects_empty_messages() {
    let app = test_router();
    let create_response = post_json(app.clone(), "/api/v3/conversations", json!({})).await;
    let session_id = response_json(create_response).await["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = post_json(
        app,
        &format!("/api/v3/conversations/{session_id}/turn"),
        json!({"message": "   "}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn conversation_and_code_task_control_keep_independent_state() {
    let state = ApiState::new(RunStore::new(temp_root()));
    state.provider_settings.lock().unwrap().mock_mode = true;
    let session = state
        .session_host
        .create_conversation(
            &state,
            ConversationSessionCreateRequest {
                repo: Some(".".to_owned()),
            },
        )
        .unwrap()
        .session;
    let mut output = state
        .session_host
        .subscribe_output(&session.session_id)
        .unwrap();
    let run_id = RunId::from_string("runtime-context-isolation");
    let (control_sender, _control_receiver) =
        tokio::sync::watch::channel(WorkflowRunControl::Running);
    state
        .session_host
        .register_task(&run_id, control_sender)
        .unwrap();

    let response = state
        .session_host
        .conversation_turn(
            &state,
            &session.session_id,
            ConversationTurnRequest {
                message: "Keep talking while code runs".to_owned(),
                repo: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(response.session.turns.len(), 2);
    let events = std::iter::from_fn(|| output.try_recv().ok())
        .map(|event| event.output)
        .collect::<Vec<_>>();
    assert!(matches!(events.first(), Some(OutputEvent::TurnStarted)));
    assert!(events
        .iter()
        .any(|event| matches!(event, OutputEvent::TextCompleted { text } if text.contains("Keep talking"))));
    assert!(events
        .iter()
        .any(|event| matches!(event, OutputEvent::SpeechIntentEnded { .. })));
    assert!(matches!(events.last(), Some(OutputEvent::TurnCompleted)));
    assert!(state.session_host.task_is_active(&run_id));
    state.session_host.deactivate_task(&run_id);
}

#[tokio::test]
async fn conversation_recovers_incremental_history_from_store() {
    let root = temp_root();
    let first_state = ApiState::new(RunStore::new(root.clone()));
    first_state.provider_settings.lock().unwrap().mock_mode = true;
    let session = first_state
        .session_host
        .create_conversation(
            &first_state,
            ConversationSessionCreateRequest { repo: None },
        )
        .unwrap()
        .session;
    first_state
        .session_host
        .conversation_turn(
            &first_state,
            &session.session_id,
            ConversationTurnRequest {
                message: "Persist this".to_owned(),
                repo: None,
            },
        )
        .await
        .unwrap();

    let recovered_state = ApiState::new(RunStore::new(root));
    let recovered = recovered_state
        .session_host
        .get_conversation(&recovered_state, &session.session_id)
        .unwrap()
        .session;

    assert_eq!(recovered.turns.len(), 2);
    assert_eq!(recovered.turns[0].content, "Persist this");
    assert_eq!(
        recovered.turns[1].content,
        "Mock conversation response: Persist this"
    );
}

#[test]
fn recovered_conversations_share_the_bounded_lru_and_release_output_channels() {
    let root = temp_root();
    let state = ApiState::new(RunStore::new(&root));

    for index in 0..=200 {
        let session_id = format!("recovered-session-{index:03}");
        state
            .store
            .append_session_record_next(
                &session_id,
                "session.created",
                json!({"repo_root": ".", "turn_count": 0}),
            )
            .unwrap();
        state
            .session_host
            .get_conversation(&state, &session_id)
            .unwrap();
    }

    assert!(state
        .session_host
        .subscribe_output("recovered-session-000")
        .is_none());
    assert!(state
        .session_host
        .subscribe_output("recovered-session-200")
        .is_some());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn conversation_recovery_reads_a_bounded_tail_and_retains_only_recent_turns() {
    let root = temp_root();
    let state = ApiState::new(RunStore::new(&root));
    let session_id = "bounded-recovery-session";
    state
        .store
        .append_session_record(
            session_id,
            1,
            "session.created",
            json!({"repo_root": ".", "turn_count": 0}),
        )
        .unwrap();
    for index in 0..1_200_u64 {
        state
            .store
            .append_session_record(
                session_id,
                index + 2,
                "session.message.appended",
                json!({
                    "turn": {
                        "role": "user",
                        "content": format!("message-{index}")
                    }
                }),
            )
            .unwrap();
    }

    let recovered = state
        .session_host
        .get_conversation(&state, session_id)
        .unwrap()
        .session;

    assert_eq!(recovered.turns.len(), 64);
    assert_eq!(recovered.turns.first().unwrap().content, "message-1136");
    assert_eq!(recovered.turns.last().unwrap().content, "message-1199");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn conversation_rejects_parallel_turns_and_validates_interrupt_identity() {
    let state = ApiState::new(RunStore::new(temp_root()));
    let base_url = spawn_delayed_openai_compatible_test_server(
        Duration::from_secs(2),
        assistant_payload("too late"),
    )
    .await;
    configure_test_provider(&state, base_url, "conversation-test");
    let session = state
        .session_host
        .create_conversation(&state, ConversationSessionCreateRequest { repo: None })
        .unwrap()
        .session;
    let mut output = state
        .session_host
        .subscribe_output(&session.session_id)
        .unwrap();

    let task_state = state.clone();
    let task_session_id = session.session_id.clone();
    let turn = tokio::spawn(async move {
        let host = task_state.session_host.clone();
        host.conversation_turn(
            &task_state,
            &task_session_id,
            ConversationTurnRequest {
                message: "Wait for control".to_owned(),
                repo: None,
            },
        )
        .await
    });
    let started = output.recv().await.unwrap();
    assert!(matches!(started.output, OutputEvent::TurnStarted));
    let turn_id = started.turn_id.unwrap();

    let parallel_error = state
        .session_host
        .conversation_turn(
            &state,
            &session.session_id,
            ConversationTurnRequest {
                message: "Do not overlap".to_owned(),
                repo: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(parallel_error.status, StatusCode::CONFLICT);

    let wrong_turn_error = state
        .session_host
        .interrupt_conversation_turn(&session.session_id, "wrong-turn")
        .unwrap_err();
    assert_eq!(wrong_turn_error.status, StatusCode::CONFLICT);
    state
        .session_host
        .interrupt_conversation_turn(&session.session_id, &turn_id)
        .unwrap();

    let response = turn.await.unwrap().unwrap();
    assert_eq!(response.status, "cancelled");
    assert_eq!(response.turn_id, turn_id);
    let follow_up = state
        .session_host
        .conversation_turn(
            &state,
            &session.session_id,
            ConversationTurnRequest {
                message: "Control was released".to_owned(),
                repo: None,
            },
        )
        .await;
    assert!(follow_up.is_ok());
}

#[tokio::test]
async fn conversation_steer_is_applied_at_the_next_model_boundary() {
    let state = ApiState::new(RunStore::new(temp_root()));
    let (base_url, captured) = spawn_delayed_sequence_capture_server(vec![
        assistant_payload("first reply"),
        assistant_payload("second reply"),
    ])
    .await;
    configure_test_provider(&state, base_url, "conversation-test");
    let session = state
        .session_host
        .create_conversation(&state, ConversationSessionCreateRequest { repo: None })
        .unwrap()
        .session;
    let mut output = state
        .session_host
        .subscribe_output(&session.session_id)
        .unwrap();

    let task_state = state.clone();
    let task_session_id = session.session_id.clone();
    let turn = tokio::spawn(async move {
        let host = task_state.session_host.clone();
        host.conversation_turn(
            &task_state,
            &task_session_id,
            ConversationTurnRequest {
                message: "Initial request".to_owned(),
                repo: None,
            },
        )
        .await
    });
    let started = output.recv().await.unwrap();
    let turn_id = started.turn_id.unwrap();

    let wrong_turn_error = state
        .session_host
        .steer_conversation_turn(
            &session.session_id,
            "wrong-turn",
            ConversationSteerRequest {
                message: "ignored".to_owned(),
            },
        )
        .unwrap_err();
    assert_eq!(wrong_turn_error.status, StatusCode::CONFLICT);
    let control = state
        .session_host
        .steer_conversation_turn(
            &session.session_id,
            &turn_id,
            ConversationSteerRequest {
                message: "Use this correction".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(control.status, "accepted");

    let response = turn.await.unwrap().unwrap();
    assert_eq!(response.status, "completed");
    assert_eq!(response.assistant_message, "first reply\n\nsecond reply");
    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 2);
    assert!(captured[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "user" && message["content"] == "Use this correction"));
}

fn assistant_payload(content: &str) -> Value {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": content
            }
        }]
    })
}

async fn spawn_delayed_sequence_capture_server(
    payloads: Vec<Value>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    async fn chat_completion(
        State(state): State<DelayedSequenceState>,
        request: Request<Body>,
    ) -> Json<Value> {
        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
        if let Ok(body) = serde_json::from_slice::<Value>(&bytes) {
            state.2.lock().unwrap().push(body);
        }
        tokio::time::sleep(state.0).await;
        Json(
            state
                .1
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| assistant_payload("fallback reply")),
        )
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::new((
        Duration::from_millis(100),
        Mutex::new(VecDeque::from(payloads)),
        captured.clone(),
    ));
    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), captured)
}
