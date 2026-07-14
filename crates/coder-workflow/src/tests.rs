use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use coder_config::{PermissionDecision, ProjectConfig};
use coder_core::{FinalReport, ReportStatus, RunId, RunStatus};
use coder_harness::{
    HarnessBackend, HarnessError, HarnessRunEvent, HarnessRunRequest, HarnessRunResult,
};
use coder_store::RepoEvidenceKind;
use serde_json::{json, Value};

use super::*;

#[test]
fn mock_runner_writes_jsonl_events_and_report() {
    let (config, root, store) = fixture();
    let runner = MockWorkflowRunner::new(&config, store.clone());

    let output = runner.run("code", "summarize the repo").unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    assert_eq!(events.first().unwrap().kind, "run.started");
    assert_eq!(events.last().unwrap().kind, "run.completed");
    assert!(output.report_ref.contains("final-report.json"));
    assert_eq!(output.report.status, ReportStatus::Completed);
    assert_eq!(output.report.evidence_refs[0].kind, "event_log");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn mock_runner_can_finish_blocked() {
    let (config, root, store) = fixture();
    let runner = MockWorkflowRunner::new(&config, store.clone());

    let output = runner
        .run_with_options(
            "code",
            "blocked task",
            MockRunOptions {
                outcome: MockRunOutcome::Blocked,
            },
        )
        .unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    assert_eq!(events.last().unwrap().kind, "run.blocked");
    assert_eq!(output.report.status, ReportStatus::Blocked);
    assert_eq!(
        output.report.blockers[0],
        "mock run requested blocked outcome"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn mock_runner_can_finish_failed() {
    let (config, root, store) = fixture();
    let runner = MockWorkflowRunner::new(&config, store.clone());

    let output = runner
        .run_with_options(
            "code",
            "failed task",
            MockRunOptions {
                outcome: MockRunOutcome::Failed,
            },
        )
        .unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    assert_eq!(events.last().unwrap().kind, "run.failed");
    assert_eq!(output.report.status, ReportStatus::Failed);
    assert_eq!(
        output.report.blockers[0],
        "mock run requested failed outcome"
    );
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_native_rust_read_only_review_writes_evidence() {
    let (mut config, root, store) = fixture();
    let repo = temp_root();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("README.md"), "# Native review\n").unwrap();
    fs::write(
        repo.join("src").join("lib.rs"),
        "pub fn answer() -> u8 { 42 }\n",
    )
    .unwrap();
    make_single_node_terminal_workflow(&mut config);
    config.harnesses.get_mut("native-code-edit").unwrap().tools = vec![
        "repo_find_files".to_owned(),
        "repo_read_file_range".to_owned(),
        "git_diff".to_owned(),
    ];
    let runner = WorkflowRunner::new(config, store.clone());
    let mut options = WorkflowRunOptions::new("code", "review README.md for TODO");
    options.repo_root = repo.clone();

    let output = runner.run(options).await.unwrap();
    let events = store.read_events(&output.run_id).unwrap();
    let evidence = store.list_repo_evidence(&output.run_id).unwrap();

    assert_eq!(output.report.status, ReportStatus::Completed);
    assert_eq!(events.first().unwrap().kind, "run.started");
    assert_eq!(events.last().unwrap().kind, "run.completed");
    assert!(events
        .iter()
        .any(|event| event.kind == "backend.native_rust.completed"));
    let started = events
        .iter()
        .find(|event| event.kind == "backend.native_rust.started")
        .unwrap();
    assert_eq!(started.payload["max_tool_use_concurrency"], 10);
    assert_eq!(
        started.payload["tool_execution_mode"],
        "streaming_state_machine"
    );
    assert_eq!(
        started.payload["execution_batches"][0]["concurrency"],
        "concurrent_safe"
    );
    assert_eq!(
        started.payload["execution_batches"][0]["tools"],
        json!(["repo_find_files", "repo_read_file_range", "git_diff"])
    );
    assert!(events.iter().any(|event| {
        event.kind == "native.tool.completed"
            && event.payload["tool"].as_str() == Some("repo_find_files")
    }));
    assert!(events.iter().any(|event| {
        event.kind == "tool.execution.started"
            && event.payload["tool"].as_str() == Some("repo_find_files")
            && event.payload["executor"].as_str() == Some("streaming_state_machine")
    }));
    assert!(events.iter().any(|event| {
        event.kind == "tool.execution.update"
            && event.payload["tool"].as_str() == Some("repo_find_files")
            && event.payload["kind"].as_str() == Some("result")
    }));
    let native_tool_order = events
        .iter()
        .filter(|event| event.kind.starts_with("native.tool."))
        .filter_map(|event| event.payload["tool"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        native_tool_order,
        vec!["repo_find_files", "repo_read_file_range", "git_diff"]
    );
    assert!(evidence
        .iter()
        .any(|item| item.kind == RepoEvidenceKind::RepoFileList));
    assert!(evidence
        .iter()
        .any(|item| item.kind == RepoEvidenceKind::RepoRead));
    assert!(output
        .report
        .evidence_refs
        .iter()
        .any(|item| item.reference.starts_with("repo-evidence://")));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_native_rust_agent_subagent_records_sidechain_and_filters_tools() {
    let (mut config, root, store) = fixture();
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Native subagent\n").unwrap();
    make_single_node_terminal_workflow(&mut config);
    config.harnesses.get_mut("native-code-edit").unwrap().tools =
        vec!["agent_subagent".to_owned(), "repo_find_files".to_owned()];
    let runner = WorkflowRunner::new(config, store.clone());
    let mut options = WorkflowRunOptions::new("code", "delegate repository scan");
    options.repo_root = repo.clone();

    let output = runner.run(options).await.unwrap();
    let events = store.read_events(&output.run_id).unwrap();
    let subagent_event = events
        .iter()
        .find(|event| {
            event.kind == "native.tool.completed"
                && event.payload["tool"].as_str() == Some("agent_subagent")
        })
        .expect("native subagent event");
    let agent_id = subagent_event.payload["agent_id"].as_str().unwrap();

    assert_eq!(output.report.status, ReportStatus::Completed);
    assert_eq!(
        subagent_event.payload["inherited_tools"],
        json!(["repo_find_files"])
    );
    let metadata = store
        .read_subagent_metadata(&output.run_id, agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(metadata.status.as_deref(), Some("completed"));
    assert_eq!(metadata.parent_agent_id, "code");
    assert_eq!(metadata.parent_harness_id, "native-code-edit");

    let records = store
        .read_subagent_transcript_records(&output.run_id, agent_id)
        .unwrap();
    assert!(records
        .iter()
        .any(|record| record.kind == "subagent.started"));
    let child_started = records
        .iter()
        .find(|record| {
            record.kind == "subagent.event"
                && record.payload["kind"].as_str() == Some("backend.native_rust.started")
        })
        .expect("child backend start event");
    assert_eq!(
        child_started.payload["payload"]["tools"],
        json!(["repo_find_files"])
    );
    let child_tools = child_started.payload["payload"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>();
    assert!(!child_tools.contains(&"agent_subagent"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_records_context_compaction_circuit_success() {
    let (mut config, root, store) = fixture();
    make_workflow_native_only(&mut config);
    make_single_node_terminal_workflow(&mut config);
    let model_id = {
        let profile = config.task_profiles.get_mut("code").unwrap();
        profile.runtime.compact_output_reserve_tokens = Some(1_000);
        profile.runtime.max_output_tokens = Some(8_000);
        profile.model.clone()
    };
    let capabilities = &mut config.models.get_mut(&model_id).unwrap().capabilities;
    capabilities.context_window_tokens = Some(32_000);
    capabilities.max_output_tokens = Some(8_000);
    capabilities.auto_compact_token_limit = Some(30_000);
    let runner = WorkflowRunner::new(config, store.clone());
    let mut options = WorkflowRunOptions::new("code", "compact large task context");
    options.task_context = Some(json!({
        "goal": "Inspect and refine the repository",
        "instructions": ["inspect and refine\n".repeat(10_000)],
        "constraints": (0..100).map(|index| format!("constraint-{index}")).collect::<Vec<_>>()
    }));

    let output = runner.run(options).await.unwrap();
    let circuit = store
        .read_compaction_circuit_state(output.run_id.as_str())
        .unwrap()
        .unwrap();
    let events = store.read_events(&output.run_id).unwrap();
    let outcome = events
        .iter()
        .find(|event| event.kind == "context.compaction.outcome")
        .unwrap();

    assert_eq!(circuit.scope_id, output.run_id.as_str());
    assert_eq!(circuit.consecutive_failures, 0);
    assert_eq!(circuit.max_consecutive_failures, 3);
    assert!(!circuit.circuit_breaker_open);
    assert_eq!(outcome.payload["success"], true);
    assert_eq!(outcome.payload["outcome"], "success");
    assert!(matches!(
        outcome.payload["status"].as_str(),
        Some("completed" | "completed_aggressive")
    ));
    assert_eq!(outcome.payload["consecutive_failures"], 0);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn native_react_lifecycle_records_reason_act_observe_steps() {
    let (mut config, root, store) = fixture();
    let repo = temp_root();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("README.md"),
        "# Native review\nTODO: tighten docs\n",
    )
    .unwrap();
    fs::write(
        repo.join("src").join("lib.rs"),
        "pub fn answer() -> u8 { 42 }\n",
    )
    .unwrap();
    make_single_node_terminal_workflow(&mut config);
    config.harnesses.get_mut("native-code-edit").unwrap().tools = vec![
        "repo_find_files".to_owned(),
        "repo_read_file_range".to_owned(),
        "git_diff".to_owned(),
    ];
    let runner = WorkflowRunner::new(config, store.clone());
    let mut options = WorkflowRunOptions::new("code", "review README.md for TODO");
    options.repo_root = repo.clone();

    let output = runner.run(options).await.unwrap();
    let events = store.read_events(&output.run_id).unwrap();
    let reasoning = events
        .iter()
        .filter(|event| event.kind == "executor.reasoning_summary")
        .collect::<Vec<_>>();
    let actions = events
        .iter()
        .filter(|event| event.kind == "executor.action_selected")
        .collect::<Vec<_>>();
    let observations = events
        .iter()
        .filter(|event| event.kind == "observation.recorded")
        .collect::<Vec<_>>();

    assert_eq!(output.report.status, ReportStatus::Completed);
    assert!(reasoning.len() >= 2);
    assert!(actions.len() >= 2);
    assert!(events.iter().any(|event| event.kind == "tool.started"));
    assert!(events.iter().any(|event| event.kind == "tool.completed"));
    assert!(observations.len() >= 2);
    assert!(reasoning[1]
        .payload
        .get("previous_observation")
        .and_then(Value::as_str)
        .unwrap()
        .contains("repo_find_files"));
    assert!(events.iter().any(|event| {
        event.kind == "executor.next_step"
            && event.payload["based_on_observation"]
                .as_str()
                .unwrap_or_default()
                .contains("repo_find_files")
            && event.payload["next_tool"].as_str() == Some("repo_read_file_range")
    }));
    assert!(events
        .iter()
        .any(|event| event.kind == "executor.completed"));
    for event in events.iter().filter(|event| {
        matches!(
            event.kind.as_str(),
            "executor.reasoning_summary"
                | "executor.action_selected"
                | "tool.started"
                | "tool.completed"
                | "observation.recorded"
                | "executor.next_step"
                | "executor.completed"
                | "executor.blocked"
                | "executor.failed"
        )
    }) {
        assert_eq!(event.payload["run_id"], output.run_id.as_str());
        assert_eq!(event.payload["workflow_id"], "code");
        assert_eq!(event.payload["node_id"], "code");
        assert_eq!(event.payload["agent_id"], "code");
        assert_eq!(event.payload["harness_id"], "native-code-edit");
        assert_eq!(event.payload["backend"], "native-rust");
        assert!(event.payload["step"].as_u64().is_some());
    }
    assert!(output
        .report
        .evidence_refs
        .iter()
        .any(|item| item.reference.starts_with("repo-evidence://")));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_native_rust_patch_preview_records_diff_evidence() {
    let (mut config, root, store) = fixture();
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    fs::write(
        repo.join("change.patch"),
        "\
diff --git a/tracked.txt b/tracked.txt
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
    )
    .unwrap();
    make_single_node_terminal_workflow(&mut config);
    config.harnesses.get_mut("native-code-edit").unwrap().tools = vec!["patch_preview".to_owned()];
    let runner = WorkflowRunner::new(config, store.clone());
    let mut options = WorkflowRunOptions::new("code", "preview change.patch");
    options.repo_root = repo.clone();

    let output = runner.run(options).await.unwrap();
    let events = store.read_events(&output.run_id).unwrap();
    let evidence = store.list_repo_evidence(&output.run_id).unwrap();

    assert_eq!(output.report.status, ReportStatus::Completed);
    assert_eq!(output.report.changed_files, vec!["tracked.txt"]);
    assert_eq!(output.report.patch_refs.len(), 1);
    assert!(events.iter().any(|event| {
        event.kind == "native.tool.completed"
            && event.payload["tool"].as_str() == Some("patch_preview")
    }));
    assert!(evidence
        .iter()
        .any(|item| item.kind == RepoEvidenceKind::RepoDiff));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_native_rust_edit_task_blocks_without_side_effects() {
    let (mut config, root, store) = fixture();
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let workflow = config.task_profiles.get_mut("code").unwrap();
    workflow.harness = "native-code-edit".to_owned();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.backend = "native-rust".to_owned();
    harness.tools = vec![
        "repo_find_files".to_owned(),
        "patch_preview".to_owned(),
        "patch_apply".to_owned(),
    ];
    harness.verification.require_evidence = false;
    let runner = WorkflowRunner::new(config, store.clone());
    let mut options = WorkflowRunOptions::new("code", "Create README.md");
    options.repo_root = repo.clone();
    options.task_context = Some(json!({
        "execution_mode": "write",
        "scope": ["README.md"],
        "constraints": ["README.md exists"]
    }));

    let output = runner.run(options).await.unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    assert_eq!(output.report.status, ReportStatus::Blocked);
    assert!(output.report.changed_files.is_empty());
    assert!(output
        .report
        .blockers
        .iter()
        .any(|blocker| { blocker.contains("produced no changed files or patch evidence") }));
    assert!(events
        .iter()
        .any(|event| event.kind == "backend.native_rust.blocked"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_report_uses_patch_event_files_when_backend_report_omits_them() {
    let (mut config, root, store) = fixture();
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    make_single_node_terminal_workflow(&mut config);
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.backend = "native-rust".to_owned();
    harness.verification.require_evidence = false;
    let registry =
        BackendRegistry::native_only().with_native_backend(Arc::new(PatchEventOnlyBackend));
    let runner = WorkflowRunner::with_registry(config, store.clone(), registry);
    let mut options = WorkflowRunOptions::new("code", "apply a code edit");
    options.repo_root = repo.clone();

    let output = runner.run(options).await.unwrap();
    let stored_report = store.read_report(&output.run_id).unwrap().unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    assert_eq!(output.report.status, ReportStatus::Completed);
    assert_eq!(
        output.report.changed_files,
        vec!["src/lib.rs".to_owned(), "tests/smoke.rs".to_owned()]
    );
    assert_eq!(stored_report.changed_files, output.report.changed_files);
    assert!(events.iter().any(|event| event.kind == "patch.applied"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_native_rust_patch_apply_requires_approval() {
    let (mut config, root, store) = fixture();
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    fs::write(
        repo.join("change.patch"),
        "\
diff --git a/tracked.txt b/tracked.txt
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
    )
    .unwrap();
    make_single_node_terminal_workflow(&mut config);
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.tools = vec!["patch_apply".to_owned()];
    harness.permissions.write_files = PermissionDecision::Ask;
    let runner = WorkflowRunner::new(config, store.clone());
    let mut options = WorkflowRunOptions::new("code", "apply patch change.patch");
    options.repo_root = repo.clone();

    let output = runner.run(options).await.unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    assert_eq!(output.report.status, ReportStatus::Blocked);
    assert!(output.report.blockers[0].contains("requires approval"));
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt")).unwrap(),
        "base\n"
    );
    let approval = events
        .iter()
        .find(|event| {
            event.kind == "approval.requested"
                && event.payload["approval_type"].as_str() == Some("patch_apply")
        })
        .unwrap();
    assert_eq!(approval.payload["required_permission"], "write_files");
    assert_eq!(
        approval.payload["permission_decision"]["contract"],
        "coder.tool_permission_decision.v1"
    );
    assert_eq!(
        approval.payload["permission_decision"]["policy_contract"],
        "coder.permission_policy.v1"
    );
    assert_eq!(
        approval.payload["permission_decision"]["decision"]["decisionReason"]["rule"]["source"],
        "policySettings"
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_native_rust_command_run_requires_approval() {
    let (mut config, root, store) = fixture();
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    make_single_node_terminal_workflow(&mut config);
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.tools = vec!["command_run".to_owned()];
    harness.permissions.run_commands = PermissionDecision::Ask;
    let runner = WorkflowRunner::new(config, store.clone());
    let mut options = WorkflowRunOptions::new("code", "run command: definitely-not-run");
    options.repo_root = repo.clone();

    let output = runner.run(options).await.unwrap();
    let events = store.read_events(&output.run_id).unwrap();
    let evidence = store.list_repo_evidence(&output.run_id).unwrap();

    assert_eq!(output.report.status, ReportStatus::Blocked);
    let approval = events
        .iter()
        .find(|event| {
            event.kind == "approval.requested"
                && event.payload["approval_type"].as_str() == Some("command")
        })
        .unwrap();
    assert_eq!(approval.payload["required_permission"], "run_commands");
    assert_eq!(
        approval.payload["permission_decision"]["contract"],
        "coder.tool_permission_decision.v1"
    );
    assert_eq!(
        approval.payload["permission_decision"]["decision"]["behavior"],
        "ask"
    );
    assert_eq!(
        approval.payload["permission_decision"]["decision"]["decisionReason"]["rule"]["source"],
        "policySettings"
    );
    assert!(events.iter().any(|event| event.kind == "executor.blocked"));
    assert!(events.iter().any(|event| {
        event.kind == "tool.completed"
            && event.payload["tool_name"].as_str() == Some("command_run")
            && event.payload["status"].as_str() == Some("blocked")
    }));
    assert!(evidence
        .iter()
        .any(|item| item.kind == RepoEvidenceKind::RepoTest));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_native_mock_blocked() {
    let (mut config, root, store) = fixture();
    make_single_node_terminal_workflow(&mut config);
    make_workflow_native_only(&mut config);
    let registry = BackendRegistry::native_only()
        .with_native_backend(Arc::new(NativeMockBackend::new(NativeMockOutcome::Blocked)));
    let runner = WorkflowRunner::with_registry(config, store.clone(), registry);

    let output = runner
        .run(WorkflowRunOptions::new("code", "blocked task"))
        .await
        .unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    assert_eq!(output.report.status, ReportStatus::Blocked);
    assert!(output.report.blockers[0].contains("blocked outcome"));
    assert!(events.iter().any(|event| event.kind == "node.blocked"));
    assert_eq!(events.last().unwrap().kind, "run.blocked");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_completed_terminal_stop() {
    let (mut config, root, store) = fixture();
    make_single_node_terminal_workflow(&mut config);
    let runner = workflow_runner_with_script(config, store.clone(), ["completed"]);

    let output = runner
        .run(WorkflowRunOptions::new("code", "terminal completed"))
        .await
        .unwrap();
    let events = store.read_events(&output.run_id).unwrap();
    let config_snapshot = store
        .read_run_config_snapshot_json(&output.run_id)
        .unwrap()
        .unwrap();

    assert_eq!(output.report.status, ReportStatus::Completed);
    assert_eq!(events.last().unwrap().kind, "run.completed");
    assert!(config_snapshot["task_profiles"]["code"].is_object());
    assert!(config_snapshot["harnesses"]["native-code-edit"]["permissions"].is_object());
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_blocks_required_evidence_when_backend_returns_none() {
    let (mut config, root, store) = fixture();
    make_required_evidence_executor_workflow(&mut config);
    let registry = BackendRegistry::native_only()
        .with_native_backend(Arc::new(EvidencePolicyBackend::new(false)));
    let runner = WorkflowRunner::with_registry(config, store.clone(), registry);

    let output = runner
        .run(WorkflowRunOptions::new("code", "complete without proof"))
        .await
        .unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    assert_eq!(output.report.status, ReportStatus::Blocked);
    assert!(output
        .report
        .blockers
        .iter()
        .any(|blocker| blocker.contains("requires evidence refs")));
    assert!(events
        .iter()
        .any(|event| event.kind == "verification.started"));
    assert!(events.iter().any(|event| {
        event.kind == "verification.failed"
            && event.payload["status"].as_str() == Some("failed")
            && event.payload["evidence"]["total_refs"].as_u64() == Some(0)
    }));
    assert!(events.iter().any(|event| event.kind == "node.blocked"));
    assert_eq!(events.last().unwrap().kind, "run.blocked");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_allows_required_evidence_when_backend_returns_refs() {
    let (mut config, root, store) = fixture();
    make_required_evidence_executor_workflow(&mut config);
    let registry = BackendRegistry::native_only()
        .with_native_backend(Arc::new(EvidencePolicyBackend::new(true)));
    let runner = WorkflowRunner::with_registry(config, store.clone(), registry);

    let output = runner
        .run(WorkflowRunOptions::new("code", "complete with proof"))
        .await
        .unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    assert_eq!(output.report.status, ReportStatus::Completed);
    assert!(output
        .report
        .evidence_refs
        .iter()
        .any(|reference| reference.kind == "evidence_policy"));
    assert!(events.iter().any(|event| {
        event.kind == "verification.completed"
            && event.payload["status"].as_str() == Some("completed")
            && event.payload["evidence"]["total_refs"]
                .as_u64()
                .unwrap_or_default()
                >= 1
    }));
    assert!(events.iter().any(|event| event.kind == "node.completed"));
    assert_eq!(events.last().unwrap().kind, "run.completed");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_blocked_terminal_stop() {
    let (mut config, root, store) = fixture();
    make_single_node_terminal_workflow(&mut config);
    let runner = workflow_runner_with_script(config, store.clone(), ["blocked"]);

    let output = runner
        .run(WorkflowRunOptions::new("code", "terminal blocked"))
        .await
        .unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    assert_eq!(output.report.status, ReportStatus::Blocked);
    assert!(events.iter().any(|event| event.kind == "node.blocked"));
    assert_eq!(events.last().unwrap().kind, "run.blocked");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_failed_terminal_stop() {
    let (mut config, root, store) = fixture();
    make_single_node_terminal_workflow(&mut config);
    let runner = workflow_runner_with_script(config, store.clone(), ["failed"]);

    let output = runner
        .run(WorkflowRunOptions::new("code", "terminal failed"))
        .await
        .unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    assert_eq!(output.report.status, ReportStatus::Failed);
    assert!(events.iter().any(|event| event.kind == "node.failed"));
    assert_eq!(events.last().unwrap().kind, "run.failed");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_cancelled_terminal_stop() {
    let (mut config, root, store) = fixture();
    make_single_node_terminal_workflow(&mut config);
    let runner = workflow_runner_with_script(config, store.clone(), ["cancelled"]);

    let output = runner
        .run(WorkflowRunOptions::new("code", "terminal cancelled"))
        .await
        .unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    assert_eq!(output.report.status, ReportStatus::Cancelled);
    assert!(events.iter().any(|event| event.kind == "node.cancelled"));
    assert_eq!(events.last().unwrap().kind, "run.cancelled");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_control_cancels_an_in_flight_backend_future() {
    let (mut config, root, store) = fixture();
    make_single_node_terminal_workflow(&mut config);
    make_workflow_native_only(&mut config);
    let registry = BackendRegistry::native_only().with_native_backend(Arc::new(DelayedBackend));
    let runner = WorkflowRunner::with_registry(config, store.clone(), registry);
    let run_id = RunId::from_string("run-live-cancel");
    let (sender, receiver) = tokio::sync::watch::channel(WorkflowRunControl::Running);
    let mut options = WorkflowRunOptions::new("code", "long running task");
    options.run_id = Some(run_id.clone());
    options.control = Some(receiver);

    let run_task = tokio::spawn(async move { runner.run(options).await.unwrap() });
    for _ in 0..50 {
        if store.read_metadata(&run_id).unwrap().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    sender.send(WorkflowRunControl::Cancelled).unwrap();
    let output = tokio::time::timeout(std::time::Duration::from_secs(1), run_task)
        .await
        .expect("cancellation should stop the backend future")
        .unwrap();

    assert_eq!(output.report.status, ReportStatus::Cancelled);
    assert_eq!(output.run_id, run_id);
    let events = store.read_events(&output.run_id).unwrap();
    assert_eq!(events.last().unwrap().kind, "run.cancelled");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_reports_unknown_backend() {
    let (mut config, root, store) = fixture();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .backend = "mystery-backend".to_owned();
    let runner = WorkflowRunner::new(config, store);
    let mut options = WorkflowRunOptions::new("code", "unknown backend");
    options.repo_root = root.clone();

    let error = runner.run(options).await.unwrap_err();

    assert!(matches!(error, WorkflowError::InvalidConfig(_)));
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_event_sequence_is_monotonic() {
    let (mut config, root, store) = fixture();
    make_workflow_native_only(&mut config);
    let runner = WorkflowRunner::new(config, store.clone());

    let output = runner
        .run(WorkflowRunOptions::new("code", "sequence task"))
        .await
        .unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.sequence, index as u64 + 1);
    }
    let first_node = events
        .iter()
        .find(|event| event.kind == "agent.started")
        .unwrap();
    assert_eq!(first_node.payload["runtime"]["context_window"], 128_000);
    assert_eq!(
        first_node.payload["runtime"]["effective_context_window"],
        121_600
    );
    assert_eq!(first_node.payload["runtime"]["compaction_failure_limit"], 3);
    assert_eq!(
        first_node.payload["runtime"]["context_budget"]["autocompact_threshold"],
        115_200
    );
    let started = events
        .iter()
        .find(|event| event.kind == "run.started")
        .unwrap();
    assert_eq!(started.payload["token_budget"], 144_000);
    assert_eq!(
        started.payload["cost_policy"]["budget_source"],
        "model_capability_default"
    );
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_final_report_has_event_log_evidence() {
    let (mut config, root, store) = fixture();
    make_workflow_native_only(&mut config);
    let runner = WorkflowRunner::new(config, store);

    let output = runner
        .run(WorkflowRunOptions::new("code", "evidence task"))
        .await
        .unwrap();

    assert!(output
        .report
        .evidence_refs
        .iter()
        .any(|reference| reference.kind == "event_log"));
    assert!(output.report.summary.contains("Requested: evidence task"));
    assert!(output.report.summary.contains("Done:"));
    assert!(output.report.summary.contains("Verification:"));
    assert!(output.report.summary.contains("Evidence:"));
    assert!(output.report.summary.contains("Remaining risks:"));
    assert!(output.report.summary.contains("Next steps:"));
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn workflow_runner_replays_terminal_status_from_events() {
    let (mut config, root, store) = fixture();
    make_single_node_terminal_workflow(&mut config);
    let runner = workflow_runner_with_script(config, store.clone(), ["failed"]);

    let output = runner
        .run(WorkflowRunOptions::new("code", "replay task"))
        .await
        .unwrap();
    let events = store.read_events(&output.run_id).unwrap();

    assert_eq!(replay_run_status(&events), Some(RunStatus::Failed));
    let metadata = store.read_metadata(&output.run_id).unwrap().unwrap();
    assert_eq!(replay_run_status(&events), Some(metadata.status));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn subagent_inheritance_filters_main_thread_sensitive_tools() {
    let tools = vec![
        "memory_read".to_owned(),
        "knowledge_retrieve".to_owned(),
        "agent_subagent".to_owned(),
        "terminal".to_owned(),
        "file_editor".to_owned(),
    ];

    let inheritable = subagent_context::subagent_inheritable_tools(&tools);

    assert!(!inheritable.contains(&"memory_read".to_owned()));
    assert!(!inheritable.contains(&"knowledge_retrieve".to_owned()));
    assert!(!inheritable.contains(&"agent_subagent".to_owned()));
    assert!(inheritable.contains(&"terminal".to_owned()));
    assert!(inheritable.contains(&"file_editor".to_owned()));
}

#[tokio::test]
async fn subagent_runtime_runs_backend_and_records_sidechain() {
    let (config, root, store) = fixture();
    let run_id = RunId::from_string("run-subagent-runtime");
    let harness = config.harnesses.get("native-code-edit").unwrap();
    let backend: Arc<dyn HarnessBackend> = Arc::new(ScriptedBackend::new(["completed"]));
    let runtime = SubagentRuntime::new(store.clone());
    let backend_context = json!({"parent": "context"});

    let output = runtime
        .run(SubagentRunInput {
            backend,
            run_id: &run_id,
            workflow_id: "code",
            node_id: "executor",
            parent_agent_id: "executor",
            parent_harness_id: "native-code-edit",
            harness,
            repo_root: ".",
            task: "review helper task",
            backend_context: &backend_context,
            agent_id: Some("agent-1".to_owned()),
            subagent_name: Some("reviewer"),
            is_built_in: false,
            invoking_request_id: Some("request-1"),
            invocation_kind: SubagentInvocationKind::Spawn,
            parent_query_depth: 1,
            parent_sequence: Some(7),
        })
        .await
        .unwrap();

    assert_eq!(output.agent_id, "agent-1");
    assert_eq!(output.result.status, "completed");
    assert!(output
        .transcript_ref
        .ends_with("subagents/agent-agent-1.jsonl"));
    assert!(output
        .metadata_ref
        .ends_with("subagents/agent-agent-1.meta.json"));

    let metadata = store
        .read_subagent_metadata(&run_id, "agent-1")
        .unwrap()
        .unwrap();
    assert_eq!(metadata.parent_agent_id, "executor");
    assert_eq!(metadata.parent_harness_id, "native-code-edit");
    assert_eq!(metadata.invocation_kind, "spawn");
    assert_eq!(metadata.status.as_deref(), Some("completed"));
    assert_eq!(
        metadata.terminal_record_kind.as_deref(),
        Some("subagent.completed")
    );
    assert_eq!(metadata.error, None);
    assert_eq!(metadata.description.as_deref(), Some("reviewer"));
    assert_eq!(
        metadata.transcript_ref.as_deref(),
        Some(output.transcript_ref.as_str())
    );
    let records = store
        .read_subagent_transcript_records(&run_id, "agent-1")
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
    assert_eq!(records[0].parent_sequence, Some(7));
    assert_eq!(records[0].payload["context"]["query_tracking"]["depth"], 2);
    assert_eq!(records[1].payload["task"], "review helper task");
    assert_eq!(records[2].payload["kind"], "backend.scripted.completed");
    assert_eq!(
        metadata.last_sequence,
        Some(records.last().unwrap().sequence)
    );
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn subagent_runtime_records_failed_sidechain_when_backend_errors() {
    struct ErrorBackend;

    #[async_trait]
    impl HarnessBackend for ErrorBackend {
        async fn run(&self, _request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
            Err(HarnessError::Failed("child backend crashed".to_owned()))
        }
    }

    let (config, root, store) = fixture();
    let run_id = RunId::from_string("run-subagent-runtime-error");
    let harness = config.harnesses.get("native-code-edit").unwrap();
    let runtime = SubagentRuntime::new(store.clone());
    let backend_context = json!({});

    let error = match runtime
        .run(SubagentRunInput {
            backend: Arc::new(ErrorBackend),
            run_id: &run_id,
            workflow_id: "code",
            node_id: "executor",
            parent_agent_id: "executor",
            parent_harness_id: "native-code-edit",
            harness,
            repo_root: ".",
            task: "review helper task",
            backend_context: &backend_context,
            agent_id: Some("agent-error".to_owned()),
            subagent_name: Some("reviewer"),
            is_built_in: false,
            invoking_request_id: Some("request-1"),
            invocation_kind: SubagentInvocationKind::Spawn,
            parent_query_depth: 1,
            parent_sequence: None,
        })
        .await
    {
        Ok(_) => panic!("subagent runtime should fail when child backend errors"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("child backend crashed"));
    let metadata = store
        .read_subagent_metadata(&run_id, "agent-error")
        .unwrap()
        .unwrap();
    assert_eq!(metadata.status.as_deref(), Some("failed"));
    assert_eq!(
        metadata.terminal_record_kind.as_deref(),
        Some("subagent.failed")
    );
    assert!(metadata
        .error
        .as_deref()
        .unwrap()
        .contains("child backend crashed"));
    let records = store
        .read_subagent_transcript_records(&run_id, "agent-error")
        .unwrap();
    assert_eq!(records.last().unwrap().kind, "subagent.failed");
    assert_eq!(
        metadata.last_sequence,
        Some(records.last().unwrap().sequence)
    );
    assert_eq!(records.last().unwrap().payload["status"], "failed");
    let _ = fs::remove_dir_all(root);
}

fn workflow_runner_with_script<I, S>(
    mut config: ProjectConfig,
    store: RunStore,
    statuses: I,
) -> WorkflowRunner
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    make_workflow_native_only(&mut config);
    let registry = BackendRegistry::native_only()
        .with_native_backend(Arc::new(ScriptedBackend::new(statuses)));
    WorkflowRunner::with_registry(config, store, registry)
}

struct ScriptedBackend {
    statuses: Mutex<VecDeque<String>>,
}

struct PatchEventOnlyBackend;

struct DelayedBackend;

#[async_trait]
impl HarnessBackend for DelayedBackend {
    async fn run(&self, _request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        Ok(HarnessRunResult {
            status: "completed".to_owned(),
            report: Some(FinalReport::completed("Delayed backend completed.")),
            events: Vec::new(),
        })
    }
}

#[async_trait]
impl HarnessBackend for PatchEventOnlyBackend {
    async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        let repo_path = PathBuf::from(&request.repo_root);
        let absolute_lib = repo_path.join("src").join("lib.rs").display().to_string();
        let repo_root = request.repo_root.clone();
        let outside_file = repo_path
            .parent()
            .map(|parent| parent.join("outside.txt"))
            .unwrap_or_else(|| PathBuf::from("outside.txt"))
            .display()
            .to_string();
        Ok(HarnessRunResult {
            status: "completed".to_owned(),
            report: Some(FinalReport::completed(
                "Patch event backend completed without report-level changed_files.",
            )),
            events: vec![HarnessRunEvent::new(
                "patch.applied",
                json!({
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "status": "applied",
                    "files": [
                        {"path": absolute_lib, "status": "modified"},
                        {"path": repo_root, "status": "modified"},
                        {"path": outside_file, "status": "modified"},
                        "tests/smoke.rs"
                    ]
                }),
            )],
        })
    }
}

impl ScriptedBackend {
    fn new<I, S>(statuses: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            statuses: Mutex::new(statuses.into_iter().map(Into::into).collect()),
        }
    }
}

#[async_trait]
impl HarnessBackend for ScriptedBackend {
    async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        let status_token = self
            .statuses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| "completed".to_owned());
        let (status, include_evidence, include_review, include_change) = match status_token.as_str()
        {
            "blocked_with_evidence" => ("blocked", true, false, false),
            "completed_with_review" => ("completed", true, true, true),
            "completed_review_only" => ("completed", false, true, false),
            _ => (status_token.as_str(), false, false, false),
        };
        let mut report = match status {
            "blocked" => FinalReport::blocked("Scripted backend blocked.", "scripted blocked"),
            "failed" => FinalReport::failed("Scripted backend failed.", "scripted failed"),
            "completed" if status_token == "completed_review_only" => {
                FinalReport::completed("Scripted review completed without durable evidence refs.")
            }
            "continue" => {
                FinalReport::completed("Scripted backend requested another executor pass.")
                    .with_check(format!("scripted continue feedback: {}", request.task))
            }
            "cancelled" => {
                FinalReport::with_status(ReportStatus::Cancelled, "Scripted backend cancelled.")
            }
            _ => FinalReport::completed("Scripted backend completed.").with_evidence(
                "scripted_backend",
                format!(
                    "scripted://runs/{}/{}",
                    request.run_id.as_str(),
                    request.node_id
                ),
            ),
        };
        if include_evidence {
            report = report.with_evidence(
                "scripted_backend",
                format!(
                    "scripted://runs/{}/{}",
                    request.run_id.as_str(),
                    request.node_id
                ),
            );
        }
        if include_review {
            let round = request
                .backend_context
                .pointer("/coder/workflow_loop/round")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if include_change {
                report.changed_files.push("src/game.rs".to_owned());
            }
            report.checks.push(format!(
                "task-specific review round {round}: {}",
                "x".repeat(1_200)
            ));
        }
        Ok(HarnessRunResult {
            status: status.to_owned(),
            report: Some(report),
            events: vec![HarnessRunEvent::new(
                format!("backend.scripted.{status}"),
                json!({
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "status": status,
                    "scripted_status": status_token
                }),
            )],
        })
    }
}

struct EvidencePolicyBackend {
    include_evidence: bool,
}

impl EvidencePolicyBackend {
    fn new(include_evidence: bool) -> Self {
        Self { include_evidence }
    }
}

#[async_trait]
impl HarnessBackend for EvidencePolicyBackend {
    async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        let mut report = FinalReport::completed("Evidence policy backend completed.");
        let mut event = HarnessRunEvent::new(
            "backend.evidence_policy.completed",
            json!({
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "status": "completed"
            }),
        );
        if self.include_evidence {
            let reference = format!(
                "evidence-policy://runs/{}/{}",
                request.run_id.as_str(),
                request.node_id
            );
            report = report.with_evidence("evidence_policy", reference.clone());
            event = event.with_ref("evidence_policy", reference);
        }
        Ok(HarnessRunResult {
            status: "completed".to_owned(),
            report: Some(report),
            events: vec![event],
        })
    }
}

fn fixture() -> (ProjectConfig, PathBuf, RunStore) {
    let config: ProjectConfig =
        serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
    let root = temp_root();
    let store = RunStore::new(&root);
    (config, root, store)
}

fn temp_root() -> PathBuf {
    static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    test_tmp_root().join(format!("coder-workflow-{}-{}", std::process::id(), id))
}

fn test_tmp_root() -> PathBuf {
    std::env::var_os("CODER_TEST_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn make_workflow_native_only(config: &mut ProjectConfig) {
    for harness in config.harnesses.values_mut() {
        harness.backend = "native-rust".to_owned();
        harness.tools.clear();
    }
}

fn make_single_node_terminal_workflow(config: &mut ProjectConfig) {
    let workflow = config.task_profiles.get_mut("code").unwrap();
    workflow.harness = "native-code-edit".to_owned();
}

fn make_required_evidence_executor_workflow(config: &mut ProjectConfig) {
    let workflow = config.task_profiles.get_mut("code").unwrap();
    workflow.harness = "native-code-edit".to_owned();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.backend = "native-rust".to_owned();
    harness.tools.clear();
    harness.verification.require_evidence = true;
}
