use super::*;

#[tokio::test]
async fn subagent_run_endpoint_spawns_child_and_records_sidechain() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    state.provider_settings.lock().unwrap().mock_mode = true;
    let app = router(state.clone());
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.backend = "mock".to_owned();
    harness.tools = vec!["read_file".to_owned()];
    let run_id = "run-subagent-api";

    let response = post_json(
        app,
        "/api/v3/tools/subagent/run",
        json!({
            "config": config,
            "run_id": run_id,
            "workflow_id": "planner-led",
            "node_id": "executor",
            "parent_agent_id": "executor",
            "parent_harness_id": "native-code-edit",
            "repo_root": ".",
            "task": "Inspect the current plan and report.",
            "agent_id": "agent-api-1",
            "subagent_name": "reviewer",
            "invoking_request_id": "request-1",
            "invocation_kind": "spawn",
            "parent_query_depth": 1,
            "backend_context": {"source": "test"}
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["run_id"], run_id);
    assert_eq!(body["agent_id"], "agent-api-1");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["event_count"], 1);
    assert_eq!(
        body["event_preview_limit"],
        subagent_tools::SUBAGENT_RESPONSE_EVENT_PREVIEW_LIMIT
    );
    assert_eq!(body["events_truncated"], false);
    assert_eq!(body["events"].as_array().unwrap().len(), 1);
    assert!(body["transcript_ref"]
        .as_str()
        .unwrap()
        .ends_with("subagents/agent-agent-api-1.jsonl"));
    let records = store
        .read_subagent_transcript_records(&RunId::from_string(run_id), "agent-api-1")
        .unwrap();
    let kinds = records
        .iter()
        .map(|record| record.kind.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            "subagent.started",
            "subagent.user",
            "subagent.event",
            "subagent.report",
            "subagent.completed"
        ]
    );
    assert_eq!(
        records[0].payload["context"]["contract"],
        "coder.subagent_context.v1"
    );
    assert!(records[0].payload["context"]
        .get("permission_recheck")
        .is_none());
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn subagent_run_endpoint_can_launch_background_task_and_report_status() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    state.provider_settings.lock().unwrap().mock_mode = true;
    let app = router(state.clone());
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.backend = "mock".to_owned();
    harness.tools = vec!["read_file".to_owned()];
    let run_id = "run-subagent-background";

    let response = post_json(
        app.clone(),
        "/api/v3/tools/subagent/run",
        json!({
            "config": config,
            "run_id": run_id,
            "workflow_id": "planner-led",
            "node_id": "executor",
            "parent_agent_id": "executor",
            "parent_harness_id": "native-code-edit",
            "repo_root": ".",
            "task": "Inspect the current plan in the background.",
            "agent_id": "agent-bg-api-1",
            "subagent_name": "background-reviewer",
            "invocation_kind": "spawn",
            "run_in_background": true,
            "backend_context": {"source": "test"}
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["run_id"], run_id);
    assert_eq!(body["agent_id"], "agent-bg-api-1");
    assert_eq!(body["status"], "backgrounded");
    let task_id = body["background_task"]["task_id"].as_str().unwrap();
    assert_eq!(body["background_task"]["run_id"], run_id);
    assert_eq!(body["background_task"]["agent_id"], "agent-bg-api-1");
    assert_eq!(
        body["background_task"]["status_url"],
        format!("/api/v3/tools/subagent/background/{task_id}")
    );

    let mut status_body = Value::Null;
    for _ in 0..20 {
        let response = get_json(
            app.clone(),
            &format!("/api/v3/tools/subagent/background/{task_id}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        status_body = response_json(response).await;
        if status_body["status"] == "completed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(status_body["status"], "completed");
    assert_eq!(status_body["run_id"], run_id);
    assert_eq!(status_body["agent_id"], "agent-bg-api-1");
    assert_eq!(status_body["event_count"], 1);
    assert_eq!(
        status_body["event_preview_limit"],
        subagent_tools::SUBAGENT_RESPONSE_EVENT_PREVIEW_LIMIT
    );
    assert!(status_body["metadata_ref"]
        .as_str()
        .unwrap()
        .ends_with("subagents/agent-agent-bg-api-1.meta.json"));
    let metadata = store
        .read_subagent_metadata(&RunId::from_string(run_id), "agent-bg-api-1")
        .unwrap()
        .unwrap();
    assert_eq!(metadata.status.as_deref(), Some("completed"));
    for _ in 0..20 {
        if state.background_subagents.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(state.background_subagents.lock().unwrap().is_empty());
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn subagent_background_status_recovers_from_durable_task_record() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    state.provider_settings.lock().unwrap().mock_mode = true;
    let app = router(state);
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.backend = "mock".to_owned();
    harness.tools = vec!["read_file".to_owned()];
    let run_id = "run-subagent-background-recover";

    let response = post_json(
        app.clone(),
        "/api/v3/tools/subagent/run",
        json!({
            "config": config,
            "run_id": run_id,
            "workflow_id": "planner-led",
            "node_id": "executor",
            "parent_agent_id": "executor",
            "parent_harness_id": "native-code-edit",
            "repo_root": ".",
            "task": "Inspect the current plan in the background.",
            "agent_id": "agent-bg-recover",
            "subagent_name": "background-reviewer",
            "invocation_kind": "spawn",
            "run_in_background": true,
            "backend_context": {"source": "test"}
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let task_id = body["background_task"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let mut status_body = Value::Null;
    for _ in 0..20 {
        let response = get_json(
            app.clone(),
            &format!("/api/v3/tools/subagent/background/{task_id}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        status_body = response_json(response).await;
        if status_body["status"] == "completed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(status_body["status"], "completed");

    let mut stale_record = store
        .read_subagent_background_task_record(&task_id)
        .unwrap()
        .unwrap();
    stale_record.status = "running".to_owned();
    stale_record.report = None;
    stale_record.event_count = 0;
    stale_record.events_truncated = false;
    store
        .write_subagent_background_task_record(&stale_record)
        .unwrap();

    let recovered_app = router(ApiState::new(store.clone()));
    let recovered_response = get_json(
        recovered_app,
        &format!("/api/v3/tools/subagent/background/{task_id}"),
    )
    .await;
    assert_eq!(recovered_response.status(), StatusCode::OK);
    let recovered_body = response_json(recovered_response).await;
    assert_eq!(recovered_body["status"], "completed");
    assert_eq!(recovered_body["run_id"], run_id);
    assert_eq!(recovered_body["agent_id"], "agent-bg-recover");
    assert_eq!(recovered_body["event_count"], 1);
    assert_eq!(
        recovered_body["events"][0]["kind"],
        "backend.native_mock.completed"
    );
    assert!(store
        .read_subagent_background_task_record(&task_id)
        .unwrap()
        .is_some());
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn subagent_background_running_task_without_live_registry_recovers_as_lost() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-subagent-background-lost");
    let transcript_ref =
        "subagent://runs/run-subagent-background-lost/subagents/agent-agent-bg-lost.jsonl";
    let metadata = coder_store::SubagentMetadata {
        agent_type: "subagent".to_owned(),
        parent_agent_id: "executor".to_owned(),
        parent_harness_id: "native-code-edit".to_owned(),
        invocation_kind: "spawn".to_owned(),
        status: Some("running".to_owned()),
        terminal_record_kind: None,
        last_sequence: None,
        error: None,
        description: Some("background-reviewer".to_owned()),
        worktree_path: Some(".".to_owned()),
        transcript_ref: Some(transcript_ref.to_owned()),
    };
    let metadata_ref = store
        .write_subagent_metadata(&run_id, "agent-bg-lost", &metadata)
        .unwrap();
    let record = coder_store::SubagentBackgroundTaskRecord {
        task_id: "task-bg-lost".to_owned(),
        run_id: run_id.as_str().to_owned(),
        agent_id: "agent-bg-lost".to_owned(),
        status: "running".to_owned(),
        created_at_ms: 1000,
        updated_at_ms: 1000,
        metadata_ref,
        transcript_ref: transcript_ref.to_owned(),
        report: None,
        event_count: 0,
        events_truncated: false,
        error: None,
    };
    store
        .write_subagent_background_task_record(&record)
        .unwrap();

    let app = router(ApiState::new(store.clone()));
    let status_response = get_json(
        app.clone(),
        "/api/v3/tools/subagent/background/task-bg-lost",
    )
    .await;
    assert_eq!(status_response.status(), StatusCode::OK);
    let status_body = response_json(status_response).await;
    assert_eq!(status_body["status"], "lost");
    assert!(status_body["error"]
        .as_str()
        .unwrap()
        .contains("no live task registry"));

    let persisted = store
        .read_subagent_background_task_record("task-bg-lost")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.status, "lost");
    assert!(persisted.error.unwrap().contains("no live task registry"));
    let metadata = store
        .read_subagent_metadata(&run_id, "agent-bg-lost")
        .unwrap()
        .unwrap();
    assert_eq!(metadata.status.as_deref(), Some("lost"));
    assert_eq!(
        metadata.terminal_record_kind.as_deref(),
        Some("subagent.lost")
    );
    let transcript = store
        .read_subagent_transcript_records(&run_id, "agent-bg-lost")
        .unwrap();
    assert!(transcript
        .iter()
        .any(|record| record.kind == "subagent.lost"));

    let cancel_response = delete_json(app, "/api/v3/tools/subagent/background/task-bg-lost").await;
    assert_eq!(cancel_response.status(), StatusCode::OK);
    let cancel_body = response_json(cancel_response).await;
    assert_eq!(cancel_body["cancelled"], false);
    assert_eq!(cancel_body["status"], "lost");
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn subagent_background_cancel_recovers_durable_running_task() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-subagent-background-cancel-recover");
    let metadata = coder_store::SubagentMetadata {
        agent_type: "subagent".to_owned(),
        parent_agent_id: "executor".to_owned(),
        parent_harness_id: "native-code-edit".to_owned(),
        invocation_kind: "spawn".to_owned(),
        status: Some("running".to_owned()),
        terminal_record_kind: None,
        last_sequence: None,
        error: None,
        description: Some("background-reviewer".to_owned()),
        worktree_path: Some(".".to_owned()),
        transcript_ref: Some(
            "subagent://runs/run-subagent-background-cancel-recover/subagents/agent-agent-bg-cancel.jsonl"
                .to_owned(),
        ),
    };
    let metadata_ref = store
        .write_subagent_metadata(&run_id, "agent-bg-cancel", &metadata)
        .unwrap();
    let record = coder_store::SubagentBackgroundTaskRecord {
        task_id: "task-bg-cancel".to_owned(),
        run_id: run_id.as_str().to_owned(),
        agent_id: "agent-bg-cancel".to_owned(),
        status: "running".to_owned(),
        created_at_ms: 1000,
        updated_at_ms: 1000,
        metadata_ref,
        transcript_ref:
            "subagent://runs/run-subagent-background-cancel-recover/subagents/agent-agent-bg-cancel.jsonl"
                .to_owned(),
        report: None,
        event_count: 0,
        events_truncated: false,
        error: None,
    };
    store
        .write_subagent_background_task_record(&record)
        .unwrap();

    let app = router(ApiState::new(store.clone()));
    let cancel_response = delete_json(
        app.clone(),
        "/api/v3/tools/subagent/background/task-bg-cancel",
    )
    .await;
    assert_eq!(cancel_response.status(), StatusCode::OK);
    let cancel_body = response_json(cancel_response).await;
    assert_eq!(cancel_body["cancelled"], true);
    assert_eq!(cancel_body["status"], "cancelled");
    let metadata = store
        .read_subagent_metadata(&run_id, "agent-bg-cancel")
        .unwrap()
        .unwrap();
    assert_eq!(metadata.status.as_deref(), Some("cancelled"));
    assert_eq!(
        metadata.terminal_record_kind.as_deref(),
        Some("subagent.cancelled")
    );
    let status_response = get_json(app, "/api/v3/tools/subagent/background/task-bg-cancel").await;
    assert_eq!(status_response.status(), StatusCode::OK);
    let status_body = response_json(status_response).await;
    assert_eq!(status_body["status"], "cancelled");
    let _ = fs::remove_dir_all(store_root);
}

#[test]
fn subagent_run_response_events_are_bounded_preview() {
    let events = (0..subagent_tools::SUBAGENT_RESPONSE_EVENT_PREVIEW_LIMIT + 2)
        .map(|index| HarnessRunEvent::new("child.event", json!({ "index": index })))
        .collect::<Vec<_>>();

    let (preview, event_count, events_truncated) =
        subagent_tools::subagent_response_event_preview(events);

    assert_eq!(
        preview.len(),
        subagent_tools::SUBAGENT_RESPONSE_EVENT_PREVIEW_LIMIT
    );
    assert_eq!(
        event_count,
        subagent_tools::SUBAGENT_RESPONSE_EVENT_PREVIEW_LIMIT + 2
    );
    assert!(events_truncated);
    assert_eq!(preview.last().unwrap().payload["index"], 999);
}
