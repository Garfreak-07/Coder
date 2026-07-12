use super::*;

#[tokio::test]
async fn planner_chat_turn_does_not_start_run_and_start_work_does() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let state = ApiState::new(store.clone());
    state.provider_settings.lock().unwrap().mock_mode = true;
    let app = router(state);
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "planner_agent_id": "planner",
            "config": example_config(),
            "mode": "discuss"
        }),
    )
    .await;
    assert_eq!(create_response.status(), StatusCode::OK);
    let create_body = response_json(create_response).await;
    let session_id = create_body["session"]["session_id"].as_str().unwrap();

    let turn_response = post_json(
        app.clone(),
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "Update README.md\nAcceptance: build passes.",
            "confirmed": true,
            "mode": "discuss",
            "planner_agent_id": "planner",
            "config": example_config()
        }),
    )
    .await;
    assert_eq!(turn_response.status(), StatusCode::OK);
    let turn_body = response_json(turn_response).await;
    assert_eq!(turn_body["ready"], true);
    assert_eq!(turn_body["execution_allowed"], false);
    assert_eq!(turn_body["should_start_workflow"], false);
    assert_eq!(turn_body["run_preview"], Value::Null);
    assert!(turn_body.get("run_id").is_none());
    assert!(turn_body.get("events_url").is_none());
    assert!(turn_body.get("timeline_url").is_none());
    assert!(store.list_run_summaries().unwrap().is_empty());

    let start_response = post_json(
        app.clone(),
        &format!("/api/v3/planner-chat/sessions/{session_id}/start-work"),
        json!({
            "repo": ".",
            "workflow_id": "planner-led",
            "planner_agent_id": "planner",
            "config": example_config(),
            "scopes": ["README.md"]
        }),
    )
    .await;
    assert_eq!(start_response.status(), StatusCode::OK);
    let start_body = response_json(start_response).await;
    assert_eq!(start_body["status"], "running");
    assert_eq!(start_body["session"]["work_in_progress"], true);
    let run_id = start_body["run_id"].as_str().unwrap().to_owned();
    assert_eq!(
        start_body["events_url"],
        format!("/api/v3/runs/{run_id}/events")
    );
    assert_eq!(
        start_body["timeline_url"],
        format!("/api/v3/runs/{run_id}/timeline")
    );
    let run_id = RunId::from_string(run_id);
    let mut report = None;
    for _ in 0..200 {
        report = store.read_report(&run_id).unwrap();
        if report.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let report = report.expect("accepted Planner work should finish in the background");
    let events = store.read_events(&run_id).unwrap();
    assert_eq!(events[0].kind, "run.started");
    assert!(events[0].payload.get("plan_context").is_some());
    let handoff_task = events[0].payload["task"].as_str().unwrap();
    assert!(handoff_task.contains("Update README.md"));
    assert!(!handoff_task.contains("Start Work has been clicked"));
    assert!(!handoff_task.contains("Do not execute until Start Work"));
    assert_eq!(
        events[0].payload["plan_context"]["start_work_authorized"],
        true
    );
    assert!(events[0].payload["plan_context"]
        .get("original_user_request")
        .is_none());
    assert!(events[0].payload["plan_context"]["plan_draft"]
        .get("goal")
        .is_none());
    assert_eq!(
        events[0].payload["plan_context"]["plan_draft"]["affected_paths"],
        json!(["README.md"])
    );
    assert!(!report
        .checks
        .iter()
        .any(|check| check.starts_with("plan_context:")));
    assert!(!report
        .checks
        .iter()
        .any(|check| check.starts_with("acceptance:")));
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn planner_chat_turn_remains_available_while_start_work_is_running() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    state.provider_settings.lock().unwrap().mock_mode = true;
    let app = router(state.clone());
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "planner_agent_id": "planner",
            "config": example_config(),
            "mode": "discuss"
        }),
    )
    .await;
    let create_body = response_json(create_response).await;
    let session_id = create_body["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let ready_response = post_json(
        app.clone(),
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "Update PARALLEL.md\nAcceptance: build passes.",
            "confirmed": true,
            "mode": "discuss",
            "planner_agent_id": "planner",
            "config": example_config()
        }),
    )
    .await;
    assert_eq!(ready_response.status(), StatusCode::OK);
    assert_eq!(response_json(ready_response).await["ready"], true);

    let provider_base_url =
        spawn_planner_work_concurrency_test_server(Duration::from_millis(500)).await;
    configure_test_provider(&state, provider_base_url, "test-model");
    let start_uri = format!("/api/v3/planner-chat/sessions/{session_id}/start-work");
    let start_repo = repo_root.display().to_string();
    let start_response = post_json(
        app.clone(),
        &start_uri,
        json!({
            "repo": start_repo,
            "workflow_id": "planner-led",
            "planner_agent_id": "planner",
            "config": example_config(),
            "scopes": ["PARALLEL.md"]
        }),
    )
    .await;
    assert_eq!(start_response.status(), StatusCode::OK);
    let start_body = response_json(start_response).await;
    assert_eq!(start_body["status"], "running");
    let run_id = start_body["run_id"].as_str().unwrap().to_owned();

    let session_uri = format!("/api/v3/planner-chat/sessions/{session_id}");
    let mut observed_running = false;
    for _ in 0..30 {
        let response = get_json(app.clone(), &session_uri).await;
        let body = response_json(response).await;
        if body["session"]["work_in_progress"] == true {
            observed_running = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        observed_running,
        "Start Work never exposed its running state"
    );

    let status_response = post_json(
        app.clone(),
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({"message": "What is the current task status?", "operation": "status"}),
    )
    .await;
    assert_eq!(status_response.status(), StatusCode::OK);
    let status_body = response_json(status_response).await;
    assert_eq!(status_body["provider_trace"], Value::Null);
    assert_eq!(status_body["session"]["work_in_progress"], true);
    assert!(status_body["assistant_message"]
        .as_str()
        .unwrap()
        .contains("progress events"));

    let parallel_message = "While that runs, plan a follow-up update to docs/README.md.";
    let turn_response = post_json(
        app.clone(),
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": parallel_message,
            "confirmed": true,
            "mode": "discuss",
            "planner_agent_id": "planner",
            "config": example_config()
        }),
    )
    .await;
    assert_eq!(turn_response.status(), StatusCode::OK);
    let turn_body = response_json(turn_response).await;
    assert_eq!(turn_body["session"]["work_in_progress"], true);

    let mut final_body = None;
    for _ in 0..200 {
        let response = get_json(app.clone(), &session_uri).await;
        let body = response_json(response).await;
        if body["session"]["work_in_progress"] == false {
            final_body = Some(body);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let final_body = final_body.expect("background Planner work should complete");
    let final_session = &final_body["session"];
    assert_eq!(final_session["work_in_progress"], false);
    assert_eq!(final_session["latest_run_id"], run_id);
    assert_eq!(final_session["ready"], true);
    assert!(final_session["turns"]
        .as_array()
        .unwrap()
        .iter()
        .any(|turn| turn["content"] == parallel_message));
    assert_eq!(
        fs::read_to_string(repo_root.join("PARALLEL.md")).unwrap(),
        "# Parallel Planner Test\n"
    );

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn planner_active_run_supplement_is_queued_without_a_model_request() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let state = ApiState::new(store.clone());
    let run_id = RunId::from_string("run-planner-guidance");
    let mut session = planner_session_fixture("pcs_guidance");
    session.work_in_progress = true;
    session.active_run_id = Some(run_id.to_string());
    store_planner_session_snapshot(
        &mut state.planner_sessions.lock().unwrap(),
        session,
        std::time::SystemTime::now(),
    );
    let (sender, _receiver) = tokio::sync::watch::channel(WorkflowRunControl::Running);
    state
        .active_run_controls
        .lock()
        .unwrap()
        .insert(run_id.to_string(), sender);
    let app = router(state.clone());

    let response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions/pcs_guidance/turn",
        json!({
            "message": "For the current task, also add keyboard controls.",
            "operation": "user_input"
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["provider_trace"], Value::Null);
    assert_eq!(body["session"]["work_in_progress"], true);
    let second_response = post_json(
        app,
        "/api/v3/planner-chat/sessions/pcs_guidance/turn",
        json!({
            "message": "For the current task, also add touch controls.",
            "operation": "user_input"
        }),
    )
    .await;
    assert_eq!(second_response.status(), StatusCode::OK);

    let events = store.read_events(&run_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, "planner.user_guidance.queued");
    let attachments = super::model_tool_async_attachments::drain_planner_user_guidance_attachments(
        &state, &run_id,
    );
    assert_eq!(attachments.len(), 2);
    assert_eq!(attachments[0]["type"], "user_guidance");
    assert!(attachments[0]["prompt"]
        .as_str()
        .unwrap()
        .contains("keyboard controls"));
    assert!(attachments[1]["prompt"]
        .as_str()
        .unwrap()
        .contains("touch controls"));
    assert!(
        super::model_tool_async_attachments::drain_planner_user_guidance_attachments(
            &state, &run_id,
        )
        .is_empty()
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn planner_guidance_finalization_is_atomic_and_reports_unapplied_work() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let state = ApiState::new(store.clone());
    let run_id = RunId::from_string("run-planner-guidance-finalize");
    let (sender, _receiver) = tokio::sync::watch::channel(WorkflowRunControl::Running);
    state
        .active_run_controls
        .lock()
        .unwrap()
        .insert(run_id.to_string(), sender);

    let queued = super::model_tool_async_attachments::queue_planner_user_guidance(
        &state,
        &run_id,
        "Also preserve keyboard navigation.",
    )
    .unwrap();
    assert!(queued.is_some());

    let unapplied =
        super::model_tool_async_attachments::finalize_planner_user_guidance(&state, &run_id);
    assert_eq!(unapplied, vec!["Also preserve keyboard navigation."]);
    assert!(!state
        .active_run_controls
        .lock()
        .unwrap()
        .contains_key(run_id.as_str()));
    assert_eq!(
        super::model_tool_async_attachments::queue_planner_user_guidance(
            &state,
            &run_id,
            "This arrived after finalization.",
        )
        .unwrap(),
        None
    );
    let events = store.read_events(&run_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].kind, "planner.user_guidance.unapplied");
    assert_eq!(events[1].payload["delivery_status"], "unapplied");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_planner_prioritizes_pending_guidance_within_the_round_budget() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let state = ApiState::new(store);
    let run_id = RunId::from_string("run-workflow-planner-pending-guidance");
    let (sender, _receiver) = tokio::sync::watch::channel(WorkflowRunControl::Running);
    state
        .active_run_controls
        .lock()
        .unwrap()
        .insert(run_id.to_string(), sender);
    super::model_tool_async_attachments::queue_planner_user_guidance(
        &state,
        &run_id,
        "Add touch controls.",
    )
    .unwrap();

    let backend = super::workflow_planner_backend::WorkflowPlannerBackend::new(state.clone());
    let result = backend
        .run(workflow_planner_request(&run_id, 1, 3))
        .await
        .unwrap();
    assert_eq!(result.status, "continue");
    let event = result
        .events
        .iter()
        .find(|event| event.kind == "planner.workflow_decision")
        .unwrap();
    assert_eq!(event.payload["decision"], "continue");
    assert_eq!(event.payload["provider_trace"], Value::Null);
    assert_eq!(
        super::model_tool_async_attachments::pending_planner_user_guidance_count(&state, &run_id),
        1
    );
    let _ = super::model_tool_async_attachments::finalize_planner_user_guidance(&state, &run_id);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_planner_blocks_pending_guidance_at_the_round_limit() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let state = ApiState::new(store);
    let run_id = RunId::from_string("run-workflow-planner-final-guidance");
    let (sender, _receiver) = tokio::sync::watch::channel(WorkflowRunControl::Running);
    state
        .active_run_controls
        .lock()
        .unwrap()
        .insert(run_id.to_string(), sender);
    super::model_tool_async_attachments::queue_planner_user_guidance(
        &state,
        &run_id,
        "Add touch controls.",
    )
    .unwrap();

    let backend = super::workflow_planner_backend::WorkflowPlannerBackend::new(state.clone());
    let result = backend
        .run(workflow_planner_request(&run_id, 3, 3))
        .await
        .unwrap();
    assert_eq!(result.status, "blocked");
    let event = result
        .events
        .iter()
        .find(|event| event.kind == "planner.workflow_decision")
        .unwrap();
    assert_eq!(event.payload["decision"], "blocked");
    assert!(event.payload["stop_reason"]
        .as_str()
        .unwrap()
        .contains("maximum round budget"));
    let _ = super::model_tool_async_attachments::finalize_planner_user_guidance(&state, &run_id);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_planner_does_not_extend_an_exhausted_shared_token_budget() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let state = ApiState::new(store);
    let run_id = RunId::from_string("run-workflow-planner-token-budget");
    let (sender, _receiver) = tokio::sync::watch::channel(WorkflowRunControl::Running);
    state
        .active_run_controls
        .lock()
        .unwrap()
        .insert(run_id.to_string(), sender);
    let mut request = workflow_planner_request(&run_id, 1, 3);
    request.backend_context["coder"]["workflow_loop"]["token_budget"] = json!(10);
    super::run_token_budget::record_run_token_usage(
        &state,
        &request,
        super::run_token_budget::RunTokenUsage {
            output_tokens: Some(10),
            ..super::run_token_budget::RunTokenUsage::default()
        },
    );
    super::model_tool_async_attachments::queue_planner_user_guidance(
        &state,
        &run_id,
        "Add touch controls.",
    )
    .unwrap();

    let backend = super::workflow_planner_backend::WorkflowPlannerBackend::new(state.clone());
    let result = backend.run(request).await.unwrap();
    assert_eq!(result.status, "blocked");
    let event = result
        .events
        .iter()
        .find(|event| event.kind == "planner.workflow_decision")
        .unwrap();
    assert_eq!(
        event.payload["stop_reason"],
        "the workflow token budget was exhausted"
    );
    assert_eq!(event.payload["token_budget"]["used_tokens"], 10);
    let _ = super::model_tool_async_attachments::finalize_planner_user_guidance(&state, &run_id);
    super::run_token_budget::clear_run_token_budget(&state, &run_id);
    let _ = fs::remove_dir_all(root);
}

fn workflow_planner_request(run_id: &RunId, round: u32, max_rounds: u32) -> HarnessRunRequest {
    HarnessRunRequest {
        run_id: run_id.clone(),
        workflow_id: "planner-led".to_owned(),
        node_id: "workflow-planner".to_owned(),
        agent_id: "workflow-planner".to_owned(),
        harness_id: "planner-model".to_owned(),
        repo_root: ".".to_owned(),
        task: "Complete the approved plan.".to_owned(),
        backend_context: json!({
            "coder": {
                "agent": {"output_contract": "workflow_decision"},
                "plan_context": {
                    "workflow_feedback": {
                        "source_node_id": "executor",
                        "signal": "completed",
                        "evidence_policy": {"checks_present": true}
                    }
                },
                "workflow_loop": {
                    "round": round,
                    "max_rounds": max_rounds,
                    "executor_evidence_this_round": true
                }
            }
        }),
    }
}

#[tokio::test]
async fn planner_cancel_turn_stops_an_in_flight_start_work_request() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    let mut session = planner_session_fixture("pcs_cancel");
    session.ready = true;
    session.readiness = PlannerReadiness::Ready;
    session.plan_draft = Some(PlanDraft {
        goal: "Create CANCELLED.md".to_owned(),
        execution_mode: PlanExecutionMode::MustWrite,
        review_mode: PlanReviewMode::Standard,
        scope: vec!["CANCELLED.md".to_owned()],
        non_goals: Vec::new(),
        assumptions: Vec::new(),
        steps: vec!["Create the file".to_owned()],
        affected_paths: vec!["CANCELLED.md".to_owned()],
        acceptance_criteria: vec!["CANCELLED.md exists".to_owned()],
        risks: Vec::new(),
        open_questions: Vec::new(),
        selected_workflow_id: "planner-led".to_owned(),
        memory_proposals: Vec::new(),
    });
    store_planner_session_snapshot(
        &mut state.planner_sessions.lock().unwrap(),
        session,
        std::time::SystemTime::now(),
    );
    let provider_base_url =
        spawn_planner_work_concurrency_test_server(Duration::from_secs(5)).await;
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);
    let start_repo = repo_root.display().to_string();
    let start_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions/pcs_cancel/start-work",
        json!({
            "repo": start_repo,
            "workflow_id": "planner-led",
            "planner_agent_id": "planner",
            "config": example_config(),
            "scopes": ["CANCELLED.md"]
        }),
    )
    .await;
    assert_eq!(start_response.status(), StatusCode::OK);
    let start_body = response_json(start_response).await;
    assert_eq!(start_body["status"], "running");
    let run_id = RunId::from_string(start_body["run_id"].as_str().unwrap());

    for _ in 0..100 {
        let response = get_json(app.clone(), "/api/v3/planner-chat/sessions/pcs_cancel").await;
        if response_json(response).await["session"]["work_in_progress"] == true {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let cancel_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions/pcs_cancel/turn",
        json!({"message": "Stop the current task.", "operation": "interrupt"}),
    )
    .await;
    assert_eq!(cancel_response.status(), StatusCode::OK);
    let cancel_body = response_json(cancel_response).await;
    assert_eq!(cancel_body["provider_trace"], Value::Null);
    assert!(cancel_body["assistant_message"]
        .as_str()
        .unwrap()
        .contains("Cancellation was requested"));

    let mut final_session = None;
    for _ in 0..200 {
        let response = get_json(app.clone(), "/api/v3/planner-chat/sessions/pcs_cancel").await;
        let body = response_json(response).await;
        if body["session"]["work_in_progress"] == false {
            final_session = Some(body["session"].clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let final_session = final_session.expect("cancelled Planner work should become idle");
    assert_eq!(final_session["work_in_progress"], false);
    assert_eq!(final_session["active_run_id"], Value::Null);
    assert!(!repo_root.join("CANCELLED.md").exists());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| matches!(
        event.kind.as_str(),
        "run.cancel_requested" | "run.cancelled"
    )));
    assert!(events
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence));
    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn planner_start_work_appends_clarification_when_not_ready() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let app = router(ApiState::new(store.clone()));
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "planner_agent_id": "planner",
            "config": example_config(),
            "mode": "discuss"
        }),
    )
    .await;
    assert_eq!(create_response.status(), StatusCode::OK);
    let create_body = response_json(create_response).await;
    let session_id = create_body["session"]["session_id"].as_str().unwrap();

    let start_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/start-work"),
        json!({
            "repo": ".",
            "workflow_id": "planner-led",
            "planner_agent_id": "planner",
            "config": example_config()
        }),
    )
    .await;

    assert_eq!(start_response.status(), StatusCode::OK);
    let body = response_json(start_response).await;
    assert_eq!(body["run_id"], Value::Null);
    assert_eq!(body["events_url"], Value::Null);
    assert_eq!(body["timeline_url"], Value::Null);
    assert_eq!(body["status"], "needs_clarification");
    assert!(body["assistant_message"]
        .as_str()
        .unwrap()
        .contains("concrete plan"));
    assert_eq!(body["session"]["readiness"], "needs_clarification");
    assert_eq!(body["session"]["turns"].as_array().unwrap().len(), 1);
    assert!(store.list_run_summaries().unwrap().is_empty());
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn planner_start_work_blocks_missing_provider_when_execution_requires_llm() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let state = ApiState::new(store.clone());
    state.provider_settings.lock().unwrap().mock_mode = true;
    let app = router(state.clone());
    let create_response = post_json(
        app.clone(),
        "/api/v3/planner-chat/sessions",
        json!({
            "workflow_id": "planner-led",
            "planner_agent_id": "planner",
            "config": example_config(),
            "mode": "discuss"
        }),
    )
    .await;
    assert_eq!(create_response.status(), StatusCode::OK);
    let create_body = response_json(create_response).await;
    let session_id = create_body["session"]["session_id"].as_str().unwrap();

    let turn_response = post_json(
        app.clone(),
        &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
        json!({
            "message": "Update README.md\nAcceptance: build passes.",
            "confirmed": true,
            "mode": "discuss",
            "planner_agent_id": "planner",
            "config": example_config()
        }),
    )
    .await;
    assert_eq!(turn_response.status(), StatusCode::OK);
    let turn_body = response_json(turn_response).await;
    assert_eq!(turn_body["ready"], true);

    let mut config = default_project_config();
    let model = config.models.get_mut("default").unwrap();
    model.provider = "missing-start-work-provider".to_owned();
    model.base_url_env = Some("CODER_TEST_START_WORK_MISSING_BASE_URL".to_owned());
    model.api_key_env = Some("CODER_TEST_START_WORK_MISSING_API_KEY".to_owned());
    {
        let mut settings = state.provider_settings.lock().unwrap();
        settings.mock_mode = false;
        settings.api_keys.clear();
        settings.base_urls.clear();
    }

    let start_response = post_json(
        app,
        &format!("/api/v3/planner-chat/sessions/{session_id}/start-work"),
        json!({
            "repo": ".",
            "workflow_id": "planner-led",
            "planner_agent_id": "planner",
            "config": config
        }),
    )
    .await;

    assert_eq!(start_response.status(), StatusCode::OK);
    let body = response_json(start_response).await;
    assert_eq!(body["run_id"], Value::Null);
    assert_eq!(body["events_url"], Value::Null);
    assert_eq!(body["timeline_url"], Value::Null);
    assert_eq!(body["status"], "blocked");
    assert!(body["assistant_message"]
        .as_str()
        .unwrap()
        .contains("Configure a provider in Settings before I can plan or execute work."));
    assert_eq!(body["session"]["readiness"], "blocked");
    assert!(store.list_run_summaries().unwrap().is_empty());
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_run_uses_provider_backed_native_tool_write_executor() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let write_args = json!({
        "path": "README.md",
        "content": "# Native Model Executor\n\nCreated by the provider-backed native file writer.\n"
    })
    .to_string();
    let final_content = json!({
        "status": "completed",
        "summary": "Wrote the requested README through a structured tool call.",
        "checks": ["provider_tool_write: completed"],
        "blockers": []
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [{
                        "id": "call-write",
                        "type": "function",
                        "function": {
                            "name": "write_text_file",
                            "arguments": write_args
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 1200,
                "completion_tokens": 300,
                "total_tokens": 1500,
                "prompt_cache_hit_tokens": 800
            }
        }),
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                }
            }]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Create README.md for this repo.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Create README.md for this repo.",
                "plan_draft": {
                    "goal": "Create README.md for this repo.",
                    "affected_paths": ["README.md"]
                },
                "acceptance_criteria": ["README.md exists"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert!(body["report"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some("README.md")));
    assert!(body["report"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check
            .as_str()
            .is_some_and(|text| text.contains("provider_tool_write: completed"))));
    assert_eq!(
        fs::read_to_string(repo_root.join("README.md")).unwrap(),
        "# Native Model Executor\n\nCreated by the provider-backed native file writer.\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "backend.native_rust.started"
            && event.payload["implementation"] == "native-model-tool-loop"
            && event.payload["model_driven"] == true
    }));
    let usage_event = events
        .iter()
        .find(|event| event.kind == "model.provider_turn.completed")
        .expect("provider usage event should be recorded");
    assert_eq!(usage_event.payload["input_tokens"], 1200);
    assert_eq!(usage_event.payload["output_tokens"], 300);
    assert_eq!(usage_event.payload["total_tokens"], 1500);
    assert_eq!(usage_event.payload["cache_read_tokens"], 800);
    assert!(
        usage_event.payload["estimated_input_tokens"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(events.iter().any(|event| {
        event.kind == "file.written"
            && event.payload["implementation"] == "shared-model-tool-runtime"
            && event.payload["path"] == "README.md"
    }));

    let captured = captured.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    let captured_body = &captured[0];
    assert_eq!(captured_body["model"], "test-model");
    assert_eq!(captured_body["max_tokens"], 8000);
    assert!(captured_body.get("thinking").is_none());
    assert!(!captured_body["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("strict JSON"));
    assert!(captured_body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["function"]["name"] == "apply_patch"));
    assert!(captured_body["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("\"start_work_authorized\":true"));
    assert!(!captured_body.to_string().contains("provider-test-token"));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_allows_read_only_root_task_with_custom_native_harness() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    fs::write(
        repo_root.join("README.md"),
        "# Existing Project\n\nRead-only review fixture.\n",
    )
    .unwrap();
    fs::write(repo_root.join("NOTES.md"), "clean fixture\n").unwrap();
    run_git(&repo_root, &["init"]);
    run_git(
        &repo_root,
        &["config", "user.email", "coder@example.invalid"],
    );
    run_git(&repo_root, &["config", "user.name", "Coder Test"]);
    run_git(&repo_root, &["add", "README.md", "NOTES.md"]);
    run_git(&repo_root, &["commit", "-m", "fixture"]);
    fs::write(repo_root.join("NOTES.md"), "sk-live-fixture\n").unwrap();
    let read_args = json!({"path": "README.md"}).to_string();
    let final_content = json!({
        "status": "completed",
        "summary": "Reviewed the existing README without changing the repository.",
        "checks": ["read_only_review: completed"],
        "blockers": []
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [{
                        "id": "call-read",
                        "type": "function",
                        "function": {
                            "name": "repo_read_file",
                            "arguments": read_args
                        }
                    }]
                }
            }]
        }),
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                }
            }]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);
    let mut config = example_config();
    let harnesses = config["harnesses"].as_object_mut().unwrap();
    let native_harness = harnesses.remove("native-code-edit").unwrap();
    harnesses.insert("read-only-native".to_owned(), native_harness);
    config["workflows"]["planner-led"]["nodes"][0]["harness"] = json!("read-only-native");

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": config,
            "workflow_id": "planner-led",
            "task": "Inspect README.md and report what it contains without changing files.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "plan_draft": {
                    "goal": "Review README.md without modifying it.",
                    "affected_paths": ["README.md"],
                    "acceptance_criteria": ["README.md is read and summarized without file changes"]
                }
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed", "{body}");
    assert!(body["report"]["changed_files"]
        .as_array()
        .is_some_and(Vec::is_empty));
    assert!(body["report"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check.as_str() == Some("read_only_review: completed")));
    assert_eq!(
        fs::read_to_string(repo_root.join("README.md")).unwrap(),
        "# Existing Project\n\nRead-only review fixture.\n"
    );
    assert_eq!(
        fs::read_to_string(repo_root.join("NOTES.md")).unwrap(),
        "sk-live-fixture\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "backend.native_rust.started"
            && event.payload["implementation"] == "native-model-tool-loop"
            && event.payload["harness_id"] == "read-only-native"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "repo_read_file"
            && event.payload["status"] == "completed"
    }));
    assert!(!events.iter().any(|event| event.kind == "file.written"));
    assert_eq!(captured.lock().unwrap().len(), 2);

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_uses_provider_backed_planner_for_open_ended_quality_goal() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let write_args = json!({
        "path": "README.md",
        "content": "# Product\n\nA concise project overview.\n"
    })
    .to_string();
    let final_content = json!({
        "status": "completed",
        "summary": "Created the requested README.",
        "checks": ["README.md created"],
        "blockers": []
    })
    .to_string();
    let planner_decision = json!({
        "decision": "finish",
        "summary": "The verified README satisfies the requested quality target.",
        "improvements": [],
        "expected_gain": "none",
        "blockers": []
    });
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [{
                        "id": "call-write",
                        "type": "function",
                        "function": {
                            "name": "write_text_file",
                            "arguments": write_args
                        }
                    }]
                }
            }]
        }),
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                }
            }]
        }),
        json!({
            "choices": [{"message": {"content": planner_decision.to_string()}}],
            "usage": {"prompt_tokens": 240, "completion_tokens": 40, "total_tokens": 280}
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Create a polished README.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Create a polished README.",
                "plan_draft": {
                    "goal": "Create a polished README.",
                    "execution_mode": "must_write",
                    "review_mode": "qualitative",
                    "affected_paths": ["README.md"],
                    "acceptance_criteria": ["README.md exists and presents the project clearly"]
                }
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    let captured = captured.lock().unwrap().clone();
    assert_eq!(captured.len(), 3);
    assert_eq!(captured[0]["max_tokens"], 8000);
    assert_eq!(captured[1]["max_tokens"], 8000);
    assert_eq!(captured[2]["max_tokens"], 900);
    assert!(captured[2]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("medium or high expected gain"));

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    let planner_event = events
        .iter()
        .find(|event| {
            event.kind == "planner.workflow_decision"
                && event.payload["implementation"] == "provider-backed-bounded-planner"
        })
        .expect("provider-backed workflow Planner event should be recorded");
    assert_eq!(planner_event.payload["decision"], "finish");
    assert_eq!(planner_event.payload["provider_trace"]["total_tokens"], 280);

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_blocks_unreviewed_quality_when_workflow_planner_provider_fails() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let write_args = json!({
        "path": "README.md",
        "content": "# Product\n\nA concise project overview.\n"
    })
    .to_string();
    let final_content = json!({
        "status": "completed",
        "summary": "Created the requested README.",
        "checks": ["README.md created"],
        "blockers": []
    })
    .to_string();
    let (provider_base_url, captured) =
        spawn_openai_compatible_status_sequence_capture_test_server(vec![
            OpenAiCompatibleStatusResponse {
                status: StatusCode::OK,
                content_type: "application/json",
                body: json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [{
                                "id": "call-write",
                                "type": "function",
                                "function": {
                                    "name": "write_text_file",
                                    "arguments": write_args
                                }
                            }]
                        }
                    }]
                })
                .to_string(),
            },
            OpenAiCompatibleStatusResponse {
                status: StatusCode::OK,
                content_type: "application/json",
                body: json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                        }
                    }]
                })
                .to_string(),
            },
            OpenAiCompatibleStatusResponse {
                status: StatusCode::PAYMENT_REQUIRED,
                content_type: "application/json",
                body: json!({"error": {"message": "account balance is insufficient"}}).to_string(),
            },
        ])
        .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Create a polished README.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Create a polished README.",
                "plan_draft": {
                    "goal": "Create a polished README.",
                    "execution_mode": "must_write",
                    "review_mode": "qualitative",
                    "affected_paths": ["README.md"],
                    "acceptance_criteria": ["README.md exists and presents the project clearly"]
                }
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "blocked", "{body}");
    assert!(body["report"]["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|blocker| blocker
            .as_str()
            .is_some_and(|text| text.contains("HTTP 402"))));
    assert_eq!(captured.lock().unwrap().len(), 3);
    assert!(repo_root.join("README.md").is_file());

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    let planner_event = events
        .iter()
        .find(|event| event.kind == "planner.workflow_decision")
        .expect("blocked workflow Planner decision should be recorded");
    assert_eq!(planner_event.payload["decision"], "blocked");
    assert!(planner_event.payload["stop_reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("HTTP 402")));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_uses_provider_native_tool_call_loop() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let find_args = json!({"query": "README", "max_results": 10}).to_string();
    let write_args = json!({
        "path": "README.md",
        "content": "# Native Tool Loop\n\nCreated through provider tool calls.\nStatus: draft\n"
    })
    .to_string();
    let edit_args = json!({
        "path": "README.md",
        "edits": [
            {
                "old_string": "Created through provider tool calls.",
                "new_string": "Created and refined through provider tool calls."
            },
            {
                "old_string": "Status: draft",
                "new_string": "Status: ready"
            }
        ]
    })
    .to_string();
    let final_content = json!({
        "status": "completed",
        "summary": "Tool loop wrote README.md.",
        "checks": ["tool_loop: completed"],
        "blockers": []
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-find",
                                "type": "function",
                                "function": {
                                    "name": "repo_find_files",
                                    "arguments": find_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-write",
                                "type": "function",
                                "function": {
                                    "name": "write_text_file",
                                    "arguments": write_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-edit",
                                "type": "function",
                                "function": {
                                    "name": "edit_text_file",
                                    "arguments": edit_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                    }
                }
            ]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Create README.md for this repo.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Create README.md for this repo.",
                "plan_draft": {
                    "goal": "Create README.md for this repo.",
                    "affected_paths": ["README.md"]
                },
                "acceptance_criteria": ["README.md exists"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert!(body["report"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some("README.md")));
    assert!(body["report"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check.as_str() == Some("tool_loop: completed")));
    assert_eq!(
        fs::read_to_string(repo_root.join("README.md")).unwrap(),
        "# Native Tool Loop\n\nCreated and refined through provider tool calls.\nStatus: ready\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_turn.started"
            && event.payload["execution_mode"] == "tool_loop"
            && event.payload["tool_call_count"] == 1
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "repo_find_files"
            && event.payload["status"] == "completed"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "write_text_file"
            && event.payload["status"] == "completed"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.phase"
            && event.payload["phase"] == "permission_decision"
            && event.payload["tool_use_id"] == "call-write"
            && event.payload["permission_policy_source"]["type"] == "turn_context_snapshot"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "edit_text_file"
            && event.payload["status"] == "completed"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "file.written"
            && event.payload["tool_name"] == "edit_text_file"
            && event.payload["operation"] == "exact_string_edit_batch"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "backend.native_rust.completed"
            && event.payload["execution_mode"] == "tool_loop"
            && event.payload["tool_call_count"] == 4
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 4);
    assert!(captured_requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["function"]["name"] == "apply_patch"));
    assert!(!captured_requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| matches!(
            tool["function"]["name"].as_str(),
            Some("write_text_file" | "edit_text_file")
        )));
    assert!(captured_requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool" && message["tool_call_id"] == "call-find"));
    assert!(captured_requests[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool" && message["tool_call_id"] == "call-write"));
    assert!(captured_requests[3]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool" && message["tool_call_id"] == "call-edit"));
    assert!(!serde_json::to_string(&captured_requests)
        .unwrap()
        .contains("provider-test-token"));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_recovers_truncated_native_tool_arguments_in_smaller_pieces() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let valid_write = json!({
        "path": "README.md",
        "content": "# Recovered write\n"
    })
    .to_string();
    let final_content = json!({
        "status": "completed",
        "summary": "Recovered the truncated write.",
        "checks": ["output_limit_recovery: completed"],
        "blockers": []
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [{
                "finish_reason": "length",
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [{
                        "id": "call-truncated-write",
                        "type": "function",
                        "function": {
                            "name": "write_text_file",
                            "arguments": "{\"path\":\"README.md\",\"content\":\"# Cut"
                        }
                    }]
                }
            }]
        }),
        json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [{
                        "id": "call-recovered-write",
                        "type": "function",
                        "function": {
                            "name": "write_text_file",
                            "arguments": valid_write
                        }
                    }]
                }
            }]
        }),
        json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]}
            }]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);
    let mut config = example_config();
    config["agents"]["executor"]["runtime"]["max_output_recovery_attempts"] = json!(1);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": config,
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Create README.md for this repo.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Create README.md for this repo.",
                "plan_draft": {
                    "goal": "Create README.md for this repo.",
                    "affected_paths": ["README.md"]
                }
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert_eq!(
        fs::read_to_string(repo_root.join("README.md")).unwrap(),
        "# Recovered write\n"
    );

    let captured = captured.lock().unwrap().clone();
    assert_eq!(captured.len(), 3);
    assert!(captured[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "user"
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains("smaller atomic apply_patch calls"))));

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model.output_limit.recovery"
            && event.payload["attempt"] == 1
            && event.payload["max_attempts"] == 1
    }));
    let truncated_event = events
        .iter()
        .find(|event| {
            event.kind == "model.tool_call.completed"
                && event.payload["tool_call_id"] == "call-truncated-write"
        })
        .unwrap_or_else(|| {
            panic!(
                "missing truncated tool event: {:?}",
                events
                    .iter()
                    .filter(|event| event.kind == "model.tool_call.completed")
                    .map(|event| &event.payload)
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(truncated_event.payload["is_error"], true);
    assert!(
        truncated_event.payload["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("provider output limit truncated")),
        "{}",
        truncated_event.payload
    );

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_provider_native_repo_read_file_uses_shared_pre_tool_hook() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    fs::write(repo_root.join("before.txt"), "before-provider-hook\n").unwrap();
    fs::write(repo_root.join("after.txt"), "after-provider-hook\n").unwrap();

    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Allow;
    let hook_response = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "updatedInput": {
                "repo_root": repo_root.display().to_string(),
                "path": "after.txt"
            },
            "additionalContext": "provider repo read hook context"
        }
    });
    let (hook_url, hook_capture) = spawn_webhook_test_server(hook_response).await;
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("repo_read_file".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: hook_url,
                if_condition: None,
                timeout: Some(5),
                headers: BTreeMap::new(),
                allowed_env_vars: Vec::new(),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };

    let read_args = json!({"path": "before.txt"}).to_string();
    let write_args = json!({
        "path": "HOOK-READ.md",
        "content": "# Shared Repo Read Hook\n\nProvider observed the hook-updated read result.\n"
    })
    .to_string();
    let final_content = json!({
        "status": "completed",
        "summary": "Provider repo read tool used the shared hook pipeline.",
        "checks": ["repo_read_pre_hook: updated_input_observed"],
        "blockers": []
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-read-hook",
                                "type": "function",
                                "function": {
                                    "name": "repo_read_file",
                                    "arguments": read_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-write-hook-read",
                                "type": "function",
                                "function": {
                                    "name": "write_text_file",
                                    "arguments": write_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                    }
                }
            ]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": serde_json::to_value(config).unwrap(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Read a file, then write HOOK-READ.md.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Read a file, then write HOOK-READ.md.",
                "plan_draft": {
                    "goal": "Read a file, then write HOOK-READ.md.",
                    "affected_paths": ["HOOK-READ.md"]
                },
                "acceptance_criteria": ["HOOK-READ.md exists"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert_eq!(
        fs::read_to_string(repo_root.join("HOOK-READ.md")).unwrap(),
        "# Shared Repo Read Hook\n\nProvider observed the hook-updated read result.\n"
    );

    let hook_capture = hook_capture.lock().unwrap();
    let hook_input = hook_capture.body.as_ref().unwrap();
    assert_eq!(hook_input["hook_event_name"], "PreToolUse");
    assert_eq!(hook_input["tool_name"], "repo_read_file");
    assert_eq!(hook_input["tool_use_id"], "call-read-hook");
    assert_eq!(hook_input["tool_input"]["path"], "before.txt");
    drop(hook_capture);

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.phase"
            && event.payload["phase"].as_str() == Some("pre_tool_use_hooks")
            && event.payload["tool_name"].as_str() == Some("repo_read_file")
            && event.payload["updated_input_applied"] == true
            && event.payload["webhook_hook_count"] == 1
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 3);
    let read_tool_result = captured_requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool" && message["tool_call_id"] == "call-read-hook")
        .and_then(|message| message["content"].as_str())
        .unwrap();
    assert!(read_tool_result.contains("after-provider-hook"));
    assert!(!read_tool_result.contains("before-provider-hook"));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_uses_provider_native_background_command_tools() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let (provider_base_url, captured) = spawn_native_background_command_tool_loop_test_server(
        platform_delayed_echo_args("native-bg-done"),
    )
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Run a background check, observe it, then write BACKGROUND.md.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Run a background check and write BACKGROUND.md.",
                "plan_draft": {
                    "goal": "Run a background check and write BACKGROUND.md.",
                    "affected_paths": ["BACKGROUND.md"]
                },
                "acceptance_criteria": ["BACKGROUND.md exists", "background command output was observed"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert!(body["report"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some("BACKGROUND.md")));
    assert!(body["report"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check.as_str() == Some("command_background: observed")));
    assert_eq!(
        fs::read_to_string(repo_root.join("BACKGROUND.md")).unwrap(),
        "# Background Command\n\nObserved native-bg-done through shared tool execution.\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "command_background"
            && event.payload["status"] == "running"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "read_command_output"
            && event.payload["status"] == "completed"
            && event
                .refs
                .iter()
                .any(|reference| reference.label == "repo_evidence")
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "write_text_file"
            && event.payload["status"] == "completed"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "backend.native_rust.completed"
            && event.payload["execution_mode"] == "tool_loop"
            && event.payload["tool_call_count"] == 4
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 3);
    let tool_names = captured_requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"command_background"));
    assert!(tool_names.contains(&"read_command_output"));
    assert!(captured_requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "call-bg"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("\"task_id\""))
        }));
    assert!(captured_requests[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "call-bg-output"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("native-bg-done"))
        }));
    assert!(!serde_json::to_string(&captured_requests)
        .unwrap()
        .contains("provider-test-token"));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_uses_provider_native_skill_tool() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let skill_args = json!({"skill": "coder.repo-review"}).to_string();
    let write_args = json!({
        "path": "SKILL.md",
        "content": "# Native Skill Tool\n\nSkill context was observed through shared tool execution.\n"
    })
    .to_string();
    let final_content = json!({
        "status": "completed",
        "summary": "Skill tool context was observed and SKILL.md was written.",
        "checks": ["skill_tool: observed"],
        "blockers": []
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-skill",
                                "type": "function",
                                "function": {
                                    "name": "Skill",
                                    "arguments": skill_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-skill-write",
                                "type": "function",
                                "function": {
                                    "name": "write_text_file",
                                    "arguments": write_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                    }
                }
            ]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Invoke a skill, observe it, then write SKILL.md.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Invoke a skill and write SKILL.md.",
                "plan_draft": {
                    "goal": "Invoke a skill and write SKILL.md.",
                    "affected_paths": ["SKILL.md"]
                },
                "acceptance_criteria": ["SKILL.md exists", "skill output was observed"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert!(body["report"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some("SKILL.md")));
    assert!(body["report"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check.as_str() == Some("skill_tool: observed")));
    assert_eq!(
        fs::read_to_string(repo_root.join("SKILL.md")).unwrap(),
        "# Native Skill Tool\n\nSkill context was observed through shared tool execution.\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "skill"
            && event.payload["status"] == "completed"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "write_text_file"
            && event.payload["status"] == "completed"
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 3);
    let tool_names = captured_requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"skill"));
    assert!(tool_names.contains(&"agent_subagent"));
    assert!(captured_requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "call-skill"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("coder.repo-review"))
        }));
    assert!(!serde_json::to_string(&captured_requests)
        .unwrap()
        .contains("provider-test-token"));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_uses_provider_native_subagent_tool() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let agent_args = json!({
        "prompt": "Review the current plan briefly and report whether creating SUBAGENT.md is in scope.",
        "description": "scope reviewer",
        "subagent_type": "reviewer"
    })
    .to_string();
    let write_args = json!({
        "path": "SUBAGENT.md",
        "content": "# Native Subagent Tool\n\nSubagent context was observed through shared tool execution.\n"
    })
    .to_string();
    let final_content = json!({
        "status": "completed",
        "summary": "Subagent output was observed and SUBAGENT.md was written.",
        "checks": ["subagent_tool: observed"],
        "blockers": []
    })
    .to_string();
    let child_final_content = json!({
        "status": "completed",
        "summary": "Subagent confirmed SUBAGENT.md is in scope.",
        "checks": ["subagent_child: reviewed"],
        "blockers": []
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-agent",
                                "type": "function",
                                "function": {
                                    "name": "Agent",
                                    "arguments": agent_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null, "tool_calls": [native_finish_tool_call("call-child-finish", &child_final_content)]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-agent-write",
                                "type": "function",
                                "function": {
                                    "name": "write_text_file",
                                    "arguments": write_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                    }
                }
            ]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Ask a subagent to review scope, then write SUBAGENT.md.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Ask a subagent to review scope and write SUBAGENT.md.",
                "plan_draft": {
                    "goal": "Ask a subagent to review scope and write SUBAGENT.md.",
                    "affected_paths": ["SUBAGENT.md"]
                },
                "acceptance_criteria": ["SUBAGENT.md exists", "subagent output was observed"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert!(body["report"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some("SUBAGENT.md")));
    assert!(body["report"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check.as_str() == Some("subagent_tool: observed")));
    assert_eq!(
        fs::read_to_string(repo_root.join("SUBAGENT.md")).unwrap(),
        "# Native Subagent Tool\n\nSubagent context was observed through shared tool execution.\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    let agent_event = events
        .iter()
        .find(|event| {
            event.kind == "model.tool_call.completed"
                && event.payload["tool_name"] == "agent_subagent"
                && event.payload["status"] == "completed"
        })
        .expect("agent_subagent tool call should complete");
    assert!(agent_event
        .refs
        .iter()
        .any(|reference| reference.label == "subagent_metadata"));
    assert!(agent_event
        .refs
        .iter()
        .any(|reference| reference.label == "subagent_transcript"));
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "write_text_file"
            && event.payload["status"] == "completed"
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert!(captured_requests.len() >= 4);
    let tools = captured_requests[0]["tools"].as_array().unwrap();
    let tool_names = tools
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"agent_subagent"));
    assert!(tool_names.contains(&"read_subagent_status"));
    let tool_description = |name: &str| {
        tools
            .iter()
            .find(|tool| tool["function"]["name"] == name)
            .and_then(|tool| tool["function"]["description"].as_str())
            .unwrap()
    };
    assert!(tool_description("agent_subagent").contains("synchronous result is final"));
    assert!(tool_description("read_subagent_status").contains("background_task.task_id"));
    assert!(tool_description("read_subagent_status").contains("never agent_id"));
    assert!(captured_requests.iter().any(|request| {
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["tool_call_id"] == "call-agent"
                    && message["content"].as_str().is_some_and(|content| {
                        content.contains("metadata_ref") && content.contains("transcript_ref")
                    })
            })
    }));
    assert!(!serde_json::to_string(&captured_requests)
        .unwrap()
        .contains("provider-test-token"));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_uses_provider_native_background_subagent_status_tool() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let (provider_base_url, captured) = spawn_native_background_subagent_status_test_server().await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Launch a background subagent, wait for its output, then write BG-SUBAGENT.md.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Launch a background subagent and write BG-SUBAGENT.md.",
                "plan_draft": {
                    "goal": "Launch a background subagent and write BG-SUBAGENT.md.",
                    "affected_paths": ["BG-SUBAGENT.md"]
                },
                "acceptance_criteria": ["BG-SUBAGENT.md exists", "background subagent output was observed"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert!(body["report"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some("BG-SUBAGENT.md")));
    assert!(body["report"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check.as_str() == Some("background_subagent_status: observed")));
    assert_eq!(
        fs::read_to_string(repo_root.join("BG-SUBAGENT.md")).unwrap(),
        "# Background Subagent\n\nThe explicit status tool observed the background subagent.\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "agent_subagent"
            && event.payload["status"] == "backgrounded"
    }));
    let output_event = events
        .iter()
        .find(|event| {
            event.kind == "model.tool_call.completed"
                && event.payload["tool_name"] == "read_subagent_status"
                && event.payload["status"] == "completed"
        })
        .unwrap_or_else(|| {
            panic!("explicit subagent status should use shared tool execution: {events:?}")
        });
    assert!(output_event
        .refs
        .iter()
        .any(|reference| reference.label == "subagent_metadata"));
    assert!(output_event
        .refs
        .iter()
        .any(|reference| reference.label == "subagent_transcript"));

    let captured_requests = captured.lock().unwrap().clone();
    assert!(captured_requests.len() >= 4);
    assert!(captured_requests.iter().any(|request| {
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["tool_call_id"] == "call-bg-agent"
                    && message["content"].as_str().is_some_and(|content| {
                        content.contains("\"background_task\"") && content.contains("\"task_id\"")
                    })
            })
    }));
    assert!(captured_requests.iter().any(|request| {
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["tool_call_id"] == "call-bg-agent-output"
                    && message["content"]
                        .as_str()
                        .is_some_and(|content| content.contains("retrieval_status"))
            })
    }));
    assert!(!serde_json::to_string(&captured_requests)
        .unwrap()
        .contains("provider-test-token"));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_aggregates_background_subagent_report_changes() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    fs::write(repo_root.join("README.md"), "base\n").unwrap();
    run_git(&repo_root, &["init"]);
    run_git(&repo_root, &["config", "user.email", "coder@example.test"]);
    run_git(&repo_root, &["config", "user.name", "Coder Test"]);
    run_git(&repo_root, &["add", "README.md"]);
    run_git(&repo_root, &["commit", "-m", "base"]);
    let (provider_base_url, captured) =
        spawn_native_background_subagent_writes_file_test_server().await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app.clone(),
        "/api/v3/workflows/run",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Launch a background subagent that writes CHILD-ONLY.md, wait for it, and finish without writing that file in the parent.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Use a background subagent to write CHILD-ONLY.md.",
                "plan_draft": {
                    "goal": "Use a background subagent to write CHILD-ONLY.md.",
                    "affected_paths": ["CHILD-ONLY.md"]
                },
                "acceptance_criteria": ["CHILD-ONLY.md exists", "Review Changes includes CHILD-ONLY.md"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert!(body["report"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some("CHILD-ONLY.md")));
    assert_eq!(
        fs::read_to_string(repo_root.join("CHILD-ONLY.md")).unwrap(),
        "# Child Only\n\nThis file was written by the background subagent.\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let changes_response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v3/runs/{}/changes", run_id.as_str()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let changes_status = changes_response.status();
    let changes_body = response_json(changes_response).await;
    assert_eq!(changes_status, StatusCode::OK, "{changes_body}");
    assert!(changes_body["changes"][0]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"].as_str() == Some("CHILD-ONLY.md")));

    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "agent_subagent"
            && event.payload["status"] == "backgrounded"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "read_subagent_status"
            && event.payload["status"] == "completed"
    }));
    let captured_requests = captured.lock().unwrap().clone();
    assert!(captured_requests.iter().any(|request| {
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["tool_call_id"] == "call-parent-output"
                    && message["content"].as_str().is_some_and(|content| {
                        content.contains("\"changed_files\"")
                            && content.contains("\"CHILD-ONLY.md\"")
                    })
            })
    }));
    assert!(!serde_json::to_string(&captured_requests)
        .unwrap()
        .contains("provider-test-token"));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_uses_provider_native_background_subagent_cancel_tool() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let (provider_base_url, captured) =
        spawn_native_background_subagent_cancel_test_server(platform_sleep_args()).await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Launch a background subagent, cancel it, then write BG-SUBAGENT-CANCEL.md.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Launch and cancel a background subagent, then write BG-SUBAGENT-CANCEL.md.",
                "plan_draft": {
                    "goal": "Launch and cancel a background subagent.",
                    "affected_paths": ["BG-SUBAGENT-CANCEL.md"]
                },
                "acceptance_criteria": ["BG-SUBAGENT-CANCEL.md exists", "background subagent was cancelled"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert!(
        body["report"]["changed_files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path.as_str() == Some("BG-SUBAGENT-CANCEL.md")),
        "{body}"
    );
    assert!(body["report"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check.as_str() == Some("background_subagent_cancel: cancelled")));
    assert_eq!(
        fs::read_to_string(repo_root.join("BG-SUBAGENT-CANCEL.md")).unwrap(),
        "# Background Subagent Cancelled\n\nThe explicit cancel tool stopped the background subagent.\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "agent_subagent"
            && event.payload["status"] == "backgrounded"
    }));
    let stop_event = events
        .iter()
        .find(|event| {
            event.kind == "model.tool_call.completed"
                && event.payload["tool_name"] == "cancel_subagent_background"
                && event.payload["status"] == "cancelled"
        })
        .expect("explicit cancel tool should stop the background subagent");
    let stop_summary = stop_event.payload["summary"].as_str().unwrap();
    let stop_payload = serde_json::from_str::<Value>(stop_summary).unwrap();
    assert_eq!(stop_payload["status"], "cancelled");
    assert_eq!(stop_payload["cancelled"], true);

    let captured_requests = captured.lock().unwrap().clone();
    let task_id = captured_requests
        .iter()
        .find_map(|request| native_task_id_from_provider_request(request, "call-cancel-bg-agent"))
        .expect("background subagent task id should be visible to parent model turn");
    let record = store
        .read_subagent_background_task_record(&task_id)
        .unwrap()
        .expect("background subagent task should be durable");
    assert_eq!(record.status, "cancelled");
    let metadata = store
        .read_subagent_metadata(&run_id, &record.agent_id)
        .unwrap()
        .expect("cancelled subagent metadata should be durable");
    assert_eq!(metadata.status.as_deref(), Some("cancelled"));
    assert_eq!(
        metadata.terminal_record_kind.as_deref(),
        Some("subagent.cancelled")
    );
    let transcript = store
        .read_subagent_transcript_records(&run_id, &record.agent_id)
        .unwrap();
    assert!(transcript
        .iter()
        .any(|record| record.kind == "subagent.cancelled"));
    assert!(captured_requests
        .iter()
        .any(provider_request_is_child_cancellation_probe));
    assert!(!serde_json::to_string(&captured_requests)
        .unwrap()
        .contains("provider-test-token"));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_provider_native_shared_tool_respects_pre_tool_use_blocking_hook() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let block_hook = if cfg!(windows) {
        HookTestCommand {
            shell: "powershell".to_owned(),
            command: "Write-Output native-provider-pre-hook-blocked; exit 2".to_owned(),
        }
    } else {
        HookTestCommand {
            shell: "sh".to_owned(),
            command: "printf native-provider-pre-hook-blocked; exit 2".to_owned(),
        }
    };
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("command_run".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Command {
                command: block_hook.command,
                if_condition: None,
                shell: Some(block_hook.shell),
                timeout: None,
                status_message: None,
                once: false,
                run_async: false,
                async_rewake: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    let command_args = json!({
        "argv": platform_write_file_args("command-ran.txt", "ran")
    })
    .to_string();
    let final_content = json!({
        "status": "blocked",
        "summary": "PreToolUse hook blocked the command.",
        "checks": ["pre_tool_use_hook: blocked"],
        "blockers": ["native-provider-pre-hook-blocked"]
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-pre-hook-command",
                                "type": "function",
                                "function": {
                                    "name": "command_run",
                                    "arguments": command_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                    }
                }
            ]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": serde_json::to_value(config).unwrap(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Run a command, but a pre hook should block it.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Run a command, but a pre hook should block it.",
                "plan_draft": {
                    "goal": "Run a command, but a pre hook should block it.",
                    "affected_paths": ["command-ran.txt"]
                },
                "acceptance_criteria": ["command-ran.txt must not be created"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "blocked");
    assert!(!repo_root.join("command-ran.txt").exists());

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "command_run"
            && event.payload["status"] == "blocked"
            && event.payload["is_error"] == true
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.phase"
            && event.payload["phase"].as_str() == Some("pre_tool_use_hooks")
            && event.payload["status"].as_str() == Some("blocked")
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 2);
    assert!(captured_requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "call-pre-hook-command"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("native-provider-pre-hook-blocked"))
        }));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_provider_native_shared_tool_returns_post_hook_updated_output() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let hook_output = json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedMCPToolOutput": {
                "replacement": "native provider post hook output"
            },
            "additionalContext": "native provider post context"
        }
    });
    let hook_output_path = repo_root.join("post-hook-output.json");
    fs::write(
        &hook_output_path,
        serde_json::to_string(&hook_output).unwrap(),
    )
    .unwrap();
    let hook_command = hook_emit_file_command(&hook_output_path);
    config.hooks = coder_config::HookSettings {
        post_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("command_run".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Command {
                command: hook_command.command,
                if_condition: None,
                shell: Some(hook_command.shell),
                timeout: None,
                status_message: None,
                once: false,
                run_async: false,
                async_rewake: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    let command_args = json!({
        "argv": platform_echo_args("original command output")
    })
    .to_string();
    let write_args = json!({
        "path": "POST-HOOK.md",
        "content": "# Post Hook\n\nThe model observed the post hook replacement output.\n"
    })
    .to_string();
    let final_content = json!({
        "status": "completed",
        "summary": "PostToolUse hook output was returned to the provider loop.",
        "checks": ["post_tool_use_hook: updated_output_observed"],
        "blockers": []
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-post-hook-command",
                                "type": "function",
                                "function": {
                                    "name": "command_run",
                                    "arguments": command_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-post-hook-write",
                                "type": "function",
                                "function": {
                                    "name": "write_text_file",
                                    "arguments": write_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                    }
                }
            ]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": serde_json::to_value(config).unwrap(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Run a command, observe post hook output, then write POST-HOOK.md.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Run a command, observe post hook output, then write POST-HOOK.md.",
                "plan_draft": {
                    "goal": "Run a command, observe post hook output, then write POST-HOOK.md.",
                    "affected_paths": ["POST-HOOK.md"]
                },
                "acceptance_criteria": ["POST-HOOK.md exists", "post hook output was observed"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert!(body["report"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some("POST-HOOK.md")));
    assert_eq!(
        fs::read_to_string(repo_root.join("POST-HOOK.md")).unwrap(),
        "# Post Hook\n\nThe model observed the post hook replacement output.\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.phase"
            && event.payload["phase"].as_str() == Some("post_tool_use_hooks")
            && event.payload["updated_tool_output_applied"] == true
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 3);
    assert!(captured_requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "call-post-hook-command"
                && message["content"].as_str().is_some_and(|content| {
                    content.contains("native provider post hook output")
                        && !content.contains("original command output")
                })
        }));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_provider_native_shared_tool_respects_prompt_pre_tool_use_blocking_hook() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    fs::write(repo_root.join("README.md"), "# Prompt Provider Hook\n").unwrap();
    let mut config = default_project_config();
    config.models.insert(
        "hook_verifier".to_owned(),
        ConfigModelSpec {
            provider: "openai-compatible".to_owned(),
            model: "prompt-hook-model".to_owned(),
            base_url_env: None,
            api_key_env: None,
            capabilities: coder_config::ModelCapabilities::default(),
        },
    );
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("repo_read_file".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Prompt {
                prompt: "Reject this provider-loop read: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: Some(5),
                model: Some("hook_verifier".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    let read_args = json!({"path": "README.md"}).to_string();
    let final_content = json!({
        "status": "blocked",
        "summary": "Prompt hook blocked the provider read.",
        "checks": ["prompt_pre_tool_use_hook: blocked"],
        "blockers": ["provider-loop prompt hook rejected read"]
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-prompt-pre-hook-read",
                                "type": "function",
                                "function": {
                                    "name": "repo_read_file",
                                    "arguments": read_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": "{\"ok\": false, \"reason\": \"provider-loop prompt hook rejected read\"}"
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                    }
                }
            ]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": serde_json::to_value(config).unwrap(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Read README.md, but a prompt hook should block it.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Read README.md, but a prompt hook should block it.",
                "plan_draft": {
                    "goal": "Read README.md, but a prompt hook should block it.",
                    "affected_paths": []
                },
                "acceptance_criteria": ["run reports blocked"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "blocked");
    assert!(body["report"]["summary"]
        .as_str()
        .unwrap()
        .contains("provider-loop prompt hook rejected read"));

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "repo_read_file"
            && event.payload["status"] == "blocked"
            && event.payload["is_error"] == true
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.phase"
            && event.payload["phase"].as_str() == Some("pre_tool_use_hooks")
            && event.payload["tool_name"].as_str() == Some("repo_read_file")
            && event.payload["status"].as_str() == Some("blocked")
            && event.payload["prompt_hook_count"] == 1
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 3);
    assert_eq!(captured_requests[0]["model"], "test-model");
    assert_eq!(captured_requests[1]["model"], "prompt-hook-model");
    assert!(captured_requests[1]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("evaluating a hook in Claude Code"));
    assert!(captured_requests[1]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("call-prompt-pre-hook-read"));
    assert!(captured_requests[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "call-prompt-pre-hook-read"
                && message["content"].as_str().is_some_and(|content| {
                    content.contains("provider-loop prompt hook rejected read")
                        && content.contains("<tool_use_error>")
                })
        }));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_provider_native_shared_tool_respects_agent_pre_tool_use_blocking_hook() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    fs::write(repo_root.join("README.md"), "# Agent Provider Hook\n").unwrap();
    let mut config = default_project_config();
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
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("repo_read_file".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Agent {
                prompt: "Agent-check this provider-loop read: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: Some(5),
                model: Some("hook_agent".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    let read_args = json!({"path": "README.md"}).to_string();
    let final_content = json!({
        "status": "blocked",
        "summary": "Agent hook blocked the provider read.",
        "checks": ["agent_pre_tool_use_hook: blocked"],
        "blockers": ["provider-loop agent hook rejected read"]
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-agent-pre-hook-read",
                                "type": "function",
                                "function": {
                                    "name": "repo_read_file",
                                    "arguments": read_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-agent-hook-structured-output",
                                "type": "function",
                                "function": {
                                    "name": "StructuredOutput",
                                    "arguments": "{\"ok\": false, \"reason\": \"provider-loop agent hook rejected read\"}"
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                    }
                }
            ]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": serde_json::to_value(config).unwrap(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Read README.md, but an agent hook should block it.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Read README.md, but an agent hook should block it.",
                "plan_draft": {
                    "goal": "Read README.md, but an agent hook should block it.",
                    "affected_paths": []
                },
                "acceptance_criteria": ["run reports blocked"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "blocked");
    assert!(body["report"]["summary"]
        .as_str()
        .unwrap()
        .contains("provider-loop agent hook rejected read"));

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "repo_read_file"
            && event.payload["status"] == "blocked"
            && event.payload["is_error"] == true
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.phase"
            && event.payload["phase"].as_str() == Some("pre_tool_use_hooks")
            && event.payload["tool_name"].as_str() == Some("repo_read_file")
            && event.payload["status"].as_str() == Some("blocked")
            && event.payload["agent_hook_count"] == 1
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 3);
    assert_eq!(captured_requests[0]["model"], "test-model");
    assert_eq!(captured_requests[1]["model"], "agent-hook-model");
    assert!(captured_requests[1]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("verifying a hook condition in Claude Code"));
    assert!(captured_requests[1]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("call-agent-pre-hook-read"));
    let agent_hook_tools = captured_requests[1]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(agent_hook_tools.contains(&"StructuredOutput"));
    assert!(agent_hook_tools.contains(&"repo_read_file"));
    assert!(!agent_hook_tools.contains(&"agent_subagent"));
    assert!(!agent_hook_tools.contains(&"command_run"));
    assert!(captured_requests[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "call-agent-pre-hook-read"
                && message["content"].as_str().is_some_and(|content| {
                    content.contains("provider-loop agent hook rejected read")
                        && content.contains("<tool_use_error>")
                })
        }));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_provider_native_shared_tool_runs_async_command_hook_without_blocking() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let stdin_capture = repo_root.join("provider-async-hook-input.json");
    let sentinel = repo_root.join("provider-async-hook-done.txt");
    let hook_command = hook_async_capture_stdin_command(&stdin_capture, &sentinel);
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("command_run".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Command {
                command: hook_command.command,
                if_condition: None,
                shell: Some(hook_command.shell),
                timeout: Some(5),
                status_message: None,
                once: false,
                run_async: true,
                async_rewake: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    let command_args = json!({
        "argv": platform_echo_args("provider-async-hook-command-output")
    })
    .to_string();
    let write_args = json!({
        "path": "ASYNC-HOOK.md",
        "content": "# Async Hook\n\nThe async command hook did not block provider execution.\n"
    })
    .to_string();
    let final_content = json!({
        "status": "completed",
        "summary": "Async hook did not block provider execution.",
        "checks": ["async_pre_tool_use_hook: backgrounded"],
        "blockers": []
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-async-hook-command",
                                "type": "function",
                                "function": {
                                    "name": "command_run",
                                    "arguments": command_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-async-hook-write",
                                "type": "function",
                                "function": {
                                    "name": "write_text_file",
                                    "arguments": write_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                    }
                }
            ]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": serde_json::to_value(config).unwrap(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Run a command, then write ASYNC-HOOK.md.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Run a command, then write ASYNC-HOOK.md.",
                "plan_draft": {
                    "goal": "Run a command, then write ASYNC-HOOK.md.",
                    "affected_paths": ["ASYNC-HOOK.md"]
                },
                "acceptance_criteria": ["ASYNC-HOOK.md exists"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert_eq!(
        fs::read_to_string(repo_root.join("ASYNC-HOOK.md")).unwrap(),
        "# Async Hook\n\nThe async command hook did not block provider execution.\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    let phase_event = events
        .iter()
        .find(|event| {
            event.kind == "model_tool.phase"
                && event.payload["phase"].as_str() == Some("pre_tool_use_hooks")
                && event.payload["tool_name"].as_str() == Some("command_run")
        })
        .expect("command_run pre hook phase should be recorded");
    assert_eq!(phase_event.payload["status"], "completed");
    assert_eq!(
        phase_event.payload["hook_results"][0]["outcome"],
        "backgrounded"
    );
    assert_eq!(phase_event.payload["hook_results"][0]["async"], true);
    let async_hook_id = phase_event.payload["hook_results"][0]["async_hook_id"]
        .as_str()
        .unwrap()
        .to_owned();

    wait_for_path(&sentinel).await;
    let completed_events = wait_for_events(&store, &run_id, |events| {
        events.iter().any(|event| {
            event.kind == "model_tool.async_hook.completed"
                && event.payload["async_hook_id"].as_str() == Some(async_hook_id.as_str())
        })
    })
    .await;
    assert!(completed_events.iter().any(|event| {
        event.kind == "model_tool.async_hook.started"
            && event.payload["async_hook_id"].as_str() == Some(async_hook_id.as_str())
    }));
    assert!(completed_events.iter().any(|event| {
        event.kind == "model_tool.async_hook.completed"
            && event.payload["async_hook_id"].as_str() == Some(async_hook_id.as_str())
            && event.payload["outcome"].as_str() == Some("success")
    }));
    let captured_stdin = fs::read_to_string(&stdin_capture).unwrap();
    let hook_input =
        serde_json::from_str::<Value>(captured_stdin.trim_start_matches('\u{feff}').trim())
            .unwrap();
    assert_eq!(hook_input["hook_event_name"], "PreToolUse");
    assert_eq!(hook_input["tool_name"], "command_run");
    assert_eq!(hook_input["tool_use_id"], "call-async-hook-command");

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 3);
    assert!(captured_requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "call-async-hook-command"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("provider-async-hook-command-output"))
        }));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_provider_native_delivers_async_rewake_hook_to_next_model_turn() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let hook_command = if cfg!(windows) {
        HookTestCommand {
            shell: "powershell".to_owned(),
            command: "Write-Output provider-async-rewake-blocking-reason; exit 2".to_owned(),
        }
    } else {
        HookTestCommand {
            shell: "sh".to_owned(),
            command: "printf 'provider-async-rewake-blocking-reason\\n'; exit 2".to_owned(),
        }
    };
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("command_run".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Command {
                command: hook_command.command,
                if_condition: None,
                shell: Some(hook_command.shell),
                timeout: Some(5),
                status_message: None,
                once: false,
                run_async: false,
                async_rewake: true,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    let command_args = json!({
        "argv": platform_echo_args("provider-async-rewake-command-output")
    })
    .to_string();
    let write_args = json!({
        "path": "ASYNC-REWAKE.md",
        "content": "# Async Rewake\n\nThe provider received the rewake system reminder before writing.\n"
    })
    .to_string();
    let final_content = json!({
        "status": "completed",
        "summary": "Async rewake reminder was delivered into the provider loop.",
        "checks": ["async_rewake_hook: delivered_to_provider_loop"],
        "blockers": []
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-async-rewake-command",
                                "type": "function",
                                "function": {
                                    "name": "command_run",
                                    "arguments": command_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-async-rewake-write",
                                "type": "function",
                                "function": {
                                    "name": "write_text_file",
                                    "arguments": write_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                    }
                }
            ]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": serde_json::to_value(config).unwrap(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Run a command, then write ASYNC-REWAKE.md.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Run a command, then write ASYNC-REWAKE.md.",
                "plan_draft": {
                    "goal": "Run a command, then write ASYNC-REWAKE.md.",
                    "affected_paths": ["ASYNC-REWAKE.md"]
                },
                "acceptance_criteria": ["ASYNC-REWAKE.md exists", "async rewake reminder reached provider context"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert_eq!(
        fs::read_to_string(repo_root.join("ASYNC-REWAKE.md")).unwrap(),
        "# Async Rewake\n\nThe provider received the rewake system reminder before writing.\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = wait_for_events(&store, &run_id, |events| {
        events.iter().any(|event| {
            event.kind == "model_tool.async_rewake.delivered"
                && event.payload["tool_use_id"].as_str() == Some("call-async-rewake-command")
        })
    })
    .await;
    let phase_event = events
        .iter()
        .find(|event| {
            event.kind == "model_tool.phase"
                && event.payload["phase"].as_str() == Some("pre_tool_use_hooks")
                && event.payload["tool_use_id"].as_str() == Some("call-async-rewake-command")
        })
        .expect("async rewake pre hook phase should be recorded");
    assert_eq!(
        phase_event.payload["hook_results"][0]["rewake_supported"],
        true
    );
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.async_rewake.notification"
            && event.payload["tool_use_id"].as_str() == Some("call-async-rewake-command")
            && event.payload["message"]
                .as_str()
                .is_some_and(|message| message.contains("provider-async-rewake-blocking-reason"))
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.async_rewake.delivered"
            && event.payload["tool_use_id"].as_str() == Some("call-async-rewake-command")
            && event.payload["delivery_channel"].as_str() == Some("model_tool_turn_attachment")
            && event.payload["drain_later_notifications"].as_bool() == Some(true)
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_turn.attachments_delivered"
            && event.payload["attachment_types"]
                .as_array()
                .is_some_and(|types| {
                    types
                        .iter()
                        .any(|item| item.as_str() == Some("queued_command"))
                })
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 3);
    let second_messages = captured_requests[1]["messages"].as_array().unwrap();
    assert!(second_messages.iter().any(|message| {
        message["role"] == "tool"
            && message["tool_call_id"] == "call-async-rewake-command"
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains("provider-async-rewake-command-output"))
    }));
    assert!(second_messages.iter().any(|message| {
        message["role"] == "system"
            && message["content"].as_str().is_some_and(|content| {
                content.contains("<system-reminder>")
                    && content.contains("provider-async-rewake-blocking-reason")
            })
    }));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_provider_native_shared_tool_respects_webhook_pre_tool_use_blocking_hook() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.permissions.run_commands = ConfigPermissionDecision::Allow;
    harness.permissions.network = ConfigPermissionDecision::Allow;
    let hook_response = json!({
        "decision": "block",
        "reason": "native-provider-webhook-pre-hook-blocked"
    });
    let (hook_url, hook_capture) = spawn_webhook_test_server(hook_response).await;
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("command_run".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: hook_url,
                if_condition: None,
                timeout: Some(5),
                headers: BTreeMap::new(),
                allowed_env_vars: Vec::new(),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    let command_args = json!({
        "argv": platform_write_file_args("webhook-command-ran.txt", "ran")
    })
    .to_string();
    let final_content = json!({
        "status": "blocked",
        "summary": "Webhook PreToolUse hook blocked the command.",
        "checks": ["webhook_pre_tool_use_hook: blocked"],
        "blockers": ["native-provider-webhook-pre-hook-blocked"]
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-webhook-pre-hook-command",
                                "type": "function",
                                "function": {
                                    "name": "command_run",
                                    "arguments": command_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                    }
                }
            ]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": serde_json::to_value(config).unwrap(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Run a command, but a webhook pre hook should block it.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Run a command, but a webhook pre hook should block it.",
                "plan_draft": {
                    "goal": "Run a command, but a webhook pre hook should block it.",
                    "affected_paths": ["webhook-command-ran.txt"]
                },
                "acceptance_criteria": ["webhook-command-ran.txt must not be created"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "blocked");
    assert!(!repo_root.join("webhook-command-ran.txt").exists());

    let hook_capture = hook_capture.lock().unwrap();
    let hook_input = hook_capture.body.as_ref().unwrap();
    assert_eq!(hook_input["hook_event_name"], "PreToolUse");
    assert_eq!(hook_input["tool_name"], "command_run");
    assert_eq!(hook_input["tool_use_id"], "call-webhook-pre-hook-command");
    drop(hook_capture);

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model.tool_call.completed"
            && event.payload["tool_name"] == "command_run"
            && event.payload["status"] == "blocked"
            && event.payload["is_error"] == true
    }));
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.phase"
            && event.payload["phase"].as_str() == Some("pre_tool_use_hooks")
            && event.payload["status"].as_str() == Some("blocked")
            && event.payload["webhook_hook_count"] == 1
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 2);
    assert!(captured_requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "call-webhook-pre-hook-command"
                && message["content"].as_str().is_some_and(|content| {
                    content.contains("native-provider-webhook-pre-hook-blocked")
                })
        }));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_provider_native_shared_tool_returns_webhook_post_hook_updated_output() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.permissions.run_commands = ConfigPermissionDecision::Allow;
    harness.permissions.network = ConfigPermissionDecision::Allow;
    config
        .workflows
        .get_mut("planner-led")
        .unwrap()
        .edges
        .retain(|edge| edge.from != "executor");
    let hook_response = json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedMCPToolOutput": {
                "replacement": "native provider webhook post hook output"
            },
            "additionalContext": "native provider webhook post context"
        }
    });
    let (hook_url, hook_capture) = spawn_webhook_test_server(hook_response).await;
    config.hooks = coder_config::HookSettings {
        post_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("command_run".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: hook_url,
                if_condition: None,
                timeout: Some(5),
                headers: BTreeMap::new(),
                allowed_env_vars: Vec::new(),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    let command_args = json!({
        "argv": platform_echo_args("original webhook command output")
    })
    .to_string();
    let write_args = json!({
        "path": "WEBHOOK-POST-HOOK.md",
        "content": "# Webhook Post Hook\n\nThe model observed the webhook post hook replacement output.\n"
    })
    .to_string();
    let final_content = json!({
        "status": "completed",
        "summary": "Webhook PostToolUse hook output was returned to the provider loop.",
        "checks": ["webhook_post_tool_use_hook: updated_output_observed"],
        "blockers": []
    })
    .to_string();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-webhook-post-hook-command",
                                "type": "function",
                                "function": {
                                    "name": "command_run",
                                    "arguments": command_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-webhook-post-hook-write",
                                "type": "function",
                                "function": {
                                    "name": "write_text_file",
                                    "arguments": write_args
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                    }
                }
            ]
        }),
    ])
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": serde_json::to_value(config).unwrap(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Run a command, observe webhook post hook output, then write WEBHOOK-POST-HOOK.md.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Run a command, observe webhook post hook output, then write WEBHOOK-POST-HOOK.md.",
                "plan_draft": {
                    "goal": "Run a command, observe webhook post hook output, then write WEBHOOK-POST-HOOK.md.",
                    "affected_paths": ["WEBHOOK-POST-HOOK.md"]
                },
                "acceptance_criteria": ["WEBHOOK-POST-HOOK.md exists", "webhook post hook output was observed"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert!(body["report"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some("WEBHOOK-POST-HOOK.md")));
    assert_eq!(
        fs::read_to_string(repo_root.join("WEBHOOK-POST-HOOK.md")).unwrap(),
        "# Webhook Post Hook\n\nThe model observed the webhook post hook replacement output.\n"
    );

    let hook_capture = hook_capture.lock().unwrap();
    let hook_input = hook_capture.body.as_ref().unwrap();
    assert_eq!(hook_input["hook_event_name"], "PostToolUse");
    assert_eq!(hook_input["tool_name"], "command_run");
    assert_eq!(hook_input["tool_use_id"], "call-webhook-post-hook-command");
    drop(hook_capture);

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.phase"
            && event.payload["phase"].as_str() == Some("post_tool_use_hooks")
            && event.payload["updated_tool_output_applied"] == true
            && event.payload["webhook_hook_count"] == 1
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 3);
    assert!(captured_requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == "call-webhook-post-hook-command"
                && message["content"].as_str().is_some_and(|content| {
                    content.contains("native provider webhook post hook output")
                        && !content.contains("original webhook command output")
                })
        }));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_completes_provider_native_tool_loop_after_file_write_turn_limit() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let write_args = json!({
        "path": "README.md",
        "content": "# Turn Limit\n\nThe model wrote this file before continuing to inspect the repo.\n"
    })
    .to_string();
    let mut payloads = vec![json!({
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [
                        {
                            "id": "call-limit-write",
                            "type": "function",
                            "function": {
                                "name": "write_text_file",
                                "arguments": write_args
                            }
                        }
                    ]
                }
            }
        ]
    })];
    for index in 2..=8 {
        payloads.push(json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": format!("call-limit-status-{index}"),
                                "type": "function",
                                "function": {
                                    "name": "git_status",
                                    "arguments": "{}"
                                }
                            }
                        ]
                    }
                }
            ]
        }));
    }
    let (provider_base_url, captured) =
        spawn_openai_compatible_sequence_capture_test_server(payloads).await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);
    let mut config = example_config();
    config["agents"]["executor"]["runtime"]["max_turns"] = json!(8);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": config,
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Create README.md and stop.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Create README.md.",
                "plan_draft": {
                    "goal": "Create README.md.",
                    "affected_paths": ["README.md"]
                },
                "acceptance_criteria": ["README.md exists"]
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["status"], "completed");
    assert!(body["report"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some("README.md")));
    assert!(body["report"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check.as_str()
            == Some("native_model_tool_loop: stopped_after_turn_limit_with_file_writes")));
    assert_eq!(
        fs::read_to_string(repo_root.join("README.md")).unwrap(),
        "# Turn Limit\n\nThe model wrote this file before continuing to inspect the repo.\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "backend.native_rust.completed"
            && event.payload["execution_mode"] == "tool_loop"
            && event.payload["tool_call_count"] == 8
    }));
    assert!(!events.iter().any(|event| {
        event.kind == "node.blocked"
            && event.payload["reason"] == "native model tool loop reached its turn limit"
    }));
    assert_eq!(captured.lock().unwrap().len(), 8);

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_stops_immediately_when_model_calls_finish() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let payloads = vec![
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [{
                        "id": "call-finish-write",
                        "type": "function",
                        "function": {
                            "name": "write_text_file",
                            "arguments": json!({"path": "README.md", "content": "# Done\n"}).to_string()
                        }
                    }]
                }
            }]
        }),
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [{
                        "id": "call-finish-terminal",
                        "type": "function",
                        "function": {
                            "name": "finish",
                            "arguments": json!({
                                "status": "completed",
                                "summary": "README.md was created.",
                                "checks": ["README.md exists"]
                            }).to_string()
                        }
                    }]
                }
            }]
        }),
    ];
    let (provider_base_url, captured) =
        spawn_openai_compatible_sequence_capture_test_server(payloads).await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store);
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "Start Work has been clicked. Create README.md.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "start_work_authorized": true,
                "original_user_request": "Create README.md.",
                "plan_draft": {"goal": "Create README.md.", "affected_paths": ["README.md"]},
                "acceptance_criteria": ["README.md exists"]
            }
        }),
    )
    .await;

    let body = response_json(response).await;
    assert_eq!(body["report"]["status"], "completed", "{body}");
    assert_eq!(captured.lock().unwrap().len(), 2);
    assert!(!body["report"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check
            .as_str()
            .is_some_and(|check| check.contains("stopped_after_turn_limit"))));
    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_without_start_work_authorization_does_not_call_native_file_writer() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let (provider_base_url, captured) = spawn_openai_compatible_capture_test_server(json!({
        "choices": [
            {
                "message": {
                    "content": "{\"status\":\"completed\",\"files\":[{\"path\":\"README.md\",\"content\":\"should not be written\"}]}"
                }
            }
        ]
    }))
    .await;
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "test-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/workflows/run",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "Create README.md for this repo.",
            "repo_root": repo_root.display().to_string(),
            "plan_context": {
                "original_user_request": "Create README.md for this repo.",
                "plan_draft": {
                    "goal": "Create README.md for this repo.",
                    "affected_paths": ["README.md"]
                }
            }
        }),
    )
    .await;

    let status = response.status();
    let body = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!repo_root.join("README.md").exists());
    assert!(captured.lock().unwrap().is_none());

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "backend.native_rust.blocked"
            && event.payload["implementation"] == "native-model-tool-loop"
            && event.payload["reason"] == "missing_start_work_approval"
    }));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}
