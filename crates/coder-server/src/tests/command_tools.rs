use super::*;

#[tokio::test]
async fn command_preview_endpoint_returns_policy_without_running() {
    let root = temp_root();
    fs::create_dir_all(&root).unwrap();
    let app = test_router();
    let response = post_json(
        app,
        "/api/v3/tools/command/preview",
        json!({
            "repo_root": root.display().to_string(),
            "cwd": ".",
            "argv": ["cmd.exe", "/C", "echo", "preview"],
            "source": "model",
            "sandbox": false
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["cwd"], ".");
    assert_eq!(body["requires_approval"], true);
    assert_eq!(body["policy"]["risk"], "medium");
    assert!(body["approval_key"].as_str().unwrap().starts_with("cmd:"));
    assert_eq!(body["evidence_kind"], "command_preview");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn command_preview_endpoint_rejects_cwd_escape() {
    let root = temp_root();
    fs::create_dir_all(&root).unwrap();
    let app = test_router();
    let response = post_json(
        app,
        "/api/v3/tools/command/preview",
        json!({
            "repo_root": root.display().to_string(),
            "cwd": "..",
            "argv": ["cmd.exe", "/C", "echo", "preview"],
            "source": "discovered"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn repo_read_file_range_endpoint_writes_evidence_when_run_id_is_present() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src").join("app.rs"), "one\ntwo\nthree\n").unwrap();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/repo/read-file-range",
        json!({
            "repo_root": repo.display().to_string(),
            "path": "src/app.rs",
            "start_line": 2,
            "max_lines": 1,
            "run_id": "run-1"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["snippet"]["text"], "two\n");
    assert!(body["evidence_ref"]["ref_id"]
        .as_str()
        .unwrap()
        .starts_with("repo-read:"));
    let evidence = store
        .list_repo_evidence(&RunId::from_string("run-1"))
        .unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].kind, RepoEvidenceKind::RepoRead);
    assert!(evidence[0].summary.contains("Read file range"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn repo_read_evidence_redacts_secret_markers_without_failing_the_read() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(
        repo.join("README.md"),
        "Document the LLM_API_KEY variable without storing its value.\n",
    )
    .unwrap();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/repo/read-file",
        json!({
            "repo_root": repo.display().to_string(),
            "path": "README.md",
            "run_id": "run-redacted-read"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert!(body["file"]["content"]
        .as_str()
        .unwrap()
        .contains("LLM_API_KEY"));
    let evidence = store
        .list_repo_evidence(&RunId::from_string("run-redacted-read"))
        .unwrap();
    let payload = store.read_repo_evidence(&evidence[0].ref_id).unwrap();
    assert_eq!(payload["file"]["content"], "[REDACTED]");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn command_run_endpoint_blocks_model_command_without_approval() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    let app = router(state.clone());

    let response = post_json(
        app,
        "/api/v3/tools/command/run",
        json!({
            "repo_root": repo.display().to_string(),
            "cwd": ".",
            "argv": platform_echo_args("blocked"),
            "source": "model",
            "sandbox": false,
            "approved": false,
            "run_id": "run-1"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["result"]["status"], "blocked");
    assert_eq!(body["result"]["blocked"], true);
    assert!(body["result"]["requires_approval"].as_bool().unwrap());
    assert!(body["evidence_ref"]["ref_id"]
        .as_str()
        .unwrap()
        .starts_with("repo-test:"));
    let events = store.read_events(&RunId::from_string("run-1")).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "approval.requested");
    assert_eq!(events[0].payload["approval_type"], "command");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn command_run_endpoint_uses_the_shared_process_registry_for_foreground_commands() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let state = ApiState::new(RunStore::new(&store_root));
    let app = router(state.clone());

    let response = post_json(
        app,
        "/api/v3/tools/command/run",
        json!({
            "repo_root": repo.display().to_string(),
            "cwd": ".",
            "argv": platform_echo_args("foreground"),
            "source": "model",
            "sandbox": true,
            "approved": true
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["result"]["status"], "completed");
    assert!(body["result"]["output"]
        .as_str()
        .unwrap()
        .contains("foreground"));
    for _ in 0..20 {
        if state.background_commands.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(state.background_commands.lock().unwrap().is_empty());

    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn command_run_endpoint_auto_background_returns_completed_result_when_fast() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/command/run",
        json!({
            "repo_root": repo.display().to_string(),
            "cwd": ".",
            "argv": platform_echo_args("fast"),
            "source": "model",
            "sandbox": true,
            "approved": true,
            "foreground_timeout_seconds": 5,
            "background_on_timeout": true,
            "run_id": "run-fast-bg"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["result"]["status"], "completed");
    assert_eq!(body["result"]["passed"], true);
    assert!(body["result"]["output"].as_str().unwrap().contains("fast"));
    assert!(body.get("background_task").is_none());
    assert!(body["evidence_ref"]["ref_id"]
        .as_str()
        .unwrap()
        .starts_with("repo-test:"));
    let events = store
        .read_events(&RunId::from_string("run-fast-bg"))
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].kind, "command.completed");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn command_run_endpoint_auto_background_returns_task_when_still_running() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store));

    let response = post_json(
        app.clone(),
        "/api/v3/tools/command/run",
        json!({
            "repo_root": repo.display().to_string(),
            "cwd": ".",
            "argv": platform_sleep_args(),
            "source": "model",
            "sandbox": true,
            "approved": true,
            "foreground_timeout_seconds": 1,
            "background_on_timeout": true,
            "run_id": "run-auto-bg"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["result"]["status"], "backgrounded");
    assert_eq!(body["result"]["timed_out"], false);
    assert!(body["result"]["output"]
        .as_str()
        .unwrap()
        .contains("still running in background task"));
    let task_id = body["background_task"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(body["background_task"]["status"], "running");
    assert!(body["evidence_ref"].is_null());

    let response = delete_json(
        app.clone(),
        &format!("/api/v3/tools/command/background/{task_id}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let status_body = wait_background_status(app, &task_id, &["cancelled"]).await;
    assert_eq!(status_body["status"], "cancelled");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn command_background_endpoint_completes_and_exposes_output() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    let app = router(state.clone());

    let response = post_json(
        app.clone(),
        "/api/v3/tools/command/background",
        json!({
            "repo_root": repo.display().to_string(),
            "cwd": ".",
            "argv": platform_delayed_echo_args("done"),
            "source": "model",
            "sandbox": true,
            "approved": true,
            "timeout_seconds": 5,
            "max_output_bytes": 1024,
            "run_id": "run-bg"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "running");
    let task_id = body["task_id"].as_str().unwrap().to_owned();

    let status_body = wait_background_status(app.clone(), &task_id, &["completed"]).await;
    assert_eq!(status_body["status"], "completed");
    assert_eq!(status_body["result"]["passed"], true);
    assert!(status_body["result"]["output"]
        .as_str()
        .unwrap()
        .contains("done"));
    assert!(status_body["evidence_ref"]["ref_id"]
        .as_str()
        .unwrap()
        .starts_with("repo-test:"));
    for _ in 0..20 {
        if state.background_commands.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(state.background_commands.lock().unwrap().is_empty());

    let response = get_json(
        app,
        &format!("/api/v3/tools/command/background/{task_id}/output"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let output_body = response_json(response).await;
    assert!(output_body["output"].as_str().unwrap().contains("done"));
    let events = store.read_events(&RunId::from_string("run-bg")).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, "command.started");
    assert_eq!(events[1].kind, "command.completed");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn command_background_output_cursor_does_not_repeat_observed_bytes() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let state = ApiState::new(RunStore::new(&store_root));
    let app = router(state.clone());

    let response = post_json(
        app.clone(),
        "/api/v3/tools/command/background",
        json!({
            "repo_root": repo.display().to_string(),
            "cwd": ".",
            "argv": platform_delayed_echo_args("cursor-output"),
            "source": "model",
            "sandbox": true,
            "approved": true,
            "timeout_seconds": 5
        }),
    )
    .await;
    let body = response_json(response).await;
    let task_id = body["task_id"].as_str().unwrap();
    let _ = wait_background_status(app, task_id, &["completed"]).await;

    let first =
        background_commands::background_command_status_since(&state, task_id, Some(0)).unwrap();
    assert!(first.output_preview.contains("cursor-output"));
    assert!(first.next_output_cursor > 0);
    let next = background_commands::background_command_status_since(
        &state,
        task_id,
        Some(first.next_output_cursor),
    )
    .unwrap();
    assert!(next.output_preview.is_empty());
    assert_eq!(next.output_cursor, first.next_output_cursor);
    assert!(!next.output_gap);

    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn command_background_interactive_process_accepts_stdin() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let app = router(ApiState::new(RunStore::new(&store_root)));

    let response = post_json(
        app.clone(),
        "/api/v3/tools/command/background",
        json!({
            "repo_root": repo.display().to_string(),
            "cwd": ".",
            "argv": platform_stdin_echo_args(),
            "source": "model",
            "sandbox": true,
            "approved": true,
            "interactive": true,
            "timeout_seconds": 5
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let task_id = body["task_id"].as_str().unwrap().to_owned();

    let response = post_json(
        app.clone(),
        &format!("/api/v3/tools/command/background/{task_id}/stdin"),
        json!({"input": "hello\n", "close_stdin": true}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let write = response_json(response).await;
    assert_eq!(write["bytes_written"], 6);
    assert_eq!(write["stdin_closed"], true);

    let status = wait_background_status(app, &task_id, &["completed"]).await;
    assert!(status["output_preview"]
        .as_str()
        .unwrap()
        .contains("got:hello"));

    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn command_background_status_and_output_recover_from_durable_task_record() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app.clone(),
        "/api/v3/tools/command/background",
        json!({
            "repo_root": repo.display().to_string(),
            "cwd": ".",
            "argv": platform_delayed_echo_args("durable"),
            "source": "model",
            "sandbox": true,
            "approved": true,
            "timeout_seconds": 5,
            "max_output_bytes": 1024,
            "run_id": "run-bg-durable"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let task_id = body["task_id"].as_str().unwrap().to_owned();

    let status_body = wait_background_status(app.clone(), &task_id, &["completed"]).await;
    assert_eq!(status_body["status"], "completed");
    assert!(status_body["output_preview"]
        .as_str()
        .unwrap()
        .contains("durable"));
    assert!(store
        .read_command_background_task_record(&task_id)
        .unwrap()
        .is_some());

    let recovered_app = router(ApiState::new(store.clone()));
    let recovered_response = get_json(
        recovered_app.clone(),
        &format!("/api/v3/tools/command/background/{task_id}"),
    )
    .await;
    assert_eq!(recovered_response.status(), StatusCode::OK);
    let recovered_body = response_json(recovered_response).await;
    assert_eq!(recovered_body["status"], "completed");
    assert_eq!(recovered_body["result"]["passed"], true);
    assert!(recovered_body["output_preview"]
        .as_str()
        .unwrap()
        .contains("durable"));

    let recovered_output_response = get_json(
        recovered_app,
        &format!("/api/v3/tools/command/background/{task_id}/output"),
    )
    .await;
    assert_eq!(recovered_output_response.status(), StatusCode::OK);
    let recovered_output = response_json(recovered_output_response).await;
    assert_eq!(recovered_output["status"], "completed");
    assert!(recovered_output["output"]
        .as_str()
        .unwrap()
        .contains("durable"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn command_background_running_task_without_live_registry_recovers_as_lost() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app.clone(),
        "/api/v3/tools/command/background",
        json!({
            "repo_root": repo.display().to_string(),
            "cwd": ".",
            "argv": platform_sleep_args(),
            "source": "model",
            "sandbox": true,
            "approved": true,
            "timeout_seconds": 30,
            "max_output_bytes": 1024,
            "run_id": "run-bg-lost"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let task_id = body["task_id"].as_str().unwrap().to_owned();

    let recovered_app = router(ApiState::new(store.clone()));
    let recovered_response = get_json(
        recovered_app.clone(),
        &format!("/api/v3/tools/command/background/{task_id}"),
    )
    .await;
    assert_eq!(recovered_response.status(), StatusCode::OK);
    let recovered_body = response_json(recovered_response).await;
    assert_eq!(recovered_body["status"], "lost");
    assert!(recovered_body["error"]
        .as_str()
        .unwrap()
        .contains("no live process registry"));

    let recovered_cancel = delete_json(
        recovered_app,
        &format!("/api/v3/tools/command/background/{task_id}"),
    )
    .await;
    assert_eq!(recovered_cancel.status(), StatusCode::OK);
    let cancel_body = response_json(recovered_cancel).await;
    assert_eq!(cancel_body["cancelled"], false);
    assert_eq!(cancel_body["status"], "lost");

    let response = delete_json(
        app.clone(),
        &format!("/api/v3/tools/command/background/{task_id}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let _ = wait_background_status(app, &task_id, &["cancelled"]).await;
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn command_background_endpoint_cancels_running_task() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app.clone(),
        "/api/v3/tools/command/background",
        json!({
            "repo_root": repo.display().to_string(),
            "cwd": ".",
            "argv": platform_sleep_args(),
            "source": "model",
            "sandbox": true,
            "approved": true,
            "timeout_seconds": 30,
            "max_output_bytes": 1024,
            "run_id": "run-cancel"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let task_id = body["task_id"].as_str().unwrap().to_owned();

    let response = delete_json(
        app.clone(),
        &format!("/api/v3/tools/command/background/{task_id}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cancel_body = response_json(response).await;
    assert_eq!(cancel_body["cancelled"], true);

    let status_body = wait_background_status(app, &task_id, &["cancelled"]).await;
    assert_eq!(status_body["status"], "cancelled");
    assert_eq!(status_body["result"]["status"], "cancelled");
    assert_eq!(status_body["result"]["passed"], false);
    let events = store
        .read_events(&RunId::from_string("run-cancel"))
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].kind, "command.failed");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn command_background_cancel_terminates_the_process_tree() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let app = router(ApiState::new(RunStore::new(&store_root)));

    let response = post_json(
        app.clone(),
        "/api/v3/tools/command/background",
        json!({
            "repo_root": repo.display().to_string(),
            "cwd": ".",
            "argv": platform_spawn_child_args(),
            "source": "model",
            "sandbox": true,
            "approved": true,
            "timeout_seconds": 30
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let task_id = body["task_id"].as_str().unwrap().to_owned();

    let pid_file = repo.join("child.pid");
    for _ in 0..100 {
        if pid_file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let child_pid = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    assert!(process_is_running(child_pid));

    let response = delete_json(
        app.clone(),
        &format!("/api/v3/tools/command/background/{task_id}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let _ = wait_background_status(app, &task_id, &["cancelled"]).await;
    for _ in 0..100 {
        if !process_is_running(child_pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!process_is_running(child_pid));

    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn command_background_endpoint_honors_explicit_timeout() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app.clone(),
        "/api/v3/tools/command/background",
        json!({
            "repo_root": repo.display().to_string(),
            "cwd": ".",
            "argv": platform_sleep_args(),
            "source": "model",
            "sandbox": true,
            "approved": true,
            "timeout_seconds": 1,
            "max_output_bytes": 1024,
            "run_id": "run-timeout"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let task_id = body["task_id"].as_str().unwrap().to_owned();

    let status_body = wait_background_status(app, &task_id, &["timeout"]).await;
    assert_eq!(status_body["status"], "timeout");
    assert_eq!(status_body["result"]["status"], "timeout");
    assert_eq!(status_body["result"]["timed_out"], true);
    assert_eq!(status_body["result"]["passed"], false);
    let events = store
        .read_events(&RunId::from_string("run-timeout"))
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].kind, "command.failed");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn command_background_endpoint_blocks_model_command_without_approval() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app.clone(),
        "/api/v3/tools/command/background",
        json!({
            "repo_root": repo.display().to_string(),
            "cwd": ".",
            "argv": platform_echo_args("blocked"),
            "source": "model",
            "sandbox": false,
            "approved": false,
            "run_id": "run-blocked-bg"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    let task_id = body["task_id"].as_str().unwrap().to_owned();

    let response = get_json(app, &format!("/api/v3/tools/command/background/{task_id}")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let status_body = response_json(response).await;
    assert_eq!(status_body["status"], "blocked");
    assert_eq!(status_body["result"]["blocked"], true);
    assert!(status_body["output_preview"]
        .as_str()
        .unwrap()
        .contains("requires explicit approval"));
    let events = store
        .read_events(&RunId::from_string("run-blocked-bg"))
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "approval.requested");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}
