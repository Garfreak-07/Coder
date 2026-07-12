use super::*;

#[tokio::test]
async fn timeline_endpoint_returns_empty_items_for_empty_run() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-empty");
    let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    store.write_metadata(&state).unwrap();
    let app = router(ApiState::new(store));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-empty/timeline")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["run_id"], "run-empty");
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["event_count"], 0);
    assert_eq!(body["returned_count"], 0);
    assert_eq!(body["truncated"], false);
    assert_eq!(body["next_after_sequence"], Value::Null);
    assert!(body.get("events").is_none());
    assert!(body.get("timeline").is_none());
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn timeline_projects_public_items_without_raw_payloads() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"task": "Update README.md", "repo_root": "."}),
            ),
        )
        .unwrap();
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                2,
                "backend.selected",
                json!({"agent_id": "executor", "backend": "native-rust", "status": "selected", "summary": "Executor backend: native-rust"}),
            ),
        )
        .unwrap();
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                3,
                "command.completed",
                json!({"command": "cargo test", "returncode": 0, "output": "ok"}),
            ),
        )
        .unwrap();
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                4,
                "patch.applied",
                json!({"files": [{"new_path": "README.md", "status": "modified"}]}),
            ),
        )
        .unwrap();
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                5,
                "executor.reasoning_summary",
                json!({"agent_id": "executor", "summary": "Need inspect repo state."}),
            ),
        )
        .unwrap();
    store
            .append_event(
                &run_id,
                &coder_events::CoderEvent::new(
                    run_id.clone(),
                    6,
                    "executor.action_selected",
                    json!({"agent_id": "executor", "tool_name": "repo_find_files", "status": "selected"}),
                ),
            )
            .unwrap();
    store
            .append_event(
                &run_id,
                &coder_events::CoderEvent::new(
                    run_id.clone(),
                    7,
                    "tool.completed",
                    json!({"agent_id": "executor", "tool_name": "repo_find_files", "status": "completed", "summary": "Found README.md"}),
                ),
            )
            .unwrap();
    store
            .append_event(
                &run_id,
                &coder_events::CoderEvent::new(
                    run_id.clone(),
                    8,
                    "observation.recorded",
                    json!({"agent_id": "executor", "tool_name": "repo_find_files", "summary": "Found README.md"}),
                ),
            )
            .unwrap();
    let mut report = FinalReport::completed("Done").with_check("cargo test: completed exit 0");
    report.next_steps.push("No next step recorded.".to_owned());
    report
        .refresh_planner_style_summary(Some("Update README.md"), &["Updated README.md".to_owned()]);
    store.write_report(&run_id, &report).unwrap();
    let app = router(ApiState::new(store));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/timeline")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let items = body["items"].as_array().unwrap();
    assert!(items.iter().any(|item| item["type"] == "reasoning_summary"));
    assert!(items.iter().any(|item| {
        item["type"] == "executor_step" && item["title"] == "Executor backend: Native"
    }));
    assert!(items
        .iter()
        .any(|item| item["type"] == "executor_step" && item["title"] == "Action selected"));
    assert!(items
        .iter()
        .any(|item| item["type"] == "executor_step" && item["title"] == "Observation recorded"));
    assert!(items
        .iter()
        .any(|item| item["type"] == "tool_call" && item["tool_name"] == "repo_find_files"));
    assert!(items.iter().any(|item| item["type"] == "command_execution"));
    assert!(items.iter().any(|item| item["type"] == "file_change"));
    assert!(items.iter().any(|item| item["type"] == "final_summary"));
    assert!(items.iter().any(|item| {
        item["type"] == "final_summary"
            && item["status"] == "completed"
            && item["next_steps"][0] == "No next step recorded."
    }));
    assert!(items.iter().all(|item| item.get("payload").is_none()));
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn report_timeline_artifact_and_jsonl_redact_key_like_strings() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let secret = "sk-live-1234567890";
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"task": format!("Use provider key {secret}"), "repo_root": "."}),
            ),
        )
        .unwrap();
    store
            .append_event(
                &run_id,
                &coder_events::CoderEvent::new(
                    run_id.clone(),
                    2,
                    "command.completed",
                    json!({"command": format!("echo {secret}"), "returncode": 0, "status": "completed"}),
                ),
            )
            .unwrap();
    let app = router(ApiState::new(store));

    let report_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/runs/run-1/report")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(report_response.status(), StatusCode::OK);
    let report_body = response_json(report_response).await;
    assert!(!report_body.to_string().contains(secret));

    let artifact_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/artifacts/final-report.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(artifact_response.status(), StatusCode::OK);
    let artifact_body = response_json(artifact_response).await;
    assert!(!artifact_body.to_string().contains(secret));

    let timeline_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/timeline")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(timeline_response.status(), StatusCode::OK);
    let timeline_body = response_json(timeline_response).await;
    assert!(!timeline_body.to_string().contains(secret));

    let events_text =
        fs::read_to_string(root.join("runs").join("run-1").join("events.jsonl")).unwrap();
    assert!(!events_text.contains(secret));
    assert!(events_text.contains("[REDACTED]"));
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn changes_endpoint_returns_empty_changes_for_no_change_run() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    run_git(&repo, &["init"]);
    run_git(&repo, &["config", "user.email", "coder@example.test"]);
    run_git(&repo, &["config", "user.name", "Coder Test"]);
    run_git(&repo, &["add", "tracked.txt"]);
    run_git(&repo, &["commit", "-m", "base"]);

    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-clean");
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"repo_root": repo.display().to_string(), "task": "inspect only"}),
            ),
        )
        .unwrap();
    store
        .write_report(&run_id, &FinalReport::completed("No changes"))
        .unwrap();
    let app = router(ApiState::new(store));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-clean/changes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let object = body.as_object().unwrap();
    assert_eq!(object.len(), 2);
    assert_eq!(body["run_id"], "run-clean");
    assert_eq!(body["changes"].as_array().unwrap().len(), 0);
    assert!(body.get("change_sets").is_none());
    assert!(body.get("items").is_none());
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn changes_endpoint_includes_untracked_new_files() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "base\n").unwrap();
    run_git(&repo, &["init"]);
    run_git(&repo, &["config", "user.email", "coder@example.test"]);
    run_git(&repo, &["config", "user.name", "Coder Test"]);
    run_git(&repo, &["add", "README.md"]);
    run_git(&repo, &["commit", "-m", "base"]);
    fs::write(repo.join("main.js"), "const score = 0;\n").unwrap();

    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-untracked");
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"repo_root": repo.display().to_string(), "task": "create new file"}),
            ),
        )
        .unwrap();
    store
        .write_report(&run_id, &FinalReport::completed("Created main.js"))
        .unwrap();
    let app = router(ApiState::new(store));

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-untracked/changes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = response_json(list_response).await;
    assert_eq!(list_body["changes"].as_array().unwrap().len(), 1);
    let diff = list_body["changes"][0]["after_diff"].as_str().unwrap();
    assert!(diff.contains("diff --git a/main.js b/main.js"));
    assert!(diff.contains("new file mode"));
    assert!(diff.contains("+const score = 0;"));
    assert!(list_body["changes"][0]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"] == "main.js"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_apply_patch_changes_are_visible_in_review_changes() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("first.txt"), "before\n").unwrap();
    run_git(&repo, &["init"]);
    run_git(&repo, &["config", "user.email", "coder@example.test"]);
    run_git(&repo, &["config", "user.name", "Coder Test"]);
    run_git(&repo, &["add", "first.txt"]);
    run_git(&repo, &["commit", "-m", "base"]);

    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-apply-patch-changes");
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({
                    "repo_root": repo.display().to_string(),
                    "task": "update two files with apply_patch"
                }),
            ),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let patch_response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-review-apply-patch",
            "tool_name": "apply_patch",
            "run_id": run_id.as_str(),
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo.display().to_string(),
                "approved": true,
                "patch": "*** Begin Patch\n*** Update File: first.txt\n@@\n-before\n+after\n*** Add File: second.txt\n+created by patch\n*** End Patch"
            }
        }),
    )
    .await;
    assert_eq!(patch_response.status(), StatusCode::OK);
    let patch_body = response_json(patch_response).await;
    assert_eq!(patch_body["status"], "completed");

    let changes_response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v3/runs/{}/changes", run_id.as_str()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(changes_response.status(), StatusCode::OK);
    let changes_body = response_json(changes_response).await;
    let changes = changes_body["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1);
    let changed_files = changes[0]["changed_files"].as_array().unwrap();
    assert!(changed_files.iter().any(|file| file["path"] == "first.txt"));
    assert!(changed_files
        .iter()
        .any(|file| file["path"] == "second.txt"));
    let diff = changes[0]["after_diff"].as_str().unwrap();
    assert!(diff.contains("diff --git a/first.txt b/first.txt"));
    assert!(diff.contains("+after"));
    assert!(diff.contains("diff --git a/second.txt b/second.txt"));
    assert!(diff.contains("+created by patch"));

    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "patch.applied"
            && event.payload["files"]
                .as_array()
                .is_some_and(|files| files.len() == 2)
    }));

    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn changes_endpoint_includes_committed_run_changes_since_start_head() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "base\n").unwrap();
    run_git(&repo, &["init"]);
    run_git(&repo, &["config", "user.email", "coder@example.test"]);
    run_git(&repo, &["config", "user.name", "Coder Test"]);
    run_git(&repo, &["add", "README.md"]);
    run_git(&repo, &["commit", "-m", "base"]);
    let base_head = run_git_capture(&repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("README.md"), "base\ncommitted by executor\n").unwrap();
    run_git(&repo, &["add", "README.md"]);
    run_git(&repo, &["commit", "-m", "executor change"]);

    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-committed");
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({
                    "repo_root": repo.display().to_string(),
                    "git_head": base_head.trim(),
                    "task": "change README"
                }),
            ),
        )
        .unwrap();
    store
        .write_report(&run_id, &FinalReport::completed("Committed README change"))
        .unwrap();
    let app = router(ApiState::new(store));

    let list_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-committed/changes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = response_json(list_response).await;
    assert_eq!(list_body["changes"].as_array().unwrap().len(), 1);
    let diff = list_body["changes"][0]["after_diff"].as_str().unwrap();
    assert!(diff.contains("diff --git a/README.md b/README.md"));
    assert!(diff.contains("+committed by executor"));
    assert_eq!(
        list_body["changes"][0]["base_git_head"].as_str().unwrap(),
        base_head.trim()
    );
    assert!(list_body["changes"][0]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"] == "README.md"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn run_projection_endpoints_missing_runs_return_structured_errors() {
    let app = test_router();
    let missing_timeline = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/missing-run/timeline")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let missing_changes = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/missing-run/changes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let missing_async_notifications = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/missing-run/async-notifications")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let malformed_timeline = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/bad*run/timeline")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(missing_timeline.status(), StatusCode::NOT_FOUND);
    assert_eq!(missing_changes.status(), StatusCode::NOT_FOUND);
    assert_eq!(missing_async_notifications.status(), StatusCode::NOT_FOUND);
    assert_eq!(malformed_timeline.status(), StatusCode::BAD_REQUEST);

    let missing_timeline_body = response_json(missing_timeline).await;
    let missing_changes_body = response_json(missing_changes).await;
    let missing_async_notifications_body = response_json(missing_async_notifications).await;
    let malformed_timeline_body = response_json(malformed_timeline).await;
    assert!(missing_timeline_body["error"]
        .as_str()
        .unwrap()
        .contains("missing-run"));
    assert!(missing_changes_body["error"]
        .as_str()
        .unwrap()
        .contains("missing-run"));
    assert!(missing_async_notifications_body["error"]
        .as_str()
        .unwrap()
        .contains("missing-run"));
    assert!(malformed_timeline_body["error"].is_string());
}

#[tokio::test]
async fn changeset_review_diff_accept_and_undo_roundtrip() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    run_git(&repo, &["init"]);
    run_git(&repo, &["config", "user.email", "coder@example.test"]);
    run_git(&repo, &["config", "user.name", "Coder Test"]);
    run_git(&repo, &["add", "tracked.txt"]);
    run_git(&repo, &["commit", "-m", "base"]);
    fs::write(repo.join("tracked.txt"), "changed\n").unwrap();

    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"repo_root": repo.display().to_string(), "task": "change file"}),
            ),
        )
        .unwrap();
    store
        .write_report(
            &run_id,
            &FinalReport {
                changed_files: vec!["tracked.txt".to_owned()],
                ..FinalReport::completed("Changed tracked.txt")
            },
        )
        .unwrap();
    let app = router(ApiState::new(store));

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/changes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = response_json(list_response).await;
    let change_set_id = list_body["changes"][0]["change_set_id"].as_str().unwrap();

    let diff_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v3/runs/run-1/changes/{change_set_id}/diff"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(diff_response.status(), StatusCode::OK);
    let diff_body = response_json(diff_response).await;
    assert!(diff_body["diff"].as_str().unwrap().contains("-base"));

    let accept_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v3/runs/run-1/changes/{change_set_id}/accept"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accept_response.status(), StatusCode::OK);
    let accepted_list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/changes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted_list_response.status(), StatusCode::OK);
    let accepted_list = response_json(accepted_list_response).await;
    assert_eq!(accepted_list["changes"][0]["status"], "accepted");
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "changed\n"
    );

    let undo_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v3/runs/run-1/changes/{change_set_id}/undo"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(undo_response.status(), StatusCode::OK);
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "base\n"
    );
    let undone_list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/changes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(undone_list_response.status(), StatusCode::OK);
    let undone_list = response_json(undone_list_response).await;
    assert_eq!(undone_list["changes"].as_array().unwrap().len(), 0);
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn changeset_list_is_empty_without_working_tree_changes() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    run_git(&repo, &["init"]);
    run_git(&repo, &["config", "user.email", "coder@example.test"]);
    run_git(&repo, &["config", "user.name", "Coder Test"]);
    run_git(&repo, &["add", "tracked.txt"]);
    run_git(&repo, &["commit", "-m", "base"]);

    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"repo_root": repo.display().to_string(), "task": "inspect only"}),
            ),
        )
        .unwrap();
    store
        .write_report(&run_id, &FinalReport::completed("No changes"))
        .unwrap();
    let app = router(ApiState::new(store));

    let list_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/changes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = response_json(list_response).await;
    assert_eq!(list_body["changes"].as_array().unwrap().len(), 0);
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn changeset_undo_conflicts_when_working_tree_diff_changed() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    run_git(&repo, &["init"]);
    run_git(&repo, &["config", "user.email", "coder@example.test"]);
    run_git(&repo, &["config", "user.name", "Coder Test"]);
    run_git(&repo, &["add", "tracked.txt"]);
    run_git(&repo, &["commit", "-m", "base"]);
    fs::write(repo.join("tracked.txt"), "changed\n").unwrap();

    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"repo_root": repo.display().to_string(), "task": "change file"}),
            ),
        )
        .unwrap();
    store
        .write_report(
            &run_id,
            &FinalReport {
                changed_files: vec!["tracked.txt".to_owned()],
                ..FinalReport::completed("Changed tracked.txt")
            },
        )
        .unwrap();
    let app = router(ApiState::new(store));

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/changes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = response_json(list_response).await;
    let change_set_id = list_body["changes"][0]["change_set_id"].as_str().unwrap();
    fs::write(repo.join("tracked.txt"), "user changed\n").unwrap();

    let undo_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v3/runs/run-1/changes/{change_set_id}/undo"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(undo_response.status(), StatusCode::CONFLICT);
    let undo_body = response_json(undo_response).await;
    assert!(undo_body["error"].as_str().unwrap().contains("tracked.txt"));
    assert!(undo_body["error"]
        .as_str()
        .unwrap()
        .contains("diff content changed"));
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "user changed\n"
    );

    let conflict_list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/changes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(conflict_list_response.status(), StatusCode::OK);
    let conflict_list = response_json(conflict_list_response).await;
    assert_eq!(conflict_list["changes"][0]["status"], "failed_to_undo");
    assert!(conflict_list["changes"][0]["undo_conflict"]
        .as_str()
        .unwrap()
        .contains("tracked.txt"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn plugin_and_cache_codex_surfaces_are_available() {
    let app = test_router();
    for uri in [
        "/api/v3/plugins/marketplaces",
        "/api/v3/plugins",
        "/api/v3/plugins/installed",
        "/api/v3/skills/extra-roots",
        "/api/v3/hooks",
        "/api/v3/cache/status",
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
    }
}

#[tokio::test]
async fn cache_status_reports_real_store_disk_usage() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    store.ensure_local_layout().unwrap();
    store.write_blob(b"hello").unwrap();
    fs::write(store_root.join("repo-index").join("index.jsonl"), b"abc").unwrap();
    let state = ApiState::new(store);
    state.provider_settings.lock().unwrap().mock_mode = true;
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/cache/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["blob_store"]["entries"], 1);
    assert_eq!(body["blob_store"]["bytes"], 5);
    assert_eq!(
        body["blob_store"]["entry_scan_limit"],
        coder_store::MAX_CACHE_USAGE_SCAN_ENTRIES
    );
    assert!(
        body["blob_store"]["scanned_entries"].as_u64().unwrap()
            >= body["blob_store"]["entries"].as_u64().unwrap()
    );
    assert_eq!(body["blob_store"]["truncated"], false);
    assert_eq!(body["repo_index"]["entries"], 1);
    assert_eq!(body["repo_index"]["bytes"], 3);
    assert_eq!(body["repo_index"]["scanned_entries"], 1);
    assert_eq!(body["repo_index"]["truncated"], false);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn cache_clear_removes_disposable_store_entries_through_api() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    store.ensure_local_layout().unwrap();
    fs::write(store_root.join("repo-index").join("index.jsonl"), b"abc").unwrap();
    fs::write(store_root.join("plugin-cache").join("plugin.json"), b"{}").unwrap();
    store.write_blob(b"durable").unwrap();
    let state = ApiState::new(store.clone());
    state.provider_settings.lock().unwrap().mock_mode = true;
    let app = router(state);

    let response = post_json(app, "/api/v3/cache/clear", json!({})).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(body["store"]["entries"], 2);
    assert_eq!(body["store"]["bytes"], 5);
    assert!(!store_root.join("repo-index").join("index.jsonl").exists());
    assert!(!store_root.join("plugin-cache").join("plugin.json").exists());
    assert_eq!(store.cache_bucket_usage("blobs").unwrap().entries, 1);
    let _ = fs::remove_dir_all(store_root);
}
