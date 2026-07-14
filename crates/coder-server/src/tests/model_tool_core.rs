use super::*;

#[tokio::test]
async fn model_tool_file_and_finish_tools_share_permissions_evidence_and_events() {
    let store_root = temp_root();
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));
    let run_id = RunId::from_string("run-model-tool-write");

    let write = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-write",
            "tool_name": "write_text_file",
            "run_id": run_id.as_str(),
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "content": "alpha\n",
                "approved": true
            }
        }),
    )
    .await;
    assert_eq!(write.status(), StatusCode::OK);
    let write = response_json(write).await;
    assert_eq!(write["status"], "completed");
    assert_eq!(write["payload"]["changed_file"]["path"], "README.md");
    assert_eq!(write["refs"][0]["label"], "repo_evidence");
    assert!(write["payload"]["model_tool_phases"]
        .as_array()
        .is_some_and(|phases| phases
            .iter()
            .any(|phase| phase["phase"] == "permission_decision")));

    let edit = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-edit",
            "tool_name": "edit_text_file",
            "run_id": run_id.as_str(),
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "old_string": "alpha",
                "new_string": "beta",
                "approved": true
            }
        }),
    )
    .await;
    assert_eq!(edit.status(), StatusCode::OK);
    let edit = response_json(edit).await;
    assert_eq!(edit["status"], "completed");
    assert_eq!(edit["payload"]["operation"], "exact_string_edit");
    assert_eq!(
        fs::read_to_string(repo.join("README.md")).unwrap(),
        "beta\n"
    );

    let finish = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-finish",
            "tool_name": "finish",
            "input": {
                "status": "completed",
                "summary": "done",
                "checks": ["write observed"]
            }
        }),
    )
    .await;
    assert_eq!(finish.status(), StatusCode::OK);
    let finish = response_json(finish).await;
    assert_eq!(finish["status"], "completed");
    assert_eq!(finish["payload"]["summary"], "done");

    let events = store.read_events(&run_id).unwrap();
    let file_events = events
        .iter()
        .filter(|event| event.kind == "file.written")
        .collect::<Vec<_>>();
    assert_eq!(file_events.len(), 2);
    assert!(file_events.iter().all(|event| event
        .refs
        .iter()
        .any(|reference| reference.label == "repo_evidence")));
    assert_eq!(store.list_repo_evidence(&run_id).unwrap().len(), 2);
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_file_write_is_blocked_by_write_permission_before_mutation() {
    let store_root = temp_root();
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-write-denied");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .write_files = ConfigPermissionDecision::Deny;
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-write-denied",
            "tool_name": "write_text_file",
            "run_id": run_id.as_str(),
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "DENIED.md",
                "content": "must not exist",
                "approved": true
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert_eq!(body["payload"]["blocked_by"], "permission_decision");
    assert!(!repo.join("DENIED.md").exists());
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_apply_patch_is_atomic_and_records_multi_file_evidence() {
    let store_root = temp_root();
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("first.txt"), "before\n").unwrap();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));
    let run_id = RunId::from_string("run-model-tool-apply-patch");

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-apply-patch",
            "tool_name": "apply_patch",
            "run_id": run_id.as_str(),
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "approved": true,
                "patch": "*** Begin Patch\n*** Update File: first.txt\n@@\n-before\n+after\n*** Add File: second.txt\n+new\n*** End Patch"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(body["payload"]["operation"], "atomic_multi_file_patch");
    assert_eq!(body["payload"]["result"]["preview"]["file_count"], 2);
    assert_eq!(body["refs"][0]["label"], "repo_evidence");
    assert_eq!(
        fs::read_to_string(repo.join("first.txt")).unwrap(),
        "after\n"
    );
    assert_eq!(
        fs::read_to_string(repo.join("second.txt")).unwrap(),
        "new\n"
    );

    let events = store.read_events(&run_id).unwrap();
    let applied = events
        .iter()
        .find(|event| event.kind == "patch.applied")
        .unwrap();
    assert_eq!(applied.payload["tool_name"], "apply_patch");
    assert_eq!(applied.payload["files"].as_array().unwrap().len(), 2);
    assert!(applied
        .refs
        .iter()
        .any(|reference| reference.label == "patch_evidence"));
    let evidence = store.list_repo_evidence(&run_id).unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].scope_paths, vec!["first.txt", "second.txt"]);

    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_apply_patch_permission_denial_prevents_all_writes() {
    let store_root = temp_root();
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-apply-patch-denied");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .write_files = ConfigPermissionDecision::Deny;
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-apply-patch-denied",
            "tool_name": "apply_patch",
            "run_id": run_id.as_str(),
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "approved": true,
                "patch": "*** Begin Patch\n*** Add File: denied.txt\n+no\n*** End Patch"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert_eq!(body["payload"]["blocked_by"], "permission_decision");
    assert!(!repo.join("denied.txt").exists());

    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_execute_endpoint_returns_tool_result_with_evidence_refs() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Model tool bridge\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-read-1",
            "tool_name": "repo_read_file",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-read"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["contract"], "coder.model_tool_result.v1");
    assert_eq!(body["type"], "tool_result");
    assert_eq!(body["tool_use_id"], "toolu-read-1");
    assert_eq!(body["tool_name"], "repo_read_file");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["is_error"], false);
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("Model tool bridge"));
    assert_eq!(body["refs"][0]["label"], "repo_evidence");
    assert!(body["refs"][0]["uri"]
        .as_str()
        .unwrap()
        .starts_with("repo-evidence://"));
    let evidence = store
        .list_repo_evidence(&RunId::from_string("run-model-tool-read"))
        .unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].kind, RepoEvidenceKind::RepoRead);
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_execute_endpoint_persists_large_tool_result_content() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let large_content = format!("{}END_MARKER", "0123456789abcdef\n".repeat(4_000));
    fs::write(repo.join("large.txt"), &large_content).unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-large-read",
            "tool_name": "repo_read_file",
            "input": {
                "repo_root": repo,
                "path": "large.txt",
                "max_file_bytes": 100_000,
                "run_id": "run-model-tool-large"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let content = body["content"].as_str().unwrap();
    assert!(content.starts_with("<persisted-output>"));
    assert!(content.contains("Full output saved to: blob://sha256/"));
    assert!(content.contains("Preview (first 2KB):"));
    assert!(!content.contains("END_MARKER"));
    assert_eq!(body["content_truncated"], true);

    let storage = &body["payload"]["model_tool_result_storage"];
    assert_eq!(storage["contract"], "coder.model_tool_result_storage.v1");
    assert_eq!(storage["policy"], "persist_large_tool_result");
    assert_eq!(storage["threshold_chars"], 50_000);
    assert_eq!(storage["preview_size_bytes"], 2_000);
    assert!(storage["original_size_bytes"].as_u64().unwrap() > 50_000);
    assert!(storage["persisted_size_bytes"].as_u64().unwrap() < 5_000);

    let blob_ref = storage["blob_ref"].as_str().unwrap();
    assert!(body["refs"].as_array().unwrap().iter().any(|reference| {
        reference["label"].as_str() == Some("model_tool_result_blob")
            && reference["uri"].as_str() == Some(blob_ref)
    }));
    let digest = blob_ref.strip_prefix("blob://sha256/").unwrap();
    let loaded = store.read_blob_sha256(digest).unwrap();
    let loaded_text = String::from_utf8(loaded).unwrap();
    assert!(loaded_text.contains("large.txt"));
    assert!(loaded_text.contains("END_MARKER"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_execute_skill_records_invoked_skill_event() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-skill-tool");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({
                    "workflow_id": "code",
                    "repo_root": ""
                }),
            ),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-skill",
            "tool_name": "Skill",
            "run_id": "run-skill-tool",
            "harness_id": "native-code-edit",
            "agent_id": "agent-child",
            "input": {
                "skill": "coder.repo-review"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["contract"], "coder.model_tool_result.v1");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["is_error"], false);
    assert_eq!(body["payload"]["contract"], "coder.skill_tool_result.v1");
    assert_eq!(body["payload"]["skill_name"], "coder.repo-review");
    assert_eq!(
        body["payload"]["skill_path"],
        "builtin://skills/coder.repo-review"
    );
    assert_eq!(body["payload"]["agent_id"], "agent-child");
    assert_eq!(body["payload"]["event_kind"], "skill.invoked");
    assert!(body["payload"]["content"]
        .as_str()
        .unwrap()
        .contains("Repository Review"));

    let phases = body["payload"]["model_tool_phases"].as_array().unwrap();
    let permission_phase = phases
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(permission_phase["required_permission"], "read_files");
    assert_eq!(
        permission_phase["policy_decision_status"],
        "allowed_by_policy"
    );

    let events = store.read_events(&run_id).unwrap();
    let invoked = events
        .iter()
        .find(|event| event.kind == "skill.invoked")
        .unwrap();
    assert_eq!(invoked.payload["contract"], "coder.invoked_skill.v1");
    assert_eq!(invoked.payload["skill_name"], "coder.repo-review");
    assert_eq!(
        body["payload"]["event_sequence"].as_u64(),
        Some(invoked.sequence)
    );
    assert_eq!(
        invoked.payload["skill_path"],
        "builtin://skills/coder.repo-review"
    );
    assert_eq!(invoked.payload["agent_id"], "agent-child");
    assert!(invoked.payload["content"]
        .as_str()
        .unwrap()
        .contains("Base directory for this skill"));
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_execute_skill_loads_local_skill_from_extra_root() {
    let store_root = temp_root();
    let skill_root = temp_root();
    let skill_dir = skill_root.join("custom-review");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: Custom Review
description: Review local code changes with project-specific rules.
allowed-tools:
  - Read
  - Grep
model: opus
effort: high
context: inline
user-invocable: false
hooks:
  PreToolUse:
    - matcher: Read
      hooks:
        - type: command
          command: echo should-not-run
mcpServers:
  sample:
    command: npx
permissionMode: bypassPermissions
---
# Custom Review

Use files from ${CLAUDE_SKILL_DIR}.
Session: ${CLAUDE_SESSION_ID}.
"#,
    )
    .unwrap();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-local-skill");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({
                    "workflow_id": "code",
                    "repo_root": ""
                }),
            ),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let add_root = post_json(
        app.clone(),
        "/api/v3/skills/extra-roots",
        json!({
            "path": skill_root.display().to_string(),
            "scope": "project"
        }),
    )
    .await;
    assert_eq!(add_root.status(), StatusCode::OK);

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-local-skill",
            "tool_name": "Skill",
            "run_id": "run-local-skill",
            "harness_id": "native-code-edit",
            "agent_id": "agent-local",
            "input": {
                "skill": "custom-review"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(body["payload"]["contract"], "coder.skill_tool_result.v1");
    assert_eq!(body["payload"]["skill_name"], "custom-review");
    assert_eq!(body["payload"]["display_name"], "Custom Review");
    assert_eq!(body["payload"]["skill_origin"], "local_extra_root");
    assert!(body["payload"]["skill_path"]
        .as_str()
        .unwrap()
        .ends_with("/custom-review/SKILL.md"));
    assert!(body["payload"]["base_dir"]
        .as_str()
        .unwrap()
        .ends_with("/custom-review"));
    assert_eq!(body["payload"]["frontmatter"]["scope"], "project");
    assert_eq!(
        body["payload"]["frontmatter"]["description"],
        "Review local code changes with project-specific rules."
    );
    assert_eq!(
        body["payload"]["execution_policy"]["allowed_tools"],
        json!(["Read", "Grep"])
    );
    assert_eq!(body["payload"]["execution_policy"]["model"], "opus");
    assert_eq!(body["payload"]["execution_policy"]["effort"], "high");
    assert_eq!(body["payload"]["execution_policy"]["context"], "inline");
    assert_eq!(
        body["payload"]["execution_policy"]["unsupported_frontmatter_fields"],
        json!(["hooks"])
    );
    assert_eq!(
        body["payload"]["execution_policy"]["ignored_trust_boundary_fields"],
        json!(["mcpServers", "permissionMode"])
    );
    assert_eq!(
        body["payload"]["execution_policy"]["disable_model_invocation"],
        false
    );
    assert_eq!(body["payload"]["execution_policy"]["user_invocable"], false);
    let content = body["payload"]["content"].as_str().unwrap();
    assert!(content.contains("Base directory for this skill"));
    assert!(content.contains("Custom Review"));
    assert!(content.contains("run-local-skill"));
    assert!(!content.contains("${CLAUDE_SKILL_DIR}"));
    assert!(!content.contains("${CLAUDE_SESSION_ID}"));
    assert!(!content.contains("description: Review local code changes"));

    let events = store.read_events(&run_id).unwrap();
    let invoked = events
        .iter()
        .find(|event| event.kind == "skill.invoked")
        .unwrap();
    assert_eq!(invoked.payload["skill_name"], "custom-review");
    assert_eq!(invoked.payload["display_name"], "Custom Review");
    assert_eq!(invoked.payload["skill_origin"], "local_extra_root");
    assert_eq!(invoked.payload["agent_id"], "agent-local");
    assert_eq!(
        invoked.payload["execution_policy"]["allowed_tools"],
        json!(["Read", "Grep"])
    );
    assert_eq!(invoked.payload["execution_policy"]["model"], "opus");
    assert_eq!(invoked.payload["execution_policy"]["effort"], "high");
    assert_eq!(
        invoked.payload["execution_policy"]["unsupported_frontmatter_fields"],
        json!(["hooks"])
    );
    assert_eq!(
        invoked.payload["execution_policy"]["ignored_trust_boundary_fields"],
        json!(["mcpServers", "permissionMode"])
    );
    assert!(invoked.payload["content"]
        .as_str()
        .unwrap()
        .contains("run-local-skill"));
    let _ = fs::remove_dir_all(skill_root);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_execute_skill_respects_disable_model_invocation_frontmatter() {
    let store_root = temp_root();
    let skill_root = temp_root();
    let skill_dir = skill_root.join("manual-only");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: Manual Only
description: This skill must not be callable by SkillTool.
disable-model-invocation: true
---
# Manual Only

This should not enter the model context.
"#,
    )
    .unwrap();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-disabled-skill");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let add_root = post_json(
        app.clone(),
        "/api/v3/skills/extra-roots",
        json!({
            "path": skill_root.display().to_string(),
            "scope": "project"
        }),
    )
    .await;
    assert_eq!(add_root.status(), StatusCode::OK);

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-disabled-skill",
            "tool_name": "Skill",
            "run_id": "run-disabled-skill",
            "harness_id": "native-code-edit",
            "input": {
                "skill": "manual-only"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "failed");
    assert_eq!(body["is_error"], true);
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("disable-model-invocation"));
    let events = store.read_events(&run_id).unwrap();
    assert!(!events.iter().any(|event| event.kind == "skill.invoked"));
    let _ = fs::remove_dir_all(skill_root);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_returns_skill_context_modifier_attachment() {
    let store_root = temp_root();
    let skill_root = temp_root();
    let skill_dir = skill_root.join("turn-policy");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: Turn Policy
description: Adds scoped context modifiers for the next model turn.
allowed-tools: Read, Grep
model: opus
effort: high
---
# Turn Policy

Use the policy for the next model turn.
"#,
    )
    .unwrap();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-skill-turn-policy");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let add_root = post_json(
        app.clone(),
        "/api/v3/skills/extra-roots",
        json!({
            "path": skill_root.display().to_string(),
            "scope": "project"
        }),
    )
    .await;
    assert_eq!(add_root.status(), StatusCode::OK);

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-skill-turn-policy",
            "harness_id": "native-code-edit",
            "agent_id": "agent-policy",
            "current_model": "opus[1m]",
            "tool_uses": [
                {
                    "id": "toolu-turn-policy",
                    "name": "Skill",
                    "input": {
                        "skill": "turn-policy"
                    }
                }
            ]
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["contract"], "coder.model_tool_turn.v1");
    assert_eq!(body["results"].as_array().unwrap().len(), 1);
    assert_eq!(body["results"][0]["status"], "completed");
    let attachments = body["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    let modifier = &attachments[0];
    assert_eq!(modifier["contract"], "coder.model_tool_turn_attachment.v1");
    assert_eq!(modifier["type"], "skill_context_modifier");
    assert_eq!(
        modifier["modifier_contract"],
        "coder.skill_context_modifier.v1"
    );
    assert_eq!(modifier["tool_use_id"], "toolu-turn-policy");
    assert_eq!(modifier["skill_name"], "turn-policy");
    assert_eq!(modifier["applies_to"], "next_model_turn");
    assert_eq!(
        modifier["application_status"],
        "propagated_for_next_model_turn"
    );
    assert_eq!(
        modifier["modifier"]["allowed_tools"],
        json!(["Read", "Grep"])
    );
    assert_eq!(modifier["modifier"]["model"], "opus[1m]");
    assert_eq!(modifier["modifier"]["requested_model"], "opus");
    assert_eq!(modifier["modifier"]["current_model"], "opus[1m]");
    assert_eq!(modifier["modifier"]["effort"], "high");
    assert!(modifier.get("model_content").is_none());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| event.kind == "skill.invoked"));
    let _ = fs::remove_dir_all(skill_root);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_execute_skill_loads_enabled_installed_skill_summary() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-installed-skill");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let install = post_json(
        app.clone(),
        "/api/v3/skills/install",
        json!({
            "skill_id": "coder.repo-review",
            "registry_url": "builtin://skills/selftest-registry"
        }),
    )
    .await;
    assert_eq!(install.status(), StatusCode::OK);

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-installed-skill",
            "tool_name": "Skill",
            "run_id": "run-installed-skill",
            "harness_id": "native-code-edit",
            "agent_id": "agent-installed",
            "input": {
                "skill": "coder.repo-review"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(body["payload"]["skill_origin"], "installed_skill");
    assert_eq!(body["payload"]["skill_name"], "coder.repo-review");
    assert_eq!(
        body["payload"]["skill_path"],
        "builtin://skills/selftest-registry"
    );
    assert_eq!(
        body["payload"]["frontmatter"]["source"],
        "installed_skill_summary_projection"
    );
    assert_eq!(
        body["payload"]["execution_policy"]["ignored_trust_boundary_fields"],
        json!(["hooks", "mcpServers", "permissionMode"])
    );
    let content = body["payload"]["content"].as_str().unwrap();
    assert!(
        content.contains("This installed skill is represented by Coder's installed skill summary.")
    );
    assert!(
        content.contains("Package hooks, MCP servers, and permission mode fields are not executed")
    );

    let events = store.read_events(&run_id).unwrap();
    let invoked = events
        .iter()
        .find(|event| event.kind == "skill.invoked")
        .unwrap();
    assert_eq!(invoked.payload["skill_origin"], "installed_skill");
    assert_eq!(invoked.payload["agent_id"], "agent-installed");
    assert!(invoked.payload["content"]
        .as_str()
        .unwrap()
        .contains("installed skill summary"));
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_execute_skill_disabled_installed_skill_blocks_builtin_fallback() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-disabled-installed-skill");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let install = post_json(
        app.clone(),
        "/api/v3/skills/install",
        json!({
            "skill_id": "coder.repo-review",
            "registry_url": "builtin://skills/selftest-registry"
        }),
    )
    .await;
    assert_eq!(install.status(), StatusCode::OK);
    let disable = post_json(
        app.clone(),
        "/api/v3/skills/coder.repo-review/disable",
        json!({}),
    )
    .await;
    assert_eq!(disable.status(), StatusCode::OK);

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-disabled-installed-skill",
            "tool_name": "Skill",
            "run_id": "run-disabled-installed-skill",
            "harness_id": "native-code-edit",
            "input": {
                "skill": "coder.repo-review"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "failed");
    assert_eq!(body["is_error"], true);
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("Unknown skill: coder.repo-review"));
    let events = store.read_events(&run_id).unwrap();
    assert!(!events.iter().any(|event| event.kind == "skill.invoked"));
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_skill_context_modifier_allows_scoped_read_tool() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-skill-modifier-read-allow");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .read_files = ConfigPermissionDecision::Ask;
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Skill modifier\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-skill-modifier-read-allow",
            "harness_id": "native-code-edit",
            "skill_context_modifiers": [skill_context_modifier_fixture(["Read"])],
            "tool_uses": [{
                "id": "toolu-read-after-skill",
                "name": "repo_read_file",
                "input": {
                    "repo_root": repo,
                    "path": "README.md"
                }
            }]
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let result = &body["results"].as_array().unwrap()[0];
    assert_eq!(result["status"], "completed");
    assert_eq!(result["is_error"], false);
    let permission_phase = result["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(permission_phase["status"], "delegated_to_tool_endpoint");
    assert_eq!(permission_phase["required_permission"], "read_files");
    assert_eq!(permission_phase["permission_result"]["behavior"], "ask");
    assert_eq!(
        permission_phase["policy_decision_status"],
        "allowed_by_skill_context_modifier"
    );
    assert_eq!(
        permission_phase["skill_context_modifier"]["contract"],
        "coder.skill_context_modifier_permission.v1"
    );
    assert_eq!(
        permission_phase["skill_context_modifier"]["policy"],
        "allowed_tools_read_and_scoped_commands"
    );
    assert_eq!(
        permission_phase["skill_context_modifier"]["allowed_by_modifier"],
        true
    );
    assert_eq!(
        permission_phase["skill_context_modifier"]["matched_allowed_tool"],
        "Read"
    );
    assert_eq!(
        permission_phase["skill_context_modifier"]["skill_name"],
        "fixture-skill"
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_skill_context_modifier_allows_scoped_bash_command_rule() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-skill-modifier-command-allow");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Ask;
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    run_git(&repo, &["init"]);

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-skill-modifier-command-allow",
            "harness_id": "native-code-edit",
            "skill_context_modifiers": [skill_context_modifier_fixture(["Bash(git status:*)"])],
            "tool_uses": [{
                "id": "toolu-command-after-skill",
                "name": "command_run",
                "input": {
                    "repo_root": repo,
                    "cwd": ".",
                    "argv": ["git", "status", "--short"],
                    "source": "model",
                    "sandbox": true
                }
            }]
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let result = &body["results"].as_array().unwrap()[0];
    assert_eq!(result["status"], "completed");
    assert_eq!(result["is_error"], false);
    assert_eq!(result["payload"]["result"]["status"], "completed");
    let permission_phase = result["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(permission_phase["required_permission"], "run_commands");
    assert_eq!(permission_phase["permission_result"]["behavior"], "ask");
    assert_eq!(
        permission_phase["policy_decision_status"],
        "allowed_by_skill_context_modifier"
    );
    assert_eq!(
        permission_phase["skill_context_modifier"]["matched_allowed_tool"],
        "Bash(git status:*)"
    );
    assert_eq!(
        permission_phase["skill_context_modifier"]["matched_allowed_tool_kind"],
        "command_rule"
    );
    assert_eq!(
        permission_phase["skill_context_modifier"]["matched_rule_content"],
        "git status:*"
    );
    assert_eq!(
        permission_phase["skill_context_modifier"]["matched_command"],
        "git status --short"
    );
    let tool_execution_phase = result["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("tool_execution"))
        .unwrap();
    assert_eq!(
        tool_execution_phase["policy_approval_defaults"]["reason"],
        "active_skill_context_modifier_allowed_tool"
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_skill_context_modifier_blocks_unmatched_bash_command_rule() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-skill-modifier-command-miss");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Ask;
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-skill-modifier-command-miss",
            "harness_id": "native-code-edit",
            "skill_context_modifiers": [skill_context_modifier_fixture(["Bash(git status:*)"])],
            "tool_uses": [{
                "id": "toolu-command-after-skill-miss",
                "name": "command_run",
                "input": {
                    "repo_root": repo,
                    "cwd": ".",
                    "argv": ["git", "diff", "--shortstat"],
                    "source": "model",
                    "sandbox": true
                }
            }]
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let result = &body["results"].as_array().unwrap()[0];
    assert_eq!(result["status"], "blocked");
    assert_eq!(result["payload"]["blocked_by"], "permission_decision");
    let permission_phase = result["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(
        permission_phase["policy_decision_status"],
        "requires_confirmation"
    );
    assert_eq!(
        permission_phase["skill_context_modifier"]["status"],
        "no_matching_allowed_tool"
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_skill_context_modifier_does_not_allow_unlisted_read_tool() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-skill-modifier-read-miss");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .read_files = ConfigPermissionDecision::Ask;
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Skill modifier\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-skill-modifier-read-miss",
            "harness_id": "native-code-edit",
            "skill_context_modifiers": [skill_context_modifier_fixture(["Read"])],
            "tool_uses": [{
                "id": "toolu-grep-after-skill",
                "name": "repo_search_text",
                "input": {
                    "repo_root": repo,
                    "query": "Skill"
                }
            }]
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let result = &body["results"].as_array().unwrap()[0];
    assert_eq!(result["status"], "blocked");
    assert_eq!(result["payload"]["blocked_by"], "permission_decision");
    let permission_phase = result["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(
        permission_phase["policy_decision_status"],
        "requires_confirmation"
    );
    assert_eq!(
        permission_phase["skill_context_modifier"]["status"],
        "no_matching_allowed_tool"
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_skill_context_modifier_does_not_override_deny() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-skill-modifier-read-deny");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .read_files = ConfigPermissionDecision::Deny;
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Skill modifier\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-skill-modifier-read-deny",
            "harness_id": "native-code-edit",
            "skill_context_modifiers": [skill_context_modifier_fixture(["Read"])],
            "tool_uses": [{
                "id": "toolu-read-after-deny",
                "name": "repo_read_file",
                "input": {
                    "repo_root": repo,
                    "path": "README.md"
                }
            }]
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let result = &body["results"].as_array().unwrap()[0];
    assert_eq!(result["status"], "blocked");
    assert_eq!(result["payload"]["blocked_by"], "permission_decision");
    let permission_phase = result["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(
        permission_phase["policy_decision_status"],
        "denied_by_policy"
    );
    assert_eq!(
        permission_phase["skill_context_modifier"]["status"],
        "not_applied_policy_denied"
    );
    assert_eq!(
        permission_phase["skill_context_modifier"]["allowed_by_modifier"],
        false
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_execute_skill_fork_context_runs_subagent_boundary() {
    let store_root = temp_root();
    let skill_root = temp_root();
    let skill_dir = skill_root.join("fork-only");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: Fork Only
description: Must run in an isolated skill subagent.
context: fork
agent: general-purpose
model: opus
effort: max
---
# Fork Only

This should not be inlined into the parent model turn.
"#,
    )
    .unwrap();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-fork-skill");
    let mut config = default_project_config();
    let mut general_profile = config.task_profiles["code"].clone();
    general_profile.instructions = "General purpose skill subagent.".to_owned();
    config
        .task_profiles
        .insert("general-purpose".to_owned(), general_profile);
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.backend = "mock".to_owned();
    harness.tools = vec!["Skill".to_owned(), "agent_subagent".to_owned()];
    harness.permissions.child_harness_permissions = ConfigPermissionDecision::Allow;
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let add_root = post_json(
        app.clone(),
        "/api/v3/skills/extra-roots",
        json!({
            "path": skill_root.display().to_string(),
            "scope": "project"
        }),
    )
    .await;
    assert_eq!(add_root.status(), StatusCode::OK);

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-fork-skill",
            "tool_name": "Skill",
            "run_id": "run-fork-skill",
            "harness_id": "native-code-edit",
            "agent_id": "parent-agent",
            "input": {
                "skill": "fork-only",
                "args": "check the fork boundary"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(body["is_error"], false);
    assert_eq!(body["payload"]["contract"], "coder.skill_tool_result.v1");
    assert_eq!(body["payload"]["execution_context"], "fork");
    assert_eq!(body["payload"]["skill_name"], "fork-only");
    assert_eq!(
        body["payload"]["execution_policy"]["agent"],
        "general-purpose"
    );
    assert_eq!(body["payload"]["execution_policy"]["model"], "opus");
    assert_eq!(body["payload"]["execution_policy"]["effort"], "max");
    assert_eq!(
        body["payload"]["subagent"]["events"][0]["payload"]["status"],
        "completed"
    );
    assert_eq!(body["payload"]["content_recorded_in_event"], true);
    assert!(body["payload"].get("content").is_none());
    assert_eq!(body["payload"]["subagent"]["status"], "completed");
    assert_eq!(
        body["payload"]["subagent"]["events"][0]["kind"],
        "backend.native_mock.completed"
    );
    assert!(body["payload"]["metadata_ref"]
        .as_str()
        .unwrap()
        .contains("subagents/agent-"));
    assert!(body["payload"]["transcript_ref"]
        .as_str()
        .unwrap()
        .contains("subagents/agent-"));
    let phases = body["payload"]["model_tool_phases"].as_array().unwrap();
    let permission_phase = phases
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(
        permission_phase["required_permission"],
        "child_harness_permissions"
    );
    assert_eq!(
        permission_phase["policy_decision_status"],
        "allowed_by_policy"
    );
    assert_eq!(
        permission_phase["required_permission"],
        body["phases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
            .unwrap()["required_permission"]
    );

    let events = store.read_events(&run_id).unwrap();
    let invoked = events
        .iter()
        .find(|event| event.kind == "skill.invoked")
        .unwrap();
    assert_eq!(invoked.payload["skill_name"], "fork-only");
    assert_eq!(invoked.payload["execution_context"], "fork");
    assert_eq!(invoked.payload["agent_id"], "parent-agent");
    assert!(invoked.payload["content"]
        .as_str()
        .unwrap()
        .contains("Fork Only"));

    let child_agent_id = body["payload"]["subagent"]["agent_id"].as_str().unwrap();
    let metadata = store
        .read_subagent_metadata(&run_id, child_agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(metadata.parent_agent_id, "parent-agent");
    assert_eq!(metadata.description.as_deref(), Some("general-purpose"));
    let transcript = store
        .read_subagent_transcript_records(&run_id, child_agent_id)
        .unwrap();
    assert!(transcript.iter().any(|record| {
        record.kind == "subagent.started"
            && record.payload["context"]["subagent_name"] == "general-purpose"
            && record.payload.get("runtime_projection").is_none()
    }));
    assert!(transcript.iter().any(|record| {
        record.kind == "subagent.user"
            && record.payload["task"]
                .as_str()
                .unwrap()
                .contains("This should not be inlined into the parent model turn.")
    }));
    let _ = fs::remove_dir_all(skill_root);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_execute_unknown_skill_does_not_record_invocation() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-unknown-skill");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-skill-missing",
            "tool_name": "Skill",
            "run_id": "run-unknown-skill",
            "harness_id": "native-code-edit",
            "input": {
                "skill": "missing-skill"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "failed");
    assert_eq!(body["is_error"], true);
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("Unknown skill: missing-skill"));
    let events = store.read_events(&run_id).unwrap();
    assert!(!events.iter().any(|event| event.kind == "skill.invoked"));
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_endpoint_persists_aggregate_tool_result_budget() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let sizes = [42_000usize, 41_500, 41_000, 40_500, 40_000];
    for (index, size) in sizes.iter().enumerate() {
        let marker = if index == 0 {
            "FIRST_AGGREGATE_MARKER"
        } else {
            ""
        };
        let content = format!("{}{}", "x".repeat(*size), marker);
        fs::write(repo.join(format!("file-{index}.txt")), content).unwrap();
    }
    let repo_root = repo.display().to_string();

    let tool_uses = (0..sizes.len())
        .map(|index| {
            json!({
                "id": format!("toolu-aggregate-{index}"),
                "name": "repo_read_file",
                "input": {
                    "repo_root": repo_root,
                    "path": format!("file-{index}.txt"),
                    "run_id": "run-model-tool-aggregate"
                }
            })
        })
        .collect::<Vec<_>>();

    let response = post_json(
        app.clone(),
        "/api/v3/tools/model/turn",
        json!({
            "max_tool_use_concurrency": 10,
            "tool_uses": tool_uses.clone()
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), sizes.len());
    let persisted = results
        .iter()
        .filter(|result| {
            result["payload"]["model_tool_result_storage"]["policy"].as_str()
                == Some("persist_aggregate_tool_result_budget")
        })
        .collect::<Vec<_>>();
    assert_eq!(persisted.len(), 1);
    let result = persisted[0];
    assert_eq!(result["tool_use_id"], "toolu-aggregate-0");
    assert_eq!(result["content_truncated"], true);
    let content = result["content"].as_str().unwrap();
    assert!(content.starts_with("<persisted-output>"));
    assert!(!content.contains("FIRST_AGGREGATE_MARKER"));

    let storage = &result["payload"]["model_tool_result_storage"];
    assert_eq!(storage["contract"], "coder.model_tool_result_storage.v1");
    assert_eq!(storage["max_tool_results_per_message_chars"], 200_000);
    assert_eq!(storage["selection_strategy"], "largest_fresh_results");
    assert_eq!(storage["content_replacement_record"]["kind"], "tool-result");
    assert_eq!(
        storage["content_replacement_record"]["toolUseId"],
        "toolu-aggregate-0"
    );
    assert!(storage["content_replacement_record"]["replacement"]
        .as_str()
        .unwrap()
        .starts_with("<persisted-output>"));
    assert_eq!(
        storage["content_replacement_persistence"]["persisted"],
        true
    );
    assert_eq!(
        storage["content_replacement_persistence"]["run_id"],
        "run-model-tool-aggregate"
    );
    assert_eq!(storage["content_replacement_persistence"]["sequence"], 1);

    let blob_ref = storage["blob_ref"].as_str().unwrap();
    assert!(result["refs"].as_array().unwrap().iter().any(|reference| {
        reference["label"].as_str() == Some("model_tool_result_blob")
            && reference["uri"].as_str() == Some(blob_ref)
    }));
    let digest = blob_ref.strip_prefix("blob://sha256/").unwrap();
    let loaded = store.read_blob_sha256(digest).unwrap();
    let loaded_text = String::from_utf8(loaded).unwrap();
    assert!(loaded_text.contains("file-0.txt"));
    assert!(loaded_text.contains("FIRST_AGGREGATE_MARKER"));
    let replacement_records = store
        .read_run_content_replacement_records(&RunId::from_string("run-model-tool-aggregate"))
        .unwrap();
    assert_eq!(replacement_records.len(), 1);
    assert_eq!(replacement_records[0].kind, "content-replacement");
    assert_eq!(
        replacement_records[0].replacements[0].tool_use_id,
        "toolu-aggregate-0"
    );
    assert!(replacement_records[0].replacements[0]
        .replacement
        .starts_with("<persisted-output>"));

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "max_tool_use_concurrency": 10,
            "tool_uses": tool_uses
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let result = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|result| result["tool_use_id"].as_str() == Some("toolu-aggregate-0"))
        .unwrap();
    assert_eq!(
        result["payload"]["model_tool_result_storage"]["selection_strategy"],
        "stable_replacement_reapply"
    );
    assert_eq!(
        result["payload"]["model_tool_result_storage"]["reapplied"],
        true
    );
    let replacement_records = store
        .read_run_content_replacement_records(&RunId::from_string("run-model-tool-aggregate"))
        .unwrap();
    assert_eq!(replacement_records.len(), 1);

    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_replays_only_bounded_content_replacement_tail() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));
    let run_id = RunId::from_string("run-model-tool-replacement-tail");
    store
        .append_run_content_replacement_record_next(
            &run_id,
            vec![coder_store::ContentReplacementRecord {
                kind: "tool-result".to_owned(),
                tool_use_id: "toolu-tail-0".to_owned(),
                replacement: "<persisted-output>stale replacement outside tail</persisted-output>"
                    .to_owned(),
            }],
        )
        .unwrap();
    for index in 1..=RUN_RESUME_CONTENT_REPLACEMENT_RECORD_LIMIT {
        store
            .append_run_content_replacement_record_next(
                &run_id,
                vec![coder_store::ContentReplacementRecord {
                    kind: "tool-result".to_owned(),
                    tool_use_id: format!("toolu-tail-dummy-{index}"),
                    replacement: format!("<persisted-output>dummy-{index}</persisted-output>"),
                }],
            )
            .unwrap();
    }

    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let sizes = [42_000usize, 41_500, 41_000, 40_500, 40_000];
    for (index, size) in sizes.iter().enumerate() {
        fs::write(
            repo.join(format!("file-{index}.txt")),
            format!("{}TAIL_MARKER_{index}", "x".repeat(*size)),
        )
        .unwrap();
    }
    let repo_root = repo.display().to_string();
    let tool_uses = (0..sizes.len())
        .map(|index| {
            json!({
                "id": format!("toolu-tail-{index}"),
                "name": "repo_read_file",
                "input": {
                    "repo_root": repo_root,
                    "path": format!("file-{index}.txt"),
                    "run_id": "run-model-tool-replacement-tail"
                }
            })
        })
        .collect::<Vec<_>>();

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "max_tool_use_concurrency": 10,
            "tool_uses": tool_uses
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let result = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|result| result["tool_use_id"].as_str() == Some("toolu-tail-0"))
        .unwrap();
    let storage = &result["payload"]["model_tool_result_storage"];
    assert_eq!(storage["selection_strategy"], "largest_fresh_results");
    assert_eq!(storage["reapplied"], false);
    assert_eq!(storage["content_replacement_persistence"]["sequence"], 102);
    assert!(!result["content"]
        .as_str()
        .unwrap()
        .contains("stale replacement outside tail"));
    let replacement_records = store.read_run_content_replacement_records(&run_id).unwrap();
    assert_eq!(
        replacement_records.len(),
        RUN_RESUME_CONTENT_REPLACEMENT_RECORD_LIMIT + 2
    );

    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[test]
fn model_tool_content_replacement_state_clears_run_scope_after_compaction() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-post-compact-cleanup");
    let state = Arc::new(Mutex::new(
        model_tool_result_storage::ModelToolContentReplacementState::default(),
    ));
    let mut results = Vec::new();
    for index in 0..5 {
        let tool_use_id = format!("toolu-cleanup-{index}");
        state
            .lock()
            .unwrap()
            .record_tool_run_id(tool_use_id.clone(), run_id.clone());
        results.push(ModelToolResultBlock {
            contract: "coder.model_tool_result.v1",
            source: "test",
            result_type: "tool_result",
            tool_use_id,
            tool_name: "repo_read_file".to_owned(),
            status: "success".to_owned(),
            is_error: false,
            content: "x".repeat(42_000),
            content_truncated: false,
            payload: json!({}),
            refs: Vec::new(),
            phases: Vec::new(),
        });
    }

    let _ = model_tool_result_storage::enforce_aggregate_model_tool_result_budget(
        &store, &state, results,
    );

    let cleanup =
        model_tool_result_storage::clear_content_replacement_state_for_run(&state, &run_id);
    assert_eq!(
        cleanup["contract"],
        "coder.model_tool_content_replacement_cleanup.v1"
    );
    assert_eq!(cleanup["status"], "completed");
    assert_eq!(cleanup["removed_tool_run_ids"], 5);
    assert_eq!(cleanup["removed_seen_ids"], 5);
    assert_eq!(cleanup["removed_replacements"], 1);
    assert_eq!(cleanup["removed_loaded_run_id"], true);

    let cleanup =
        model_tool_result_storage::clear_content_replacement_state_for_run(&state, &run_id);
    assert_eq!(cleanup["status"], "completed");
    assert_eq!(cleanup["removed_tool_run_ids"], 0);
    assert_eq!(cleanup["removed_seen_ids"], 0);
    assert_eq!(cleanup["removed_replacements"], 0);
    assert_eq!(cleanup["removed_loaded_run_id"], false);

    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_execute_endpoint_records_claude_style_phase_events() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Phase events\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-phase-read",
            "tool_name": "repo_read_file",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-phases"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases.len(), 4);
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["slow_phase_threshold_ms"], 2000);
    assert_eq!(phases[0]["hook_timing_display_threshold_ms"], 500);
    assert_eq!(phases[0]["slow_phase"], false);
    assert_eq!(phases[0]["show_inline_timing_summary"], false);
    assert_eq!(phases[1]["phase"], "permission_decision");
    assert_eq!(phases[1]["required_permission"], "read_files");
    assert_eq!(phases[1]["slow_phase_threshold_ms"], 2000);
    assert_eq!(phases[1]["slow_phase"], false);
    assert!(phases[1]["hook_timing_display_threshold_ms"].is_null());
    assert_eq!(
        phases[1]["permission_policy_source"]["harness_id"],
        "native-code-edit"
    );
    assert_eq!(
        phases[1]["permission_policy_source"]["type"],
        "default_project_config"
    );
    assert_eq!(
        phases[1]["permission_policy"]["contract"],
        "coder.permission_policy.v1"
    );
    assert_eq!(phases[1]["permission_result"]["behavior"], "allow");
    assert_eq!(
        phases[1]["permission_result"]["decisionReason"]["rule"]["source"],
        "policySettings"
    );
    assert_eq!(phases[1]["policy_decision_status"], "allowed_by_policy");
    assert_eq!(phases[2]["phase"], "tool_execution");
    assert_eq!(phases[2]["status"], "completed");
    assert_eq!(phases[3]["phase"], "post_tool_use_hooks");
    assert_eq!(
        body["payload"]["model_tool_phases"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let events = store
        .read_events(&RunId::from_string("run-model-tool-phases"))
        .unwrap();
    let phase_events = events
        .iter()
        .filter(|event| event.kind == "model_tool.phase")
        .collect::<Vec<_>>();
    assert_eq!(phase_events.len(), 4);
    assert_eq!(phase_events[1].payload["phase"], "permission_decision");
    assert_eq!(phase_events[1].payload["tool_use_id"], "toolu-phase-read");
    assert_eq!(phase_events[1].payload["required_permission"], "read_files");
    assert_eq!(
        phase_events[1].payload["permission_result"]["behavior"],
        "allow"
    );
    let progress_events = events
        .iter()
        .filter(|event| event.kind == "model_tool.phase.progress")
        .collect::<Vec<_>>();
    assert_eq!(progress_events.len(), 4);
    assert_eq!(
        progress_events[0].payload["contract"],
        "coder.model_tool_phase_progress.v1"
    );
    assert_eq!(progress_events[0].payload["progress_kind"], "phase_started");
    assert_eq!(progress_events[0].payload["phase"], "pre_tool_use_hooks");
    assert_eq!(progress_events[1].payload["phase"], "permission_decision");
    assert_eq!(progress_events[2].payload["phase"], "tool_execution");
    assert_eq!(progress_events[3].payload["phase"], "post_tool_use_hooks");
    assert_eq!(events[0].kind, "model_tool.phase.progress");
    assert_eq!(events[1].kind, "model_tool.phase");
    assert_eq!(events[0].payload["phase"], "pre_tool_use_hooks");
    assert_eq!(events[1].payload["phase"], "pre_tool_use_hooks");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_phase_progress_is_visible_while_tool_runs() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));
    let run_id = RunId::from_string("run-model-tool-live-progress");

    let request = json!({
        "tool_use_id": "toolu-live-progress",
        "tool_name": "sleep",
        "input": {
            "duration_ms": 1000,
            "run_id": run_id.as_str()
        }
    });
    let handle =
        tokio::spawn(async move { post_json(app, "/api/v3/tools/model/execute", request).await });

    let events = wait_for_events(&store, &run_id, |events| {
        events.iter().any(|event| {
            event.kind == "model_tool.phase.progress"
                && event.payload["phase"].as_str() == Some("tool_execution")
                && event.payload["status"].as_str() == Some("started")
        })
    })
    .await;
    assert!(
        !handle.is_finished(),
        "tool_execution progress should be visible before the tool returns"
    );
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.phase.progress"
            && event.payload["contract"].as_str() == Some("coder.model_tool_phase_progress.v1")
            && event.payload["phase"].as_str() == Some("tool_execution")
    }));

    let response = handle.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    let phases = body["phases"].as_array().unwrap();
    let tool_execution_phase = phases
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("tool_execution"))
        .unwrap();
    assert!(
        tool_execution_phase["duration_ms"].as_u64().unwrap() >= 900,
        "sleep tool should record wall-clock execution duration"
    );
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_phase_reports_configured_hooks_from_run_snapshot() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-hooks");
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
            command: "Write-Output pre".to_owned(),
        }
    } else {
        HookTestCommand {
            shell: "sh".to_owned(),
            command: "printf pre".to_owned(),
        }
    };
    config.hooks = serde_yaml::from_str::<coder_config::HookSettings>(&format!(
        r#"
PreToolUse:
  - matcher: repo_read_file
    hooks:
      - type: command
        shell: {}
        command: "{}"
PostToolUse:
  - matcher: "*"
    hooks:
      - type: webhook
        url: http://127.0.0.1:8765/hooks
"#,
        hook_command.shell, hook_command.command
    ))
    .unwrap();
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Hooks\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-hook-read",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-hooks"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["contract"], "coder.model_tool_hooks.v1");
    assert_eq!(phases[0]["hook_event"], "PreToolUse");
    assert_eq!(phases[0]["hook_config_source"], "run_config_snapshot");
    assert_eq!(phases[0]["matched_hook_count"], 1);
    assert_eq!(phases[0]["matched_hook_types"][0], "command");
    assert_eq!(phases[0]["executed_hook_count"], 1);
    assert_eq!(phases[0]["execution_status"], "completed");
    assert_eq!(phases[0]["hook_results"][0]["outcome"], "success");
    assert!(phases[0]["hook_results"][0]["output_preview"]
        .as_str()
        .unwrap()
        .contains("pre"));
    assert_eq!(phases[3]["phase"], "post_tool_use_hooks");
    assert_eq!(phases[3]["status"], "permission_blocked");
    assert_eq!(phases[3]["hook_event"], "PostToolUse");
    assert_eq!(phases[3]["matched_hook_count"], 1);
    assert_eq!(phases[3]["matched_hook_types"][0], "webhook");
    assert_eq!(phases[3]["execution_status"], "skipped_permission_required");
    assert_eq!(
        phases[3]["hook_results"][0]["required_permission"],
        "network"
    );

    let events = store.read_events(&run_id).unwrap();
    let pre_hook_event = events
        .iter()
        .find(|event| {
            event.kind == "model_tool.phase"
                && event.payload["phase"].as_str() == Some("pre_tool_use_hooks")
        })
        .unwrap();
    assert_eq!(pre_hook_event.payload["execution_status"], "completed");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_pre_hook_exit_code_two_blocks_tool_execution() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-hook-block");
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
            command: "Write-Output blocked; exit 2".to_owned(),
        }
    } else {
        HookTestCommand {
            shell: "sh".to_owned(),
            command: "printf blocked; exit 2".to_owned(),
        }
    };
    config.hooks = serde_yaml::from_str::<coder_config::HookSettings>(&format!(
        r#"
PreToolUse:
  - matcher: Read
    hooks:
      - type: command
        shell: {}
        command: "{}"
"#,
        block_hook.shell, block_hook.command
    ))
    .unwrap();
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Blocked\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-hook-block",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-hook-block"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert_eq!(body["is_error"], true);
    assert_eq!(body["payload"]["blocked_by"], "pre_tool_use_hook");
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases.len(), 4);
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "blocked");
    assert_eq!(phases[0]["hook_results"][0]["outcome"], "blocking");
    assert_eq!(phases[0]["hook_results"][0]["returncode"], 2);
    assert!(phases[0]["blocking_error"]
        .as_str()
        .unwrap()
        .contains("blocked"));
    assert_eq!(phases[1]["status"], "skipped_pre_tool_use_hook_blocked");
    assert_eq!(phases[2]["status"], "blocked");
    assert_eq!(phases[3]["status"], "skipped_pre_tool_use_hook_blocked");

    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.phase"
            && event.payload["phase"].as_str() == Some("pre_tool_use_hooks")
            && event.payload["status"].as_str() == Some("blocked")
    }));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_command_hooks_respect_run_commands_permission() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-hook-permission");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Deny;
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Permission\n").unwrap();
    let sentinel = repo.join("hook-executed.txt");
    let hook_command = if cfg!(windows) {
        format!("echo executed > \"{}\"", sentinel.display())
    } else {
        format!("printf executed > '{}'", sentinel.display())
    };
    config.hooks = serde_yaml::from_str::<coder_config::HookSettings>(&format!(
        r#"
PreToolUse:
  - matcher: repo_read_file
    hooks:
      - type: command
        command: |
          {hook_command}
"#
    ))
    .unwrap();
    store.write_run_config_snapshot(&run_id, &config).unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-hook-permission",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-hook-permission"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "permission_blocked");
    assert_eq!(phases[0]["execution_status"], "skipped_permission_required");
    assert_eq!(phases[0]["matched_hook_count"], 1);
    assert_eq!(phases[0]["executed_hook_count"], 0);
    assert_eq!(phases[0]["skipped_permission_count"], 1);
    assert_eq!(
        phases[0]["hook_results"][0]["outcome"],
        "skipped_permission_required"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["required_permission"],
        "run_commands"
    );
    assert_eq!(phases[0]["hook_results"][0]["permission_behavior"], "deny");
    assert!(!sentinel.exists());

    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.phase"
            && event.payload["phase"].as_str() == Some("pre_tool_use_hooks")
            && event.payload["status"].as_str() == Some("permission_blocked")
    }));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_async_command_hook_backgrounds_without_blocking_tool_execution() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-async-hook");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Async hook\n").unwrap();
    let stdin_capture = repo.join("async-hook-input.json");
    let sentinel = repo.join("async-hook-done.txt");
    let hook_command = hook_async_capture_stdin_command(&stdin_capture, &sentinel);
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
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
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-async-hook",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-async-hook"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"].as_str().unwrap().contains("Async hook"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["hook_results"][0]["outcome"], "backgrounded");
    assert_eq!(phases[0]["hook_results"][0]["async"], true);
    assert_eq!(phases[0]["hook_results"][0]["rewake_supported"], false);
    let async_hook_id = phases[0]["hook_results"][0]["async_hook_id"]
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
    let captured = fs::read_to_string(&stdin_capture).unwrap();
    let hook_input =
        serde_json::from_str::<Value>(captured.trim_start_matches('\u{feff}').trim()).unwrap();
    assert_eq!(hook_input["hook_event_name"], "PreToolUse");
    assert_eq!(hook_input["tool_name"], "repo_read_file");
    assert_eq!(hook_input["tool_use_id"], "toolu-async-hook");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_async_command_hook_delivers_response_attachment_on_next_turn() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-async-hook-response");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Async hook response\n").unwrap();
    let stdin_capture = repo.join("async-hook-response-input.json");
    let sentinel = repo.join("async-hook-response-done.txt");
    let hook_command = hook_async_response_command(&stdin_capture, &sentinel);
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
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
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-async-hook-response",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-async-hook-response"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("Async hook response"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["hook_results"][0]["outcome"], "backgrounded");
    assert_eq!(phases[0]["hook_results"][0]["async"], true);
    assert_eq!(phases[0]["hook_results"][0]["async_rewake"], false);
    let async_hook_id = phases[0]["hook_results"][0]["async_hook_id"]
        .as_str()
        .unwrap()
        .to_owned();

    wait_for_path(&sentinel).await;
    let response_events = wait_for_events(&store, &run_id, |events| {
        events.iter().any(|event| {
            event.kind == "model_tool.async_hook.response"
                && event.payload["async_hook_id"].as_str() == Some(async_hook_id.as_str())
        })
    })
    .await;
    assert!(response_events.iter().any(|event| {
        event.kind == "model_tool.async_hook.completed"
            && event.payload["async_hook_id"].as_str() == Some(async_hook_id.as_str())
            && event.payload["async_response_recorded"].as_bool() == Some(true)
    }));
    let response_event = response_events
        .iter()
        .find(|event| {
            event.kind == "model_tool.async_hook.response"
                && event.payload["async_hook_id"].as_str() == Some(async_hook_id.as_str())
        })
        .unwrap();
    assert_eq!(
        response_event.payload["response"]["systemMessage"],
        "async system note"
    );
    assert_eq!(
        response_event.payload["response"]["hookSpecificOutput"]["additionalContext"],
        "async context note"
    );
    assert_eq!(
        response_event.payload["delivery_status"],
        "recorded_not_delivered"
    );

    let turn_response = post_json(
        app.clone(),
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-model-tool-async-hook-response",
            "harness_id": "native-code-edit",
            "tool_uses": []
        }),
    )
    .await;
    assert_eq!(turn_response.status(), StatusCode::OK);
    let turn_body = response_json(turn_response).await;
    assert_eq!(turn_body["results"].as_array().unwrap().len(), 0);
    let attachments = turn_body["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["type"], "async_hook_response");
    assert_eq!(attachments[0]["processId"], async_hook_id);
    assert_eq!(attachments[0]["hookEvent"], "PreToolUse");
    assert_eq!(attachments[0]["toolName"], "repo_read_file");
    assert_eq!(
        attachments[0]["response"]["systemMessage"],
        "async system note"
    );
    let model_content = attachments[0]["model_content"].as_array().unwrap();
    assert_eq!(model_content.len(), 2);
    assert!(model_content[0]["text"]
        .as_str()
        .unwrap()
        .contains("<system-reminder>"));
    assert!(model_content[0]["text"]
        .as_str()
        .unwrap()
        .contains("async system note"));
    assert!(model_content[1]["text"]
        .as_str()
        .unwrap()
        .contains("async context note"));

    let delivered_events = store.read_events(&run_id).unwrap();
    assert!(delivered_events.iter().any(|event| {
        event.kind == "model_tool.async_hook.response.delivered"
            && event.payload["response_sequence"].as_u64() == Some(response_event.sequence)
            && event.payload["delivery_status"].as_str() == Some("delivered")
    }));

    let repeated_turn_response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-model-tool-async-hook-response",
            "harness_id": "native-code-edit",
            "tool_uses": []
        }),
    )
    .await;
    assert_eq!(repeated_turn_response.status(), StatusCode::OK);
    let repeated_turn_body = response_json(repeated_turn_response).await;
    assert_eq!(
        repeated_turn_body["attachments"].as_array().unwrap().len(),
        0
    );

    let captured = fs::read_to_string(&stdin_capture).unwrap();
    let hook_input =
        serde_json::from_str::<Value>(captured.trim_start_matches('\u{feff}').trim()).unwrap();
    assert_eq!(hook_input["hook_event_name"], "PreToolUse");
    assert_eq!(hook_input["tool_name"], "repo_read_file");
    assert_eq!(hook_input["tool_use_id"], "toolu-async-hook-response");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_async_rewake_hook_records_exit_code_two_notification() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-async-rewake-hook");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Async rewake hook\n").unwrap();
    let hook_command = if cfg!(windows) {
        HookTestCommand {
            shell: "powershell".to_owned(),
            command: "Write-Output 'rewake blocking reason'; exit 2".to_owned(),
        }
    } else {
        HookTestCommand {
            shell: "sh".to_owned(),
            command: "printf 'rewake blocking reason\\n'; exit 2".to_owned(),
        }
    };
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
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
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-async-rewake-hook",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-async-rewake-hook"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("Async rewake hook"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["hook_results"][0]["outcome"], "backgrounded");
    assert_eq!(phases[0]["hook_results"][0]["async_rewake"], true);
    assert_eq!(
        phases[0]["hook_results"][0]["rewake_delivery"],
        "recorded_on_exit_code_2_pending_model_turn_delivery"
    );
    let async_hook_id = phases[0]["hook_results"][0]["async_hook_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let events = wait_for_events(&store, &run_id, |events| {
        events.iter().any(|event| {
            event.kind == "model_tool.async_rewake.notification"
                && event.payload["async_hook_id"].as_str() == Some(async_hook_id.as_str())
        })
    })
    .await;
    assert!(events.iter().any(|event| {
        event.kind == "model_tool.async_hook.completed"
            && event.payload["async_hook_id"].as_str() == Some(async_hook_id.as_str())
            && event.payload["returncode"].as_i64() == Some(2)
            && event.payload["rewake_notification_recorded"].as_bool() == Some(true)
    }));
    let notification = events
        .iter()
        .find(|event| {
            event.kind == "model_tool.async_rewake.notification"
                && event.payload["async_hook_id"].as_str() == Some(async_hook_id.as_str())
        })
        .unwrap();
    assert_eq!(
        notification.payload["delivery_status"],
        "recorded_not_delivered"
    );
    assert!(notification.payload["message"]
        .as_str()
        .unwrap()
        .contains("rewake blocking reason"));

    let notification_response = get_json(
        app.clone(),
        "/api/v3/runs/run-model-tool-async-rewake-hook/async-notifications?limit=1",
    )
    .await;
    assert_eq!(notification_response.status(), StatusCode::OK);
    let notification_body = response_json(notification_response).await;
    assert_eq!(
        notification_body["contract"],
        "coder.run_async_notifications.v1"
    );
    assert_eq!(notification_body["source"], "coder-server");
    assert_eq!(notification_body["policy"], "incremental_page");
    assert_eq!(
        notification_body["delivery_status"],
        "durable_read_available"
    );
    assert_eq!(
        notification_body["event_count"].as_u64().unwrap(),
        events.len() as u64
    );
    assert_eq!(notification_body["notification_count"], 1);
    assert_eq!(notification_body["returned_count"], 1);
    assert_eq!(notification_body["truncated"], false);
    assert_eq!(
        notification_body["next_after_sequence"].as_u64(),
        Some(notification.sequence)
    );
    assert_eq!(
        notification_body["notifications"][0]["kind"],
        "model_tool.async_rewake.notification"
    );
    assert_eq!(
        notification_body["notifications"][0]["payload"]["async_hook_id"],
        async_hook_id
    );

    let empty_incremental_response = get_json(
        app.clone(),
        &format!(
            "/api/v3/runs/run-model-tool-async-rewake-hook/async-notifications?after_sequence={}",
            notification.sequence
        ),
    )
    .await;
    assert_eq!(empty_incremental_response.status(), StatusCode::OK);
    let empty_incremental_body = response_json(empty_incremental_response).await;
    assert_eq!(empty_incremental_body["notification_count"], 1);
    assert_eq!(empty_incremental_body["returned_count"], 0);
    assert_eq!(empty_incremental_body["truncated"], false);
    assert_eq!(empty_incremental_body["next_after_sequence"], Value::Null);

    let non_sleep_turn_response = post_json(
        app.clone(),
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-model-tool-async-rewake-hook",
            "harness_id": "native-code-edit",
            "tool_uses": []
        }),
    )
    .await;
    assert_eq!(non_sleep_turn_response.status(), StatusCode::OK);
    let non_sleep_turn_body = response_json(non_sleep_turn_response).await;
    assert_eq!(
        non_sleep_turn_body["attachments"].as_array().unwrap().len(),
        0
    );

    let turn_response = post_json(
        app.clone(),
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-model-tool-async-rewake-hook",
            "harness_id": "native-code-edit",
            "tool_uses": [
                {
                    "id": "toolu-sleep",
                    "name": "sleep",
                    "input": {
                        "duration_ms": 0
                    }
                }
            ]
        }),
    )
    .await;
    assert_eq!(turn_response.status(), StatusCode::OK);
    let turn_body = response_json(turn_response).await;
    let sleep_results = turn_body["results"].as_array().unwrap();
    assert_eq!(sleep_results.len(), 1);
    assert_eq!(sleep_results[0]["tool_use_id"], "toolu-sleep");
    assert_eq!(sleep_results[0]["status"], "completed");
    let attachments = turn_body["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(
        attachments[0]["contract"],
        "coder.model_tool_turn_attachment.v1"
    );
    assert_eq!(attachments[0]["type"], "queued_command");
    assert_eq!(attachments[0]["commandMode"], "task-notification");
    assert_eq!(attachments[0]["source_uuid"], async_hook_id);
    assert!(attachments[0]["prompt"]
        .as_str()
        .unwrap()
        .contains("<system-reminder>"));
    assert!(attachments[0]["prompt"]
        .as_str()
        .unwrap()
        .contains("rewake blocking reason"));
    assert_eq!(attachments[0]["model_content"]["type"], "text");

    let delivered_events = store.read_events(&run_id).unwrap();
    assert!(delivered_events.iter().any(|event| {
        event.kind == "model_tool.async_rewake.delivered"
            && event.payload["notification_sequence"].as_u64() == Some(notification.sequence)
            && event.payload["delivery_status"].as_str() == Some("delivered")
            && event.payload["delivery_channel"].as_str() == Some("model_tool_turn_attachment")
            && event.payload["drain_later_notifications"].as_bool() == Some(true)
    }));

    let repeated_turn_response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-model-tool-async-rewake-hook",
            "harness_id": "native-code-edit",
            "tool_uses": []
        }),
    )
    .await;
    assert_eq!(repeated_turn_response.status(), StatusCode::OK);
    let repeated_turn_body = response_json(repeated_turn_response).await;
    assert_eq!(
        repeated_turn_body["attachments"].as_array().unwrap().len(),
        0
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_async_rewake_notifications_are_agent_scoped() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-async-rewake-agent-scope");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Agent scoped rewake hook\n").unwrap();
    let hook_command = if cfg!(windows) {
        HookTestCommand {
            shell: "powershell".to_owned(),
            command: "Write-Output 'agent scoped rewake reason'; exit 2".to_owned(),
        }
    } else {
        HookTestCommand {
            shell: "sh".to_owned(),
            command: "printf 'agent scoped rewake reason\\n'; exit 2".to_owned(),
        }
    };
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
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
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-scoped-rewake-hook",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "agent_id": "agent-child",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-async-rewake-agent-scope"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    let phases = body["phases"].as_array().unwrap();
    let async_hook_id = phases[0]["hook_results"][0]["async_hook_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let events = wait_for_events(&store, &run_id, |events| {
        events.iter().any(|event| {
            event.kind == "model_tool.async_rewake.notification"
                && event.payload["async_hook_id"].as_str() == Some(async_hook_id.as_str())
        })
    })
    .await;
    let notification = events
        .iter()
        .find(|event| {
            event.kind == "model_tool.async_rewake.notification"
                && event.payload["async_hook_id"].as_str() == Some(async_hook_id.as_str())
        })
        .unwrap();
    assert_eq!(notification.payload["agent_id"], "agent-child");
    assert_eq!(notification.payload["agentId"], "agent-child");

    let main_thread_turn_response = post_json(
        app.clone(),
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-model-tool-async-rewake-agent-scope",
            "harness_id": "native-code-edit",
            "tool_uses": [
                {
                    "id": "toolu-main-sleep",
                    "name": "sleep",
                    "input": {
                        "duration_ms": 0
                    }
                }
            ]
        }),
    )
    .await;
    assert_eq!(main_thread_turn_response.status(), StatusCode::OK);
    let main_thread_turn_body = response_json(main_thread_turn_response).await;
    assert_eq!(
        main_thread_turn_body["attachments"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let wrong_agent_turn_response = post_json(
        app.clone(),
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-model-tool-async-rewake-agent-scope",
            "harness_id": "native-code-edit",
            "agent_id": "agent-other",
            "tool_uses": [
                {
                    "id": "toolu-wrong-agent-sleep",
                    "name": "sleep",
                    "input": {
                        "duration_ms": 0
                    }
                }
            ]
        }),
    )
    .await;
    assert_eq!(wrong_agent_turn_response.status(), StatusCode::OK);
    let wrong_agent_turn_body = response_json(wrong_agent_turn_response).await;
    assert_eq!(
        wrong_agent_turn_body["attachments"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let matching_agent_turn_response = post_json(
        app.clone(),
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-model-tool-async-rewake-agent-scope",
            "harness_id": "native-code-edit",
            "agent_id": "agent-child",
            "tool_uses": [
                {
                    "id": "toolu-matching-agent-sleep",
                    "name": "sleep",
                    "input": {
                        "duration_ms": 0
                    }
                }
            ]
        }),
    )
    .await;
    assert_eq!(matching_agent_turn_response.status(), StatusCode::OK);
    let matching_agent_turn_body = response_json(matching_agent_turn_response).await;
    let attachments = matching_agent_turn_body["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["type"], "queued_command");
    assert_eq!(attachments[0]["agent_id"], "agent-child");
    assert_eq!(attachments[0]["agentId"], "agent-child");
    assert!(attachments[0]["prompt"]
        .as_str()
        .unwrap()
        .contains("agent scoped rewake reason"));

    let delivered_events = store.read_events(&run_id).unwrap();
    assert!(delivered_events.iter().any(|event| {
        event.kind == "model_tool.async_rewake.delivered"
            && event.payload["notification_sequence"].as_u64() == Some(notification.sequence)
            && event.payload["delivery_status"].as_str() == Some("delivered")
            && event.payload["agent_id"].as_str() == Some("agent-child")
            && event.payload["agentId"].as_str() == Some("agent-child")
            && event.payload["drain_agent_id"].as_str() == Some("agent-child")
            && event.payload["drainAgentId"].as_str() == Some("agent-child")
    }));

    let repeated_matching_turn_response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-model-tool-async-rewake-agent-scope",
            "harness_id": "native-code-edit",
            "agent_id": "agent-child",
            "tool_uses": [
                {
                    "id": "toolu-repeated-matching-agent-sleep",
                    "name": "sleep",
                    "input": {
                        "duration_ms": 0
                    }
                }
            ]
        }),
    )
    .await;
    assert_eq!(repeated_matching_turn_response.status(), StatusCode::OK);
    let repeated_matching_turn_body = response_json(repeated_matching_turn_response).await;
    assert_eq!(
        repeated_matching_turn_body["attachments"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}
