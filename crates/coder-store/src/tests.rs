use coder_events::CoderEvent;
use serde_json::json;

use super::*;

#[test]
fn event_log_roundtrips() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_test");
    let event = CoderEvent::new(
        run_id.clone(),
        1,
        "run.started",
        json!({"workflow_id": "wf"}),
    );

    store.append_event(&run_id, &event).unwrap();
    let events = store.read_events(&run_id).unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "run.started");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn event_log_page_supports_after_sequence_and_tail_without_full_vec() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_page");
    for sequence in 1..=5 {
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    sequence,
                    format!("event.{sequence}"),
                    json!({"sequence": sequence}),
                ),
            )
            .unwrap();
    }

    let page = store
        .read_events_page(
            &run_id,
            DurableJsonlPageOptions::with_after_sequence(Some(2), 2).unwrap(),
        )
        .unwrap();
    assert_eq!(page.total_records, 5);
    assert_eq!(page.matching_records, 3);
    assert_eq!(page.returned_records, 2);
    assert_eq!(page.next_after_sequence, Some(4));
    assert_eq!(
        page.records
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(page.truncated);

    let tail = store
        .read_events_page(&run_id, DurableJsonlPageOptions::tail(2).unwrap())
        .unwrap();
    assert_eq!(
        tail.records
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![4, 5]
    );
    assert_eq!(tail.next_after_sequence, Some(5));
    assert!(tail.truncated);
    assert_eq!(store.event_count(&run_id).unwrap(), 5);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn durable_jsonl_page_limit_is_bounded() {
    assert!(matches!(
        DurableJsonlPageOptions::new(0).unwrap_err(),
        StoreError::DurableJsonlPageLimitOutOfRange { .. }
    ));
    assert!(matches!(
        DurableJsonlPageOptions::tail(MAX_DURABLE_JSONL_PAGE_LIMIT + 1).unwrap_err(),
        StoreError::DurableJsonlPageLimitOutOfRange { .. }
    ));
}

#[test]
fn append_event_redacts_payload_before_jsonl_write() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_test");
    let event = CoderEvent {
        event_id: "evt_manual".to_owned(),
        run_id: run_id.clone(),
        sequence: 1,
        timestamp: OffsetDateTime::now_utc(),
        kind: "run.started".to_owned(),
        payload: json!({"task": "Use sk-live-1234567890"}),
        refs: Vec::new(),
    };

    store.append_event(&run_id, &event).unwrap();

    let events = store.read_events(&run_id).unwrap();
    assert_eq!(events[0].payload["task"], "[REDACTED]");
    let text = fs::read_to_string(root.join("runs").join("run_test").join("events.jsonl")).unwrap();
    assert!(!text.contains("sk-live-1234567890"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn local_layout_creates_required_directories() {
    let root = temp_root();
    let store = RunStore::new(&root);

    let layout = store.ensure_local_layout().unwrap();

    assert_eq!(layout.root, root);
    for dir in LOCAL_STORE_DIRS {
        assert!(root.join(dir).is_dir(), "{dir} should exist");
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn session_records_append_jsonl_and_reject_secret_like_payloads() {
    let root = temp_root();
    let store = RunStore::new(&root);

    store
        .append_session_record(
            "session_1",
            1,
            "session.created",
            json!({"workflow_id": "planner-led", "mode": "discuss"}),
        )
        .unwrap();
    store
        .append_session_record(
            "session_1",
            2,
            "session.turn.completed",
            json!({"turn_count": 2, "ready": false}),
        )
        .unwrap();

    let records = store.read_session_records("session_1").unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].sequence, 1);
    assert_eq!(records[1].kind, "session.turn.completed");
    let text = fs::read_to_string(root.join("sessions").join("session_1.jsonl")).unwrap();
    assert_eq!(text.lines().count(), 2);

    let error = store
        .append_session_record(
            "session_1",
            3,
            "session.turn.completed",
            json!({"api_key": "redacted"}),
        )
        .unwrap_err();
    assert!(matches!(error, StoreError::SessionRecordSecretLikeText));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn session_record_next_sequence_uses_persistent_sidecar() {
    let root = temp_root();
    let store = RunStore::new(&root);

    let first = store
        .append_session_record_next(
            "session_1",
            "session.created",
            json!({"workflow_id": "planner-led"}),
        )
        .unwrap();
    let second = store
        .append_session_record_next(
            "session_1",
            "session.turn.completed",
            json!({"turn_count": 2}),
        )
        .unwrap();

    assert_eq!(first, 1);
    assert_eq!(second, 2);
    let records = store.read_session_records("session_1").unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].sequence, 1);
    assert_eq!(records[1].sequence, 2);
    let sequence_text = fs::read_to_string(root.join("sessions").join("session_1.seq")).unwrap();
    assert_eq!(sequence_text.trim(), "2");
    let error = store
        .append_session_record_next(
            "session_1",
            "session.turn.completed",
            json!({"api_key": "redacted"}),
        )
        .unwrap_err();
    assert!(matches!(error, StoreError::SessionRecordSecretLikeText));
    let sequence_text = fs::read_to_string(root.join("sessions").join("session_1.seq")).unwrap();
    assert_eq!(sequence_text.trim(), "2");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn session_records_page_reads_incremental_records() {
    let root = temp_root();
    let store = RunStore::new(&root);
    for index in 1..=4 {
        store
            .append_session_record(
                "session_page",
                index,
                "session.turn.completed",
                json!({"index": index}),
            )
            .unwrap();
    }

    let page = store
        .read_session_records_page(
            "session_page",
            DurableJsonlPageOptions::with_after_sequence(Some(1), 2).unwrap(),
        )
        .unwrap();

    assert_eq!(page.total_records, 4);
    assert_eq!(page.matching_records, 3);
    assert_eq!(page.returned_records, 2);
    assert_eq!(page.next_after_sequence, Some(3));
    assert_eq!(
        page.records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn subagent_transcript_and_metadata_use_run_scoped_sidecars() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_subagents");

    let first = store
        .append_subagent_transcript_record_next(
            &run_id,
            "agent-1",
            None,
            "subagent.started",
            json!({"prompt": "review files"}),
        )
        .unwrap();
    let second = store
        .append_subagent_transcript_record_next(
            &run_id,
            "agent-1",
            Some(first),
            "subagent.message",
            json!({"summary": "looked at src/lib.rs"}),
        )
        .unwrap();
    let metadata = SubagentMetadata {
        agent_type: "code-reviewer".to_owned(),
        parent_agent_id: "executor".to_owned(),
        parent_harness_id: "native-code-edit".to_owned(),
        invocation_kind: "spawn".to_owned(),
        status: Some("completed".to_owned()),
        terminal_record_kind: Some("subagent.completed".to_owned()),
        last_sequence: Some(second),
        error: None,
        description: Some("review implementation".to_owned()),
        worktree_path: None,
        transcript_ref: Some(
            "subagent://runs/run_subagents/subagents/agent-agent-1.jsonl".to_owned(),
        ),
    };
    let metadata_ref = store
        .write_subagent_metadata(&run_id, "agent-1", &metadata)
        .unwrap();

    assert_eq!(first, 1);
    assert_eq!(second, 2);
    assert_eq!(
        metadata_ref,
        "subagent://runs/run_subagents/subagents/agent-agent-1.meta.json"
    );
    let records = store
        .read_subagent_transcript_records(&run_id, "agent-1")
        .unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].sequence, 1);
    assert_eq!(records[1].parent_sequence, Some(1));
    assert_eq!(records[1].payload["summary"], "looked at src/lib.rs");
    let loaded = store
        .read_subagent_metadata(&run_id, "agent-1")
        .unwrap()
        .unwrap();
    assert_eq!(loaded, metadata);
    let sequence_text = fs::read_to_string(
        root.join("runs")
            .join("run_subagents")
            .join("subagents")
            .join("agent-agent-1.seq"),
    )
    .unwrap();
    assert_eq!(sequence_text.trim(), "2");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn subagent_background_task_record_roundtrips() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let record = SubagentBackgroundTaskRecord {
        task_id: "task-123".to_owned(),
        run_id: "run_subagents".to_owned(),
        agent_id: "agent-1".to_owned(),
        status: "running".to_owned(),
        created_at_ms: 1000,
        updated_at_ms: 1000,
        metadata_ref: "subagent://runs/run_subagents/subagents/agent-agent-1.meta.json".to_owned(),
        transcript_ref: "subagent://runs/run_subagents/subagents/agent-agent-1.jsonl".to_owned(),
        report: None,
        event_count: 0,
        events_truncated: false,
        error: None,
    };

    let task_ref = store
        .write_subagent_background_task_record(&record)
        .unwrap();
    assert_eq!(task_ref, "background-task://subagents/task-123.json");
    let loaded = store
        .read_subagent_background_task_record("task-123")
        .unwrap()
        .unwrap();
    assert_eq!(loaded.task_id, record.task_id);
    assert_eq!(loaded.run_id, record.run_id);
    assert_eq!(loaded.agent_id, record.agent_id);
    assert_eq!(loaded.status, "running");
    assert_eq!(loaded.event_count, 0);
    assert!(root
        .join("background-tasks")
        .join("subagents")
        .join("task-123.json")
        .exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn command_background_task_record_and_output_tail_roundtrips() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let record = CommandBackgroundTaskRecord {
        task_id: "cmd-123".to_owned(),
        run_id: Some("run_cmd".to_owned()),
        repo_root: ".".to_owned(),
        cwd: ".".to_owned(),
        argv: vec!["echo".to_owned(), "hello".to_owned()],
        command: "echo hello".to_owned(),
        approval_key: "approval-1".to_owned(),
        policy: json!({"allowed": true}),
        status: "running".to_owned(),
        created_at_ms: 1000,
        updated_at_ms: 1000,
        output_ref: "background-task-output://commands/cmd-123.output".to_owned(),
        output_bytes: 0,
        output_start_offset: 0,
        output_total_bytes: 0,
        output_truncated: false,
        max_output_bytes: 16,
        result: None,
        evidence_ref: None,
        error: None,
    };

    let task_ref = store.write_command_background_task_record(&record).unwrap();
    let output_ref = store
        .write_command_background_output_tail("cmd-123", b"0123456789abcdef")
        .unwrap();

    assert_eq!(task_ref, "background-task://commands/cmd-123.json");
    assert_eq!(
        output_ref,
        "background-task-output://commands/cmd-123.output"
    );
    let loaded = store
        .read_command_background_task_record("cmd-123")
        .unwrap()
        .unwrap();
    assert_eq!(loaded.task_id, record.task_id);
    assert_eq!(loaded.command, "echo hello");
    let full = store
        .read_command_background_output_tail("cmd-123", 64)
        .unwrap();
    assert_eq!(full.output, "0123456789abcdef");
    assert_eq!(full.bytes, 16);
    assert!(!full.truncated);
    let tail = store
        .read_command_background_output_tail("cmd-123", 6)
        .unwrap();
    assert_eq!(tail.output, "abcdef");
    assert_eq!(tail.bytes, 16);
    assert!(tail.truncated);
    assert!(root
        .join("background-tasks")
        .join("commands")
        .join("cmd-123.json")
        .exists());
    assert!(root
        .join("background-tasks")
        .join("commands")
        .join("cmd-123.output")
        .exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn subagent_transcript_page_reads_tail_records() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_subagent_page");
    for index in 1..=4 {
        store
            .append_subagent_transcript_record_next(
                &run_id,
                "agent-1",
                None,
                "subagent.message",
                json!({"index": index}),
            )
            .unwrap();
    }

    let page = store
        .read_subagent_transcript_records_page(
            &run_id,
            "agent-1",
            DurableJsonlPageOptions::tail(2).unwrap(),
        )
        .unwrap();

    assert_eq!(page.total_records, 4);
    assert_eq!(page.matching_records, 4);
    assert_eq!(
        page.records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(page.truncated);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn run_content_replacement_records_roundtrip_and_reject_secrets() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_replacements");

    let first = store
        .append_run_content_replacement_record_next(
            &run_id,
            vec![ContentReplacementRecord {
                kind: "tool-result".to_owned(),
                tool_use_id: "toolu-1".to_owned(),
                replacement: "<persisted-output>preview</persisted-output>".to_owned(),
            }],
        )
        .unwrap();
    let second = store
        .append_run_content_replacement_record_next(
            &run_id,
            vec![ContentReplacementRecord {
                kind: "tool-result".to_owned(),
                tool_use_id: "toolu-2".to_owned(),
                replacement: "<persisted-output>preview 2</persisted-output>".to_owned(),
            }],
        )
        .unwrap();

    assert_eq!(first, 1);
    assert_eq!(second, 2);
    let records = store.read_run_content_replacement_records(&run_id).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].kind, "content-replacement");
    assert_eq!(records[0].replacements[0].tool_use_id, "toolu-1");
    let page = store
        .read_run_content_replacement_records_page(
            &run_id,
            DurableJsonlPageOptions::tail(1).unwrap(),
        )
        .unwrap();
    assert_eq!(page.records[0].sequence, 2);
    let sequence_text = fs::read_to_string(
        root.join("runs")
            .join("run_replacements")
            .join("content-replacements.seq"),
    )
    .unwrap();
    assert_eq!(sequence_text.trim(), "2");

    let error = store
        .append_run_content_replacement_record_next(
            &run_id,
            vec![ContentReplacementRecord {
                kind: "tool-result".to_owned(),
                tool_use_id: "toolu-secret".to_owned(),
                replacement: "api_key should not persist".to_owned(),
            }],
        )
        .unwrap_err();
    assert!(matches!(error, StoreError::SessionRecordSecretLikeText));
    let sequence_text = fs::read_to_string(
        root.join("runs")
            .join("run_replacements")
            .join("content-replacements.seq"),
    )
    .unwrap();
    assert_eq!(sequence_text.trim(), "2");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn subagent_sidecars_reject_unsafe_agent_ids_and_secret_like_payloads() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_subagents");

    let path_error = store
        .append_subagent_transcript_record_next(
            &run_id,
            "../escape",
            None,
            "subagent.started",
            json!({}),
        )
        .unwrap_err();
    assert!(matches!(path_error, StoreError::InvalidFileName(_)));

    let secret_error = store
        .append_subagent_transcript_record_next(
            &run_id,
            "agent-1",
            None,
            "subagent.message",
            json!({"api_key": "redacted"}),
        )
        .unwrap_err();
    assert!(matches!(
        secret_error,
        StoreError::SessionRecordSecretLikeText
    ));

    let metadata = SubagentMetadata {
        agent_type: "reviewer".to_owned(),
        parent_agent_id: "executor".to_owned(),
        parent_harness_id: "native".to_owned(),
        invocation_kind: "spawn".to_owned(),
        status: Some("running".to_owned()),
        terminal_record_kind: None,
        last_sequence: None,
        error: None,
        description: Some("contains password marker".to_owned()),
        worktree_path: None,
        transcript_ref: None,
    };
    let metadata_error = store
        .write_subagent_metadata(&run_id, "agent-1", &metadata)
        .unwrap_err();
    assert!(matches!(
        metadata_error,
        StoreError::SessionRecordSecretLikeText
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cache_bucket_usage_counts_real_files_and_bytes() {
    let root = temp_root();
    let store = RunStore::new(&root);
    store.ensure_local_layout().unwrap();

    store.write_blob(b"abc").unwrap();
    fs::write(root.join("repo-index").join("index.jsonl"), b"abcd").unwrap();

    let blob_usage = store.cache_bucket_usage("blobs").unwrap();
    let repo_index_usage = store.cache_bucket_usage("repo-index").unwrap();
    let missing_usage = store.cache_bucket_usage("logs").unwrap();

    assert_eq!(blob_usage.entries, 1);
    assert_eq!(blob_usage.bytes, 3);
    assert_eq!(repo_index_usage.entries, 1);
    assert_eq!(repo_index_usage.bytes, 4);
    assert_eq!(missing_usage.entries, 0);
    assert_eq!(missing_usage.bytes, 0);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cache_bucket_usage_stops_at_scan_limit() {
    let root = temp_root();
    let cache_dir = root.join("repo-index");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(cache_dir.join("first.txt"), b"1234").unwrap();
    fs::create_dir_all(cache_dir.join("empty-dir")).unwrap();
    fs::write(cache_dir.join("second.txt"), b"5678").unwrap();

    let usage = cache_bucket_usage_at_with_limit(&cache_dir, 2).unwrap();

    assert_eq!(usage.scanned_entries, 2);
    assert_eq!(usage.entry_scan_limit, 2);
    assert!(usage.truncated);
    assert!(usage.entries <= 2);
    assert!(usage.bytes <= 8);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn clear_disposable_caches_preserves_durable_store_data() {
    let root = temp_root();
    let store = RunStore::new(&root);
    store.ensure_local_layout().unwrap();

    fs::write(root.join("repo-index").join("index.jsonl"), b"repo").unwrap();
    fs::write(root.join("tmp").join("scratch.txt"), b"tmp").unwrap();
    fs::write(root.join("logs").join("server.log"), b"log").unwrap();
    store.write_blob(b"blob").unwrap();
    store
        .append_session_record("session-1", 1, "session.created", json!({}))
        .unwrap();

    let summary = store.clear_disposable_caches().unwrap();

    assert_eq!(
        summary.directories,
        vec!["repo-index", "plugin-cache", "skill-cache", "tmp"]
    );
    assert_eq!(summary.entries, 2);
    assert_eq!(summary.bytes, 7);
    assert!(root.join("repo-index").exists());
    assert!(root.join("tmp").exists());
    assert!(!root.join("repo-index").join("index.jsonl").exists());
    assert!(!root.join("tmp").join("scratch.txt").exists());
    assert!(root.join("logs").join("server.log").exists());
    assert!(root.join("sessions").join("session-1.jsonl").exists());
    assert_eq!(store.cache_bucket_usage("blobs").unwrap().entries, 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn blob_refs_are_content_addressed() {
    let root = temp_root();
    let store = RunStore::new(&root);

    let first = store.write_blob(b"same content").unwrap();
    let second = store.write_blob(b"same content").unwrap();

    assert_eq!(first, second);
    assert!(first.starts_with("blob://sha256/"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn large_text_refs_store_full_content_outside_event_payload() {
    let root = temp_root();
    let store = RunStore::new(&root);

    let payload = store
        .write_large_text_ref_with_limit("0123456789", 4)
        .unwrap();

    assert_eq!(payload.preview, "0123");
    assert!(payload.truncated);
    assert!(payload.blob_ref.starts_with("blob://sha256/"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn artifact_names_reject_path_traversal() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_test");

    let error = store
        .write_artifact(&run_id, "../escape.json", &json!({"bad": true}))
        .unwrap_err();

    assert!(matches!(error, StoreError::InvalidFileName(_)));
    let wildcard_error = store
        .write_artifact(&run_id, "bad*name.json", &json!({"bad": true}))
        .unwrap_err();
    assert!(matches!(wildcard_error, StoreError::InvalidFileName(_)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn compaction_circuit_state_persists_failures_and_resets_on_success() {
    let root = temp_root();
    let store = RunStore::new(&root);

    let first = store
        .record_compaction_circuit_outcome("run-compact", 3, false)
        .unwrap();
    let second = store
        .record_compaction_circuit_outcome("run-compact", 3, false)
        .unwrap();
    let third = store
        .record_compaction_circuit_outcome("run-compact", 3, false)
        .unwrap();

    assert_eq!(first.consecutive_failures, 1);
    assert_eq!(second.consecutive_failures, 2);
    assert_eq!(third.consecutive_failures, 3);
    assert!(third.circuit_breaker_open);
    let persisted = store
        .read_compaction_circuit_state("run-compact")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.consecutive_failures, 3);
    assert!(persisted.circuit_breaker_open);

    let reset = store
        .record_compaction_circuit_outcome("run-compact", 3, true)
        .unwrap();
    assert_eq!(reset.consecutive_failures, 0);
    assert!(!reset.circuit_breaker_open);
    let invalid = store
        .record_compaction_circuit_outcome("../escape", 3, false)
        .unwrap_err();
    assert!(matches!(invalid, StoreError::InvalidFileName(_)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn goal_state_json_roundtrips_deletes_and_rejects_unsafe_or_secret_like_state() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let state = json!({
        "session_id": "session-1",
        "objective": "Ship the goal runtime",
        "status": "active",
        "token_budget": 100,
        "tokens_used": 5
    });

    assert!(store.read_goal_state_json("session-1").unwrap().is_none());
    let reference = store.write_goal_state_json("session-1", &state).unwrap();
    assert_eq!(reference, "goal://sessions/session-1.json");
    assert_eq!(
        store.read_goal_state_json("session-1").unwrap().unwrap(),
        state
    );
    assert!(store.delete_goal_state("session-1").unwrap());
    assert!(store.read_goal_state_json("session-1").unwrap().is_none());
    assert!(!store.delete_goal_state("session-1").unwrap());

    let invalid_write = store
        .write_goal_state_json("../escape", &json!({"objective": "safe"}))
        .unwrap_err();
    let invalid_read = store.read_goal_state_json("../escape").unwrap_err();
    let secret_write = store
        .write_goal_state_json("session-secret", &json!({"objective": "api_key=abc"}))
        .unwrap_err();

    assert!(matches!(invalid_write, StoreError::InvalidFileName(_)));
    assert!(matches!(invalid_read, StoreError::InvalidFileName(_)));
    assert!(matches!(
        secret_write,
        StoreError::SessionRecordSecretLikeText
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn artifact_json_roundtrips_and_reports_missing() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_test");

    let reference = store
        .write_artifact(&run_id, "summary.json", &json!({"status": "ok"}))
        .unwrap();
    let payload = store.read_artifact_json(&run_id, "summary.json").unwrap();
    let missing = store
        .read_artifact_json(&run_id, "missing.json")
        .unwrap_err();

    assert_eq!(reference, "artifact://runs/run_test/artifacts/summary.json");
    assert_eq!(payload["status"], "ok");
    assert!(matches!(missing, StoreError::ArtifactNotFound { .. }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn checkpoint_json_roundtrips_lists_and_reports_missing() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_test");

    let reference = store
        .write_checkpoint(&run_id, "resume.json", &json!({"step": 2}))
        .unwrap();
    let payload = store.read_checkpoint_json(&run_id, "resume.json").unwrap();
    let checkpoints = store.list_checkpoints(&run_id).unwrap();
    let missing = store
        .read_checkpoint_json(&run_id, "missing.json")
        .unwrap_err();

    assert_eq!(
        reference,
        "checkpoint://runs/run_test/checkpoints/resume.json"
    );
    assert_eq!(payload["step"], 2);
    assert_eq!(checkpoints[0].name, "resume.json");
    assert_eq!(checkpoints[0].checkpoint_ref, reference);
    assert!(matches!(missing, StoreError::CheckpointNotFound { .. }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn checkpoint_names_reject_path_traversal() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_test");

    let error = store
        .write_checkpoint(&run_id, "../escape.json", &json!({"bad": true}))
        .unwrap_err();

    assert!(matches!(error, StoreError::InvalidFileName(_)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn blob_reads_by_sha256_digest() {
    let root = temp_root();
    let store = RunStore::new(&root);

    let reference = store.write_blob(b"same content").unwrap();
    let digest = reference.strip_prefix("blob://sha256/").unwrap();
    let loaded = store.read_blob_sha256(digest).unwrap();
    let missing = store
        .read_blob_sha256("0000000000000000000000000000000000000000000000000000000000000000")
        .unwrap_err();
    let invalid = store.read_blob_sha256("../escape").unwrap_err();

    assert_eq!(loaded, b"same content");
    assert!(matches!(missing, StoreError::BlobNotFound(_)));
    assert!(matches!(invalid, StoreError::InvalidBlobDigest(_)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn durable_read_limit_rejects_oversized_files() {
    let root = temp_root();
    fs::create_dir_all(&root).unwrap();
    let path = root.join("oversized.jsonl");
    fs::write(&path, "12345").unwrap();

    let error = reject_file_over_read_limit(&path, 4).unwrap_err();

    assert!(matches!(
        error,
        StoreError::DurableReadLimitExceeded {
            bytes: 5,
            max_bytes: 4,
            ..
        }
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn durable_json_sidecar_reads_reject_oversized_files() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_oversized_sidecars");
    let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    state.status = coder_core::RunStatus::Completed;
    store.write_metadata(&state).unwrap();
    store
        .write_artifact(&run_id, "summary.json", &json!({"status": "ok"}))
        .unwrap();
    store
        .write_checkpoint(&run_id, "resume.json", &json!({"step": 1}))
        .unwrap();
    store
        .write_subagent_metadata(
            &run_id,
            "agent-1",
            &SubagentMetadata {
                agent_type: "reviewer".to_owned(),
                parent_agent_id: "executor".to_owned(),
                parent_harness_id: "native".to_owned(),
                invocation_kind: "spawn".to_owned(),
                status: Some("running".to_owned()),
                terminal_record_kind: None,
                last_sequence: None,
                error: None,
                description: None,
                worktree_path: None,
                transcript_ref: None,
            },
        )
        .unwrap();

    let oversized_paths = [
        root.join("runs")
            .join("run_oversized_sidecars")
            .join("metadata.json"),
        root.join("runs")
            .join("run_oversized_sidecars")
            .join("artifacts")
            .join("summary.json"),
        root.join("runs")
            .join("run_oversized_sidecars")
            .join("checkpoints")
            .join("resume.json"),
        root.join("runs")
            .join("run_oversized_sidecars")
            .join("subagents")
            .join("agent-agent-1.meta.json"),
    ];
    for path in &oversized_paths {
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(MAX_DURABLE_READ_BYTES + 1)
            .unwrap();
    }

    assert!(matches!(
        store.read_metadata(&run_id).unwrap_err(),
        StoreError::DurableReadLimitExceeded { .. }
    ));
    assert!(matches!(
        store
            .read_artifact_json(&run_id, "summary.json")
            .unwrap_err(),
        StoreError::DurableReadLimitExceeded { .. }
    ));
    assert!(matches!(
        store
            .read_checkpoint_json(&run_id, "resume.json")
            .unwrap_err(),
        StoreError::DurableReadLimitExceeded { .. }
    ));
    assert!(matches!(
        store
            .read_subagent_metadata(&run_id, "agent-1")
            .unwrap_err(),
        StoreError::DurableReadLimitExceeded { .. }
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn run_store_operations_reject_unsafe_run_segments() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("../escape");
    let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    state.status = coder_core::RunStatus::Completed;

    let metadata_error = store.write_metadata(&state).unwrap_err();
    let event_error = store
        .append_event(
            &run_id,
            &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
        )
        .unwrap_err();
    let artifact_error = store
        .write_artifact(&run_id, "summary.json", &json!({"bad": true}))
        .unwrap_err();

    assert!(matches!(
        metadata_error,
        StoreError::InvalidStoreSegment { .. }
    ));
    assert!(matches!(
        event_error,
        StoreError::InvalidStoreSegment { .. }
    ));
    assert!(matches!(
        artifact_error,
        StoreError::InvalidStoreSegment { .. }
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn metadata_and_report_roundtrip() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_test");
    let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    state.status = coder_core::RunStatus::Completed;
    let report = FinalReport::completed("done").with_evidence("event_log", "eventlog://run");

    store.write_metadata(&state).unwrap();
    store.write_report(&run_id, &report).unwrap();

    let loaded_state = store.read_metadata(&run_id).unwrap().unwrap();
    let loaded_report = store.read_report(&run_id).unwrap().unwrap();
    assert_eq!(loaded_state.status, coder_core::RunStatus::Completed);
    assert_eq!(loaded_report.summary, "done");
    assert_eq!(loaded_report.evidence_refs[0].kind, "event_log");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn report_artifact_redacts_key_like_strings() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_test");
    let mut report = FinalReport::completed("Used sk-live-1234567890");
    report
        .checks
        .push("cargo test sk-live-1234567890".to_owned());
    report
        .blockers
        .push("blocked by sk-live-1234567890".to_owned());

    store.write_report(&run_id, &report).unwrap();

    let text = fs::read_to_string(
        root.join("runs")
            .join("run_test")
            .join("artifacts")
            .join("final-report.json"),
    )
    .unwrap();
    assert!(!text.contains("sk-live-1234567890"));
    assert!(text.contains("[REDACTED]"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_blocks_on_command_approval_request() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "approval.requested",
                json!({
                    "approval_type": "command",
                    "command": "cargo test",
                    "approval_key": "cmd:abc"
                }),
            ),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.status, ReportStatus::Blocked);
    assert!(report
        .blockers
        .iter()
        .any(|item| item.contains("cargo test")));
    assert!(report
        .evidence_refs
        .iter()
        .any(|reference| reference.kind == "event_log"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_fails_on_failed_command_event() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "command.failed",
                json!({
                    "command": "cargo test",
                    "status": "failed",
                    "passed": false,
                    "returncode": 101
                }),
            ),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.status, ReportStatus::Failed);
    assert!(report.checks[0].contains("cargo test"));
    assert!(report.blockers[0].contains("Command failed"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_keeps_recovered_command_failure_as_non_blocking_history() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "command.failed",
                json!({
                    "command": "cargo test",
                    "status": "failed",
                    "passed": false,
                    "returncode": 101
                }),
            ),
        )
        .unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(run_id.clone(), 2, "run.completed", json!({})),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.status, ReportStatus::Completed);
    assert!(report
        .checks
        .iter()
        .any(|check| check.contains("cargo test: failed exit 101")));
    assert!(report.blockers.is_empty());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_does_not_fail_on_cancelled_command_event() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "command.failed",
                json!({
                    "command": "powershell Start-Sleep -Seconds 30",
                    "status": "cancelled",
                    "passed": false,
                    "returncode": 1,
                    "timed_out": false
                }),
            ),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.status, ReportStatus::Completed);
    assert!(report
        .checks
        .iter()
        .any(|check| check.contains("cancelled exit 1")));
    assert!(report.blockers.is_empty());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_includes_plan_context_from_run_started() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({
                    "plan_context": {
                        "original_user_request": "Update Planner loop",
                        "plan_draft": {
                            "goal": "Update Planner loop",
                            "acceptance_criteria": ["final report cites plan context"]
                        }
                    }
                }),
            ),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert!(report
        .checks
        .iter()
        .any(|check| check == "plan_context: Update Planner loop"));
    assert!(!report
        .checks
        .iter()
        .any(|check| check == "acceptance: final report cites plan context"));
    assert!(report.summary.contains("Requested: Update Planner loop"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_summary_covers_request_work_evidence_risks_and_next_steps() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"task": "Update README.md"}),
            ),
        )
        .unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                2,
                "command.completed",
                json!({
                    "command": "cargo test",
                    "status": "completed",
                    "passed": true,
                    "returncode": 0
                }),
            )
            .with_ref("command_evidence", "repo-evidence://repo-test:abc"),
        )
        .unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                3,
                "patch.applied",
                json!({
                    "evidence_ref": "repo-diff:def",
                    "files": [{"new_path": "README.md", "status": "modified"}]
                }),
            )
            .with_ref("patch_evidence", "repo-evidence://repo-diff:def"),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.status, ReportStatus::Completed);
    assert!(report.summary.contains("Status: completed"));
    assert!(report.summary.contains("Requested: Update README.md"));
    assert!(report
        .summary
        .contains("Done: Command completed: cargo test"));
    assert!(report.summary.contains("Patch applied"));
    assert!(report.summary.contains("Changed files: README.md"));
    assert!(report
        .summary
        .contains("Verification: cargo test: completed exit 0"));
    assert!(report
        .summary
        .contains("Evidence: 3 evidence ref(s) recorded"));
    assert!(report
        .summary
        .contains("Remaining risks: No remaining blocker or risk was recorded."));
    assert!(report
        .summary
        .contains("Next steps: No next step was recorded."));
    assert!(!report.summary.contains("repo-evidence://"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_includes_completed_verification_events() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "verification.completed",
                json!({
                    "status": "completed",
                    "summary": "browser gameplay passed",
                    "evidence": {"total_refs": 1}
                }),
            )
            .with_ref("browser_validation", "blob://sha256/browser-proof"),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.status, ReportStatus::Completed);
    assert!(report
        .checks
        .iter()
        .any(|check| check == "verification: browser gameplay passed"));
    assert!(report
        .summary
        .contains("Verification: verification: browser gameplay passed"));
    assert!(report.evidence_refs.iter().any(|reference| {
        reference.kind == "browser_validation"
            && reference.reference == "blob://sha256/browser-proof"
    }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_expands_verification_check_payloads() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "verification.completed",
                json!({
                    "status": "completed",
                    "summary": "verification passed",
                    "checks": [
                        {
                            "name": "task.check",
                            "status": "passed",
                            "detail": "restart reset visible score"
                        }
                    ]
                }),
            ),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert!(report
        .checks
        .iter()
        .any(|check| { check == "verification: task.check passed - restart reset visible score" }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_blocks_on_missing_required_verification_evidence() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "verification.failed",
                    json!({
                        "status": "failed",
                        "reason": "verification requires evidence refs before completion, but the backend returned none",
                        "evidence": {"total_refs": 0}
                    }),
                ),
            )
            .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.status, ReportStatus::Blocked);
    assert!(report
        .checks
        .iter()
        .any(|check| check.contains("verification: failed")));
    assert!(report
        .blockers
        .iter()
        .any(|blocker| blocker.contains("Verification blocked")));
    assert!(report
        .summary
        .contains("Remaining risks: Verification blocked"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_clears_repaired_verification_failure() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "verification.failed",
                json!({
                    "status": "blocked",
                    "reason": "external runtime missing"
                }),
            ),
        )
        .unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                2,
                "verification.completed",
                json!({
                    "status": "completed",
                    "summary": "verification passed"
                }),
            ),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.status, ReportStatus::Completed);
    assert!(report
        .checks
        .iter()
        .any(|check| check.contains("external runtime missing")));
    assert!(report.blockers.is_empty());
    assert!(report
        .summary
        .contains("Remaining risks: No remaining blocker or risk was recorded."));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_cancels_on_cancelled_run_state() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    state.status = RunStatus::Cancelled;
    store.write_metadata(&state).unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.cancelled",
                json!({"reason": "user_cancelled"}),
            ),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.status, ReportStatus::Cancelled);
    assert!(report.summary.contains("cancelled"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_includes_repo_evidence_only_runs() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let reference = store
        .write_repo_evidence(
            &run_id,
            RepoEvidenceKind::RepoRead,
            "repo",
            Vec::new(),
            "read",
            json!({"snippet": "safe"}),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.status, ReportStatus::Completed);
    assert!(report
        .evidence_refs
        .iter()
        .any(|item| item.reference == reference.ref_id));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_includes_patch_event_files_and_refs() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "patch.previewed",
                json!({
                    "evidence_ref": "repo-diff:abc",
                    "files": [
                        {
                            "old_path": "src/old.py",
                            "new_path": "src/app.py",
                            "status": "modified"
                        }
                    ]
                }),
            )
            .with_ref("patch_evidence", "repo-evidence://repo-diff:abc"),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.changed_files, vec!["src/app.py"]);
    assert_eq!(report.patch_refs, vec!["repo-evidence://repo-diff:abc"]);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_blocks_on_patch_apply_approval_request() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "approval.requested",
                json!({
                    "approval_type": "patch_apply",
                    "patch_file": "change.patch",
                    "evidence_ref": "repo-diff:abc",
                    "files": [
                        {
                            "old_path": "src/app.py",
                            "new_path": "src/app.py",
                            "status": "modified"
                        }
                    ]
                }),
            )
            .with_ref("patch_evidence", "repo-evidence://repo-diff:abc"),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.status, ReportStatus::Blocked);
    assert_eq!(report.changed_files, vec!["src/app.py"]);
    assert_eq!(report.patch_refs, vec!["repo-evidence://repo-diff:abc"]);
    assert!(report.blockers[0].contains("change.patch"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_tracks_applied_and_failed_patch_events() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "patch.applied",
                json!({
                    "patch_file": "good.patch",
                    "evidence_ref": "repo-diff:good",
                    "files": [
                        {
                            "old_path": "src/app.py",
                            "new_path": "src/app.py",
                            "status": "modified"
                        }
                    ]
                }),
            )
            .with_ref("patch_evidence", "repo-evidence://repo-diff:good"),
        )
        .unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                2,
                "patch.failed",
                json!({
                    "patch_file": "bad.patch",
                    "evidence_ref": "repo-diff:bad",
                    "files": [
                        {
                            "old_path": "src/bad.py",
                            "new_path": "src/bad.py",
                            "status": "modified"
                        }
                    ]
                }),
            )
            .with_ref("patch_evidence", "repo-evidence://repo-diff:bad"),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.status, ReportStatus::Failed);
    assert_eq!(report.changed_files, vec!["src/app.py", "src/bad.py"]);
    assert_eq!(
        report.patch_refs,
        vec![
            "repo-evidence://repo-diff:bad",
            "repo-evidence://repo-diff:good"
        ]
    );
    assert!(report.blockers[0].contains("bad.patch"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_report_includes_repo_patch_preview_files_and_refs() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let reference = store
        .write_repo_evidence(
            &run_id,
            RepoEvidenceKind::RepoDiff,
            "repo",
            Vec::new(),
            "Previewed patch touching 1 file.",
            json!({
                "operation": "patch_preview",
                "preview": {
                    "files": [
                        {
                            "old_path": null,
                            "new_path": "src/new.py",
                            "status": "added"
                        }
                    ]
                }
            }),
        )
        .unwrap();

    let report = store.build_evidence_report(&run_id).unwrap();

    assert_eq!(report.changed_files, vec!["src/new.py"]);
    assert_eq!(
        report.patch_refs,
        vec![format!("repo-evidence://{}", reference.ref_id)]
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn list_run_summaries_reports_counts_and_skips_unsafe_dirs() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run_test");
    let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    state.status = coder_core::RunStatus::Completed;
    let report = FinalReport::completed("done");

    fs::create_dir_all(root.join("runs").join("bad run")).unwrap();
    store.write_metadata(&state).unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
        )
        .unwrap();
    store.write_report(&run_id, &report).unwrap();
    store
        .write_repo_evidence(
            &run_id,
            RepoEvidenceKind::RepoRead,
            "repo",
            Vec::new(),
            "read",
            json!({"snippet": "safe"}),
        )
        .unwrap();

    let summaries = store.list_run_summaries().unwrap();

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].run_id, "run_test");
    assert_eq!(summaries[0].metadata.as_ref().unwrap().status, state.status);
    assert_eq!(summaries[0].event_count, 1);
    assert!(summaries[0].has_report);
    assert_eq!(summaries[0].repo_evidence_count, 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn list_run_summaries_is_empty_without_runs_dir() {
    let root = temp_root();
    let store = RunStore::new(&root);

    let summaries = store.list_run_summaries().unwrap();

    assert!(summaries.is_empty());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn repo_evidence_roundtrips_with_index_ref() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");

    let reference = store
        .write_repo_evidence(
            &run_id,
            RepoEvidenceKind::RepoTextSearch,
            "F:/repo",
            vec!["src".to_owned()],
            "Found one hit.",
            json!({"evidence_kind": "repo_evidence", "hits": [{"path": "src/app.py", "line": 1}]}),
        )
        .unwrap();
    let payload = store.read_repo_evidence(&reference.ref_id).unwrap();

    assert!(reference.ref_id.starts_with("repo-text-search:"));
    assert_eq!(reference.kind, RepoEvidenceKind::RepoTextSearch);
    assert!(PathBuf::from(&reference.payload_path)
        .starts_with(root.join("runs").join("run-1").join("repo_evidence")));
    assert_eq!(payload["hits"][0]["path"], "src/app.py");
    let records = store.list_repo_evidence(&run_id).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].ref_id, reference.ref_id);
    assert_eq!(records[0].summary, "Found one hit.");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn repo_evidence_rejects_unsafe_segments() {
    let root = temp_root();
    let store = RunStore::new(&root);

    let run_error = store
        .write_repo_evidence(
            &RunId::from_string("../escape"),
            RepoEvidenceKind::RepoRead,
            "repo",
            Vec::new(),
            "bad",
            json!({"text": "safe"}),
        )
        .unwrap_err();
    let ref_error = store.read_repo_evidence("../escape").unwrap_err();

    assert!(matches!(run_error, StoreError::InvalidStoreSegment { .. }));
    assert!(matches!(ref_error, StoreError::InvalidStoreSegment { .. }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn repo_evidence_compacts_large_strings_and_lists() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let items = (0..350)
        .map(|index| json!({"path": format!("src/{index}.rs")}))
        .collect::<Vec<_>>();

    let reference = store
        .write_repo_evidence(
            &run_id,
            RepoEvidenceKind::RepoFileList,
            "repo",
            Vec::new(),
            "large",
            json!({"snippet": "x".repeat(20_000), "items": items}),
        )
        .unwrap();
    let payload = store.read_repo_evidence(&reference.ref_id).unwrap();

    assert!(payload["snippet"].as_str().unwrap().len() < 20_000);
    assert!(payload["snippet"].as_str().unwrap().ends_with("..."));
    assert_eq!(payload["items"].as_array().unwrap().len(), 301);
    assert_eq!(payload["items"][300]["truncated"], true);
    assert_eq!(payload["items"][300]["omitted_items"], 50);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn repo_evidence_rejects_secret_like_text() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");

    let error = store
        .write_repo_evidence(
            &run_id,
            RepoEvidenceKind::RepoRead,
            "repo",
            Vec::new(),
            "secret",
            json!({"snippet": "api_key=abc"}),
        )
        .unwrap_err();

    assert!(matches!(error, StoreError::RepoEvidenceSecretLikeText));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn repo_evidence_rejects_payload_path_escape() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let reference = store
        .write_repo_evidence(
            &run_id,
            RepoEvidenceKind::RepoRead,
            "repo",
            Vec::new(),
            "read",
            json!({"snippet": "safe"}),
        )
        .unwrap();
    let outside = root.join("outside.json");
    fs::write(&outside, "{}").unwrap();
    let mut escaped = reference;
    escaped.payload_path = outside.display().to_string();
    let index_path = root
        .join("runs")
        .join("run-1")
        .join("repo_evidence")
        .join("index.jsonl");
    fs::write(
        index_path,
        format!("{}\n", serde_json::to_string(&escaped).unwrap()),
    )
    .unwrap();

    let error = store.read_repo_evidence(&escaped.ref_id).unwrap_err();

    assert!(matches!(error, StoreError::RepoEvidencePathEscape(_)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn repo_evidence_reads_reject_oversized_index_and_payload() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let reference = store
        .write_repo_evidence(
            &run_id,
            RepoEvidenceKind::RepoRead,
            "repo",
            Vec::new(),
            "read",
            json!({"snippet": "safe"}),
        )
        .unwrap();
    let payload_path = PathBuf::from(&reference.payload_path);
    fs::OpenOptions::new()
        .write(true)
        .open(&payload_path)
        .unwrap()
        .set_len(MAX_DURABLE_READ_BYTES + 1)
        .unwrap();

    let payload_error = store.read_repo_evidence(&reference.ref_id).unwrap_err();
    assert!(matches!(
        payload_error,
        StoreError::DurableReadLimitExceeded { .. }
    ));

    fs::write(&payload_path, "{}").unwrap();
    let index_path = root
        .join("runs")
        .join("run-1")
        .join("repo_evidence")
        .join("index.jsonl");
    fs::OpenOptions::new()
        .write(true)
        .open(&index_path)
        .unwrap()
        .set_len(MAX_DURABLE_READ_BYTES + 1)
        .unwrap();

    let read_index_error = store.read_repo_evidence(&reference.ref_id).unwrap_err();
    assert!(matches!(
        read_index_error,
        StoreError::DurableReadLimitExceeded { .. }
    ));
    let list_index_error = store.list_repo_evidence(&run_id).unwrap_err();
    assert!(matches!(
        list_index_error,
        StoreError::DurableReadLimitExceeded { .. }
    ));
    let _ = fs::remove_dir_all(root);
}

fn temp_root() -> PathBuf {
    static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    test_tmp_root().join(format!("coder-store-{}-{}", std::process::id(), id))
}

fn test_tmp_root() -> PathBuf {
    std::env::var_os("CODER_TEST_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}
