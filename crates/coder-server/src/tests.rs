use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};

use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header::CONTENT_TYPE, HeaderValue, Request, StatusCode},
    response::IntoResponse,
    routing::post,
    Json,
};
use coder_config::{
    MemoryScope as ConfigMemoryScope, ModelSpec as ConfigModelSpec,
    PermissionDecision as ConfigPermissionDecision,
};
use coder_core::RunState;
use coder_events::CoderEvent;
use coder_harness::{HarnessBackend, HarnessRunEvent, HarnessRunRequest};
use coder_workflow::ModelToolResultBlock;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::model_tool_command_hooks::CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS;
use super::outbound_http::url_matches_no_proxy;
use super::planner_provider_dispatch::{planner_event_text, NativePlannerContextAdapter};
use super::planner_provider_recovery::PlannerProviderRequestMode;
use super::planner_provider_runtime::{
    parse_live_planner_response_with_idle_timeout, planner_chat_completion_body,
    planner_chat_completion_body_with_tools, planner_provider_trace,
};
use super::planner_session::{
    store_planner_session_snapshot, trim_planner_session_turns, PLANNER_SESSION_CACHE_LIMIT,
    PLANNER_SESSION_MAX_TURNS,
};
use super::provider_runtime::{
    provider_api_key, provider_chat_completions_endpoint,
    provider_chat_completions_endpoint_for_display, provider_proxy_mode,
    provider_proxy_url_for_url, provider_request_max_retries, provider_stream_idle_timeout_ms,
    provider_stream_max_retries, provider_supports_websockets,
    provider_websocket_connect_timeout_ms, PROVIDER_REQUEST_MAX_RETRIES,
    PROVIDER_STREAM_IDLE_TIMEOUT_MS, PROVIDER_STREAM_MAX_RETRIES,
    PROVIDER_WEBSOCKET_CONNECT_TIMEOUT_MS,
};
use super::provider_settings::{apply_provider_settings_patch, provider_test_chat_completion_body};
use super::*;

type CaptureState = Arc<(Value, Arc<Mutex<Option<Value>>>)>;
type SequenceCaptureState = Arc<(Mutex<VecDeque<Value>>, Arc<Mutex<Vec<Value>>>)>;
type StreamingSequenceCaptureState = Arc<(Mutex<VecDeque<String>>, Arc<Mutex<Vec<Value>>>)>;
type CommandLoopState = Arc<(Vec<String>, Arc<Mutex<Vec<Value>>>)>;
type StatusSequenceState = Arc<(
    Mutex<VecDeque<OpenAiCompatibleStatusResponse>>,
    Arc<Mutex<Vec<Value>>>,
)>;

fn planner_session_fixture(session_id: impl Into<String>) -> PlannerChatSession {
    PlannerChatSession {
        session_id: session_id.into(),
        workflow_id: "planner-led".to_owned(),
        repo_root: None,
        mode: "discuss".to_owned(),
        runtime: None,
        ready: false,
        readiness: PlannerReadiness::NeedsClarification,
        plan_draft: None,
        open_questions: Vec::new(),
        acceptance_criteria: Vec::new(),
        risks: Vec::new(),
        work_in_progress: false,
        active_run_id: None,
        latest_run_id: None,
        turns: Vec::new(),
    }
}

#[tokio::test]
async fn health_endpoint_returns_v3_status() {
    let app = test_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["api_version"], "v3");
}

#[test]
fn planner_session_turn_cache_keeps_recent_history() {
    let mut session = planner_session_fixture("session-trim");
    for index in 0..(PLANNER_SESSION_MAX_TURNS + 5) {
        session.turns.push(PlannerChatTurn {
            role: if index % 2 == 0 { "user" } else { "assistant" }.to_owned(),
            content: format!("turn-{index}"),
            artifacts: Vec::new(),
            response_truncated: false,
        });
    }

    trim_planner_session_turns(&mut session);

    assert_eq!(session.turns.len(), PLANNER_SESSION_MAX_TURNS);
    assert_eq!(session.turns.first().unwrap().content, "turn-5");
    assert_eq!(
        session.turns.last().unwrap().content,
        format!("turn-{}", PLANNER_SESSION_MAX_TURNS + 4)
    );
}

#[test]
fn planner_session_cache_evicts_oldest_live_session() {
    let mut sessions = BTreeMap::new();
    for index in 0..=PLANNER_SESSION_CACHE_LIMIT {
        let session_id = format!("session-{index:03}");
        let session = planner_session_fixture(session_id);
        let now = UNIX_EPOCH + Duration::from_secs(index as u64);
        store_planner_session_snapshot(&mut sessions, session, now);
    }

    assert_eq!(sessions.len(), PLANNER_SESSION_CACHE_LIMIT);
    assert!(!sessions.contains_key("session-000"));
    assert!(sessions.contains_key(&format!("session-{PLANNER_SESSION_CACHE_LIMIT:03}")));
}

#[tokio::test]
async fn capabilities_and_role_cards_expose_product_surface() {
    let app = test_router();
    let capabilities_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/capabilities")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(capabilities_response.status(), StatusCode::OK);
    let capabilities = response_json(capabilities_response).await;
    assert_eq!(capabilities["api_version"], "v3");
    assert!(capabilities["workflow"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("graph_semantics")));
    assert!(capabilities["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("command_run")));
    assert!(capabilities["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("command_background")));
    assert!(capabilities["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("read_command_output")));
    assert!(capabilities["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("agent_subagent")));
    assert!(capabilities["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("read_subagent_status")));
    assert!(capabilities["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("cancel_subagent_background")));
    assert!(capabilities["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("model_tool_execute")));
    assert!(capabilities["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("model_tool_turn")));
    assert!(capabilities["runs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("verification_evidence")));
    assert!(capabilities["runs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("async_notifications")));
    assert!(capabilities["runs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("async_notifications_drain")));
    assert!(capabilities["runs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("permission_updates")));
    assert!(capabilities["planner_chat"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("persistent_goals")));

    let role_cards_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/agent-role-cards")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(role_cards_response.status(), StatusCode::OK);
    let role_cards = response_json(role_cards_response).await;
    let executor = role_cards["role_cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["id"] == "executor")
        .unwrap();
    assert_eq!(executor["role"], "executor");
    assert!(executor["default_capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("return_execution_result")));
}

#[tokio::test]
async fn goal_runtime_endpoints_persist_claude_style_state_transitions() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));

    let empty_response = get_json(app.clone(), "/api/v3/goals/session-goal").await;
    assert_eq!(empty_response.status(), StatusCode::OK);
    let empty = response_json(empty_response).await;
    assert_eq!(empty["goal"], Value::Null);
    assert_eq!(empty["policy"]["blocked_consecutive_threshold"], 3);
    assert_eq!(empty["policy"]["max_goal_turns"], 150);

    let create_response = post_json(
        app.clone(),
        "/api/v3/goals/session-goal",
        json!({
            "objective": "Ship goal runtime",
            "token_budget": 100
        }),
    )
    .await;
    assert_eq!(create_response.status(), StatusCode::OK);
    let created = response_json(create_response).await;
    assert_eq!(created["state_ref"], "goal://sessions/session-goal.json");
    assert_eq!(created["goal"]["status"], "active");

    let loaded_response = get_json(app.clone(), "/api/v3/goals/session-goal").await;
    assert_eq!(loaded_response.status(), StatusCode::OK);
    let loaded = response_json(loaded_response).await;
    assert_eq!(loaded["goal"]["objective"], "Ship goal runtime");
    assert!(store
        .read_goal_state_json("session-goal")
        .unwrap()
        .is_some());

    let first_block = post_json(
        app.clone(),
        "/api/v3/goals/session-goal/blocked",
        json!({"reason": "Need provider"}),
    )
    .await;
    assert_eq!(first_block.status(), StatusCode::OK);
    let first = response_json(first_block).await;
    assert_eq!(first["goal"]["status"], "active");
    assert_eq!(first["goal"]["blocked_attempts"], 1);

    let reset_block = post_json(
        app.clone(),
        "/api/v3/goals/session-goal/blocked",
        json!({"reason": "Need approval"}),
    )
    .await;
    assert_eq!(reset_block.status(), StatusCode::OK);
    let reset = response_json(reset_block).await;
    assert_eq!(reset["goal"]["status"], "active");
    assert_eq!(reset["goal"]["blocked_attempts"], 1);

    let second_block = post_json(
        app.clone(),
        "/api/v3/goals/session-goal/blocked",
        json!({"reason": " need approval "}),
    )
    .await;
    assert_eq!(second_block.status(), StatusCode::OK);
    let second = response_json(second_block).await;
    assert_eq!(second["goal"]["status"], "active");
    assert_eq!(second["goal"]["blocked_attempts"], 2);

    let blocked_response = post_json(
        app.clone(),
        "/api/v3/goals/session-goal/blocked",
        json!({"reason": "Need Approval"}),
    )
    .await;
    assert_eq!(blocked_response.status(), StatusCode::OK);
    let blocked = response_json(blocked_response).await;
    assert_eq!(blocked["goal"]["status"], "blocked");
    assert_eq!(blocked["goal"]["blocked_attempts"], 3);

    let turn_create_response = post_json(
        app.clone(),
        "/api/v3/goals/session-turns",
        json!({
            "objective": "Exercise max turns",
            "token_budget": null
        }),
    )
    .await;
    assert_eq!(turn_create_response.status(), StatusCode::OK);

    let mut last_turn = Value::Null;
    for _ in 0..150 {
        let turn_response =
            post_json(app.clone(), "/api/v3/goals/session-turns/turn", json!({})).await;
        assert_eq!(turn_response.status(), StatusCode::OK);
        last_turn = response_json(turn_response).await;
    }
    assert_eq!(last_turn["goal"]["status"], "max_turns");
    assert_eq!(last_turn["goal"]["turns_executed"], 150);

    let continue_response = post_json(
        app.clone(),
        "/api/v3/goals/session-turns/continue",
        json!({}),
    )
    .await;
    assert_eq!(continue_response.status(), StatusCode::OK);
    let continued = response_json(continue_response).await;
    assert_eq!(continued["goal"]["status"], "active");
    assert_eq!(continued["goal"]["turns_executed"], 0);

    let budget_create_response = post_json(
        app.clone(),
        "/api/v3/goals/session-budget",
        json!({
            "objective": "Exercise token budget",
            "token_budget": 10
        }),
    )
    .await;
    assert_eq!(budget_create_response.status(), StatusCode::OK);
    let first_tokens = post_json(
        app.clone(),
        "/api/v3/goals/session-budget/tokens",
        json!({"delta": 4}),
    )
    .await;
    assert_eq!(first_tokens.status(), StatusCode::OK);
    let first_tokens = response_json(first_tokens).await;
    assert_eq!(first_tokens["goal"]["status"], "active");
    assert_eq!(first_tokens["goal"]["tokens_used"], 4);

    let budget_limited = post_json(
        app.clone(),
        "/api/v3/goals/session-budget/tokens",
        json!({"delta": 6}),
    )
    .await;
    assert_eq!(budget_limited.status(), StatusCode::OK);
    let budget_limited = response_json(budget_limited).await;
    assert_eq!(budget_limited["goal"]["status"], "budget_limited");
    assert_eq!(budget_limited["goal"]["tokens_used"], 10);

    let clear_response =
        post_json(app.clone(), "/api/v3/goals/session-budget/clear", json!({})).await;
    assert_eq!(clear_response.status(), StatusCode::OK);
    let clear = response_json(clear_response).await;
    assert_eq!(clear["removed"], true);
    let cleared_response = get_json(app, "/api/v3/goals/session-budget").await;
    assert_eq!(cleared_response.status(), StatusCode::OK);
    assert_eq!(response_json(cleared_response).await["goal"], Value::Null);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn default_workflow_endpoint_returns_planner_led_spec() {
    let app = test_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/workflows/default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["workflow_id"], "planner-led");
    assert_eq!(body["config"]["version"], 1);
    assert_eq!(
        body["config"]["harnesses"]["planner-conversation"]["backend"],
        "planner-model"
    );
    assert_eq!(body["workflow"]["nodes"][0]["harness"], "native-code-edit");
    assert_eq!(body["workflow"]["nodes"][1]["harness"], "workflow-planner");
    assert_eq!(body["workflow"]["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(body["workflow"]["name"], "Verified Execution Workflow");
}

#[tokio::test]
async fn library_workflow_endpoints_roundtrip_in_memory_specs() {
    let app = test_router();
    let save_response = post_json(
        app.clone(),
        "/api/v3/library/workflows",
        json!({
            "workflow_id": "custom-flow",
            "workflow": {
                "name": "Custom Flow",
                "nodes": [{"id": "planner", "agent": "planner", "harness": "planner-harness"}],
                "edges": []
            }
        }),
    )
    .await;
    assert_eq!(save_response.status(), StatusCode::OK);
    let save_body = response_json(save_response).await;
    assert_eq!(save_body["workflow_id"], "custom-flow");
    assert_eq!(save_body["saved"], true);

    let get_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/library/workflows/custom-flow")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_response.status(), StatusCode::OK);
    let get_body = response_json(get_response).await;
    assert_eq!(get_body["workflow"]["name"], "Custom Flow");

    let list_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/library")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = response_json(list_response).await;
    assert_eq!(list_body["workflows"][0]["id"], "custom-flow");
}

mod planner_chat;

#[tokio::test]
async fn project_memory_load_records_summary_event_without_full_content() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(
        repo.join("memory.json"),
        r#"{
              "version": 1,
              "records": [
                {
                  "id": "mem_1",
                  "scope": "project",
                  "key": "architecture",
                  "content": "Rust owns the control plane.",
                  "tags": ["rust"],
                  "source_ref": "memory://project/architecture"
                }
              ]
            }"#,
    )
    .unwrap();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-1");
    let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    store.write_metadata(&state).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/memory/project/load",
        json!({
            "repo_root": repo.display().to_string(),
            "memory_path": "memory.json",
            "requested_by_role": "planning_chat",
            "run_id": "run-1"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["record_count"], 1);
    assert_eq!(body["event_recorded"], true);
    assert_eq!(body["memory"]["records"][0]["key"], "architecture");
    let events = store.read_events(&run_id).unwrap();
    assert_eq!(events[0].kind, "memory.read");
    assert_eq!(events[0].payload["records"][0]["key"], "architecture");
    assert!(!events[0].payload.to_string().contains("control plane"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn workflow_agents_cannot_read_project_memory() {
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(
            repo.join("memory.json"),
            r#"{"version":1,"records":[{"id":"mem_1","scope":"project","key":"architecture","content":"Rust owns the control plane.","tags":[],"source_ref":"memory://project/architecture"}]}"#,
        )
        .unwrap();
    let app = test_router();

    let response = post_json(
        app,
        "/api/v3/memory/project/load",
        json!({
            "repo_root": repo.display().to_string(),
            "memory_path": "memory.json",
            "requested_by_role": "task_execution"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _ = fs::remove_dir_all(repo);
}

#[tokio::test]
async fn project_memory_write_proposal_records_bounded_event_only() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-1");
    let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    store.write_metadata(&state).unwrap();
    let app = router(ApiState::new(store.clone()));
    let content = format!("{}tail-marker", "x".repeat(520));

    let response = post_json(
        app,
        "/api/v3/memory/project/propose-write",
        json!({
            "run_id": "run-1",
            "proposed_by_role": "planning_chat",
            "record": {
                "id": "mem_2",
                "scope": "project",
                "key": "migration-note",
                "content": content,
                "tags": ["rust"],
                "evidence_refs": [{"kind": "doc", "reference": "docs/memory-spec.md"}],
                "source_ref": "memory://project/migration-note"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["run_id"], "run-1");
    assert_eq!(body["event"]["kind"], "memory.write.proposed");
    assert_eq!(body["event"]["payload"]["record"]["key"], "migration-note");
    assert_eq!(body["event"]["payload"]["content_truncated"], true);
    assert!(!body["event"]["payload"].to_string().contains("tail-marker"));
    let events = store.read_events(&run_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "memory.write.proposed");
    assert!(!events[0].payload.to_string().contains("tail-marker"));
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn workflow_agents_cannot_propose_project_memory_write() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-1");
    let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    store.write_metadata(&state).unwrap();
    let app = router(ApiState::new(store));

    let response = post_json(
        app,
        "/api/v3/memory/project/propose-write",
        json!({
            "run_id": "run-1",
            "proposed_by_role": "task_execution",
            "record": {
                "id": "mem_executor_proposal",
                "scope": "project",
                "key": "blocked-proposal",
                "content": "Executor should not propose durable project memory.",
                "tags": [],
                "source_ref": "memory://project/blocked-proposal"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn project_memory_confirm_write_persists_and_records_summary_event() {
    let repo = temp_root();
    let store_root = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-1");
    let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    store.write_metadata(&state).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/memory/project/confirm-write",
        json!({
            "repo_root": repo.display().to_string(),
            "memory_path": "memory.json",
            "run_id": "run-1",
            "confirmed_by_role": "planning_chat",
            "record": {
                "id": "mem_3",
                "scope": "project",
                "key": "rust-api",
                "content": "Rust API v3 is the primary product path.",
                "tags": ["rust"],
                "evidence_refs": [{"kind": "doc", "reference": "docs/ARCHITECTURE.md"}],
                "source_ref": "memory://project/rust-api"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["record_count"], 1);
    assert_eq!(body["event_recorded"], true);
    assert_eq!(body["event"]["kind"], "memory.write.confirmed");
    let persisted = fs::read_to_string(repo.join("memory.json")).unwrap();
    assert!(persisted.contains("Rust API v3 is the primary product path."));
    let events = store.read_events(&run_id).unwrap();
    assert_eq!(events[0].kind, "memory.write.confirmed");
    assert!(!events[0]
        .payload
        .to_string()
        .contains("primary product path"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn workflow_agents_cannot_confirm_project_memory_write() {
    let repo = temp_root();
    let store_root = temp_root();
    for role in ["workflow_supervisor", "task_execution"] {
        fs::create_dir_all(&repo).unwrap();
        let store = RunStore::new(&store_root);
        let run_id = RunId::from_string(format!("run-{role}"));
        let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
        store.write_metadata(&state).unwrap();
        let app = router(ApiState::new(store));

        let response = post_json(
            app,
            "/api/v3/memory/project/confirm-write",
            json!({
                "repo_root": repo.display().to_string(),
                "memory_path": "memory.json",
                "run_id": run_id.as_str(),
                "confirmed_by_role": role,
                "record": {
                    "id": format!("mem_{role}"),
                    "scope": "project",
                    "key": "blocked",
                    "content": "Workflow agents should not directly persist this.",
                    "tags": [],
                    "source_ref": "memory://project/blocked"
                }
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert!(!repo.join("memory.json").exists());
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn knowledge_endpoints_import_list_chunks_and_retrieve_hints() {
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    let app = test_router();

    let import_response = post_json(
        app.clone(),
        "/api/v3/knowledge-sources/import-text",
        json!({
            "repo_root": repo.display().to_string(),
            "title": "Rust migration notes",
            "text": "# Workflow\n\nRust workflow evidence lives in crates/coder-server/src/lib.rs.",
            "tags": ["rust"],
            "allowed_agents": ["workflow_supervisor"],
            "purpose": ["project_rules"],
            "allowed_contexts": ["planner_order"],
            "sensitivity": "project"
        }),
    )
    .await;

    assert_eq!(import_response.status(), StatusCode::OK);
    let import_body = response_json(import_response).await;
    assert_eq!(import_body["index_dirty"], true);
    assert_eq!(import_body["chunks"].as_array().unwrap().len(), 1);
    let source_id = import_body["source"]["source_id"].as_str().unwrap();
    assert!(repo
        .join(".coder")
        .join("memory")
        .join("knowledge_sources.jsonl")
        .exists());

    let repo_query = percent_encode(&repo.display().to_string());
    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v3/knowledge-sources?repo_root={repo_query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = response_json(list_response).await;
    assert_eq!(list_body["sources"][0]["source_id"], source_id);

    let chunks_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v3/knowledge-sources/{source_id}/chunks?repo_root={repo_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(chunks_response.status(), StatusCode::OK);
    let chunks_body = response_json(chunks_response).await;
    assert_eq!(chunks_body["chunks"][0]["title"], "Workflow");

    let retrieve_response = post_json(
        app.clone(),
        "/api/v3/knowledge/retrieve",
        json!({
            "repo_root": repo.display().to_string(),
            "role": "workflow_supervisor",
            "query": "workflow evidence",
            "requested_context": "planner_order",
            "tags": ["rust"],
            "include_content": false
        }),
    )
    .await;
    assert_eq!(retrieve_response.status(), StatusCode::OK);
    let retrieve_body = response_json(retrieve_response).await;
    assert_eq!(
        retrieve_body["results"][0]["evidence_kind"],
        "knowledge_hint"
    );
    assert_eq!(
        retrieve_body["results"][0]["requires_repo_verification"],
        true
    );
    assert_eq!(retrieve_body["results"][0]["backend"], "lexical");
    assert_eq!(retrieve_body["hits"][0]["backend"], "lexical");
    assert_eq!(retrieve_body["hits"][0]["source_id"], source_id);
    assert_eq!(retrieve_body["results"][0]["content_preview"], Value::Null);

    let dense_response = post_json(
        app.clone(),
        "/api/v3/knowledge/retrieve",
        json!({
            "repo_root": repo.display().to_string(),
            "role": "workflow_supervisor",
            "query": "workflow evidence coder server",
            "requested_context": "planner_order",
            "backend": "dense_mock",
            "scope": "project",
            "top_k": 5,
            "include_content": true
        }),
    )
    .await;
    let dense_body = response_json(dense_response).await;
    assert_eq!(dense_body["results"][0]["backend"], "dense_mock");
    assert_eq!(dense_body["hits"][0]["backend"], "dense_mock");
    assert!(dense_body["hits"][0]["evidence_ref"]
        .as_str()
        .unwrap()
        .starts_with("knowledge://"));

    let denied_response = post_json(
        app,
        "/api/v3/knowledge/retrieve",
        json!({
            "repo_root": repo.display().to_string(),
            "role": "task_execution",
            "query": "workflow evidence",
            "requested_context": "execution_prompt",
            "include_content": true
        }),
    )
    .await;
    let denied_body = response_json(denied_response).await;
    assert!(denied_body["results"].as_array().unwrap().is_empty());
    let _ = fs::remove_dir_all(repo);
}

#[tokio::test]
async fn config_validate_endpoint_returns_report() {
    let app = test_router();
    let response = post_json(
        app,
        "/api/v3/config/validate",
        json!({"config": example_config()}),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "pass");
}

mod mcp_api;

#[tokio::test]
async fn extension_plugins_endpoint_lists_builtin_manifests() {
    let app = test_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/extensions/plugins")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let plugin_ids = body["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|plugin| plugin["id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(plugin_ids.contains("command-runner"));
    assert!(plugin_ids.contains("filesystem-patch"));
    assert_eq!(plugin_ids.len(), 2);
}

#[tokio::test]
async fn extension_plugin_validate_endpoint_rejects_external_effect_without_preview() {
    let app = test_router();
    let response = post_json(
        app,
        "/api/v3/extensions/plugins/validate",
        json!({
            "manifest": {
                "id": "unsafe",
                "name": "Unsafe",
                "operations": ["publish"],
                "external_effect": true,
                "requires_preview": false
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["ok"], false);
    assert!(body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| error == "external_effect plugins must require preview"));
}

#[tokio::test]
async fn skill_manifest_validate_endpoint_rejects_unsafe_manifest() {
    let app = test_router();
    let response = post_json(
        app,
        "/api/v3/extensions/skills/validate",
        json!({
            "manifest": {
                "id": "unsafe-skill",
                "name": "Unsafe Skill",
                "version": "0.1.0",
                "description": "Runs externally.",
                "category": "coding",
                "publisher": "local",
                "external_effect": true,
                "requires_preview": false,
                "requires_human_approval": false
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["ok"], false);
    assert!(body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| error == "external_effect skills must require preview"));
}

#[tokio::test]
async fn skill_lifecycle_endpoints_cover_ui_baseline() {
    let app = test_router();
    let initial = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/skills/installed")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let initial_body = response_json(initial).await;
    assert!(initial_body["skills"].as_array().unwrap().is_empty());

    let discover = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/skills/discover?registry_url=builtin%3A%2F%2Fskills")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let discover_body = response_json(discover).await;
    assert_eq!(discover_body["skills"][0]["installed"], false);

    let install = post_json(
        app.clone(),
        "/api/v3/skills/install",
        json!({"skill_id": "coder.repo-review", "registry_url": "builtin://skills"}),
    )
    .await;
    assert_eq!(install.status(), StatusCode::OK);
    let install_body = response_json(install).await;
    assert_eq!(install_body["status"], "installed");
    assert_eq!(install_body["skill"]["enabled"], true);

    let disable = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/skills/coder.repo-review/disable")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let disable_body = response_json(disable).await;
    assert_eq!(disable_body["skill"]["enabled"], false);

    let enable = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/skills/coder.repo-review/enable")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let enable_body = response_json(enable).await;
    assert_eq!(enable_body["skill"]["enabled"], true);

    let pin = post_json(
        app.clone(),
        "/api/v3/skills/coder.repo-review/pin",
        json!({}),
    )
    .await;
    let pin_body = response_json(pin).await;
    assert_eq!(pin_body["status"], "pinned");

    let updates = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/skills/updates?registry_url=builtin%3A%2F%2Fskills")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let updates_body = response_json(updates).await;
    assert_eq!(updates_body["updates"][0]["skill_id"], "coder.repo-review");
    assert_eq!(updates_body["updates"][0]["pinned_version"], "0.1.0");

    let unpin = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/skills/coder.repo-review/unpin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let unpin_body = response_json(unpin).await;
    assert_eq!(unpin_body["status"], "unpinned");

    let policy = post_json(
        app.clone(),
        "/api/v3/skills/coder.repo-review/update-policy",
        json!({"update_policy": "auto_official_low_risk"}),
    )
    .await;
    let policy_body = response_json(policy).await;
    assert_eq!(policy_body["status"], "update_policy_set");

    let rollback = post_json(
        app.clone(),
        "/api/v3/skills/coder.repo-review/rollback",
        json!({}),
    )
    .await;
    let rollback_body = response_json(rollback).await;
    assert_eq!(rollback_body["status"], "no_history");

    let search = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/extensions/search?q=repo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let search_body = response_json(search).await;
    assert!(search_body["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|extension| extension["extension_type"] == "skill"));

    let remove = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v3/skills/coder.repo-review")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let remove_body = response_json(remove).await;
    assert_eq!(remove_body["deleted"], true);

    let developer_import = post_json(
        app,
        "/api/v3/skills/developer-import",
        json!({"path": "C:/unsafe"}),
    )
    .await;
    assert_eq!(developer_import.status(), StatusCode::FORBIDDEN);
}

mod provider_runtime;

#[tokio::test]
async fn run_list_endpoint_returns_empty_store() {
    let app = test_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["runs"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn run_preview_is_side_effect_free_and_reports_ready() {
    let root = temp_root();
    let app = router(ApiState::new(RunStore::new(&root)));
    let response = post_json(
        app,
        "/api/v3/runs/preview",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "summarize the repo"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "ready");
    assert_eq!(body["requires_confirmation"], true);
    assert_eq!(body["issues"].as_array().unwrap().len(), 0);
    assert!(body["backends"]
        .as_array()
        .unwrap()
        .iter()
        .any(|backend| backend.as_str() == Some("native-rust")));
    assert!(!root.join("runs").exists());
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn run_preview_blocks_missing_workflow_and_empty_task() {
    let app = test_router();
    let response = post_json(
        app,
        "/api/v3/runs/preview",
        json!({
            "config": example_config(),
            "workflow_id": "missing",
            "task": "  "
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert_eq!(body["requires_confirmation"], false);
    let codes = body["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["code"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"workflow_not_found"));
    assert!(codes.contains(&"task_empty"));
}

#[tokio::test]
async fn run_endpoint_uses_workflow_runner_and_plan_context() {
    let root = temp_root();
    fs::create_dir_all(&root).unwrap();
    let store_root = temp_root();
    let mut config: ProjectConfig =
        serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
    for harness in config.harnesses.values_mut() {
        harness.backend = "native-rust".to_owned();
        harness.tools.clear();
        harness.memory.read = vec![ConfigMemoryScope::Workflow, ConfigMemoryScope::Run];
        harness.memory.write = vec![ConfigMemoryScope::Run];
    }
    config.surface_bindings.planner_chat = None;
    let app = router(ApiState::new(RunStore::new(&store_root)));

    let response = post_json(
        app,
        "/api/v3/runs",
        json!({
            "config": config,
            "workflow_id": "planner-led",
            "task": "Inspect project scope acceptance: evidence report exists",
            "repo_root": root.display().to_string(),
            "plan_context": {
                "original_user_request": "Inspect project scope",
                "planner_conversation_summary": "Ready to inspect project scope.",
                "plan_draft": {
                    "goal": "Inspect project scope",
                    "scope": ["."],
                    "non_goals": [],
                    "assumptions": [],
                    "steps": ["Inspect", "Report"],
                    "affected_paths": ["."],
                    "acceptance_criteria": ["evidence report exists"],
                    "risks": [],
                    "open_questions": [],
                    "selected_workflow_id": "planner-led"
                },
                "acceptance_criteria": ["evidence report exists"],
                "risks": [],
                "affected_paths": ["."],
                "selected_workflow_id": "planner-led"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert!(body["run_id"].as_str().unwrap().len() > 8);
    assert!(!body["report"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check.as_str() == Some("acceptance: evidence report exists")));
    assert!(body["report_ref"]
        .as_str()
        .unwrap()
        .ends_with("/final-report.json"));
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(store_root);
}

mod command_tools;

mod subagent_endpoints;

#[tokio::test]
async fn patch_preview_endpoint_summarizes_patch_without_writing_store() {
    let root = temp_root();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    fs::write(
        root.join("change.patch"),
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
    let app = test_router();

    let response = post_json(
        app,
        "/api/v3/tools/patch/preview",
        json!({
            "repo_root": root.display().to_string(),
            "patch_file": "change.patch"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["file_count"], 1);
    assert_eq!(body["files"][0]["new_path"], "tracked.txt");
    assert!(!root.join("runs").exists());
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn patch_apply_endpoint_requires_run_id_and_records_approval() {
    let repo = temp_root();
    let store_root = temp_root();
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
    let app = router(ApiState::new(RunStore::new(&store_root)));

    let missing_run_response = post_json(
        app.clone(),
        "/api/v3/tools/patch/apply",
        json!({
            "repo_root": repo.display().to_string(),
            "patch_file": "change.patch",
            "source": "model"
        }),
    )
    .await;
    assert_eq!(missing_run_response.status(), StatusCode::BAD_REQUEST);

    let apply_response = post_json(
        app.clone(),
        "/api/v3/tools/patch/apply",
        json!({
            "repo_root": repo.display().to_string(),
            "patch_file": "change.patch",
            "source": "model",
            "approved": false,
            "run_id": "run-1"
        }),
    )
    .await;
    assert_eq!(apply_response.status(), StatusCode::OK);
    let apply_body = response_json(apply_response).await;
    assert_eq!(apply_body["result"]["status"], "blocked");
    assert!(apply_body["evidence_ref"]["ref_id"]
        .as_str()
        .unwrap()
        .starts_with("repo-diff:"));

    let report_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/report/preview")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let report_body = response_json(report_response).await;
    assert_eq!(report_body["report"]["status"], "blocked");
    assert_eq!(report_body["report"]["changed_files"][0], "tracked.txt");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn run_report_preview_and_write_are_evidence_backed() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                1,
                "command.completed",
                json!({
                    "command": "cargo test",
                    "status": "completed",
                    "passed": true,
                    "returncode": 0
                }),
            ),
        )
        .unwrap();
    let app = router(ApiState::new(store));

    let preview_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/report/preview")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(preview_response.status(), StatusCode::OK);
    let preview_body = response_json(preview_response).await;
    assert_eq!(preview_body["report_ref"], Value::Null);
    assert_eq!(preview_body["report"]["status"], "completed");
    assert!(preview_body["report"]["checks"][0]
        .as_str()
        .unwrap()
        .contains("cargo test"));

    let write_response = app
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
    assert_eq!(write_response.status(), StatusCode::OK);
    let write_body = response_json(write_response).await;
    assert!(write_body["report_ref"]
        .as_str()
        .unwrap()
        .ends_with("/final-report.json"));

    let detail_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let detail_body = response_json(detail_response).await;
    assert_eq!(
        detail_body["report"]["checks"][0],
        "cargo test: completed exit 0"
    );
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn run_verification_evidence_endpoint_records_completed_browser_result() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    store.write_metadata(&state).unwrap();
    let app = router(ApiState::new(store));

    let response = post_json(
        app.clone(),
        "/api/v3/runs/run-1/verification/evidence",
        json!({
            "status": "ok",
            "source": "playwright",
            "summary": "browser gameplay passed",
            "evidence": {
                "browser": "msedge",
                "checks": {
                    "canvas_visible": true,
                    "direction_key_moves_game": true,
                    "restart_works": true
                }
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(body["event_count"], 1);
    assert!(body["evidence_ref"]
        .as_str()
        .unwrap()
        .starts_with("blob://sha256/"));
    assert_eq!(body["report"]["status"], "completed");
    assert!(body["report"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check.as_str() == Some("verification: browser gameplay passed")));
    assert!(body["report"]["evidence_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| {
            reference["kind"] == "verification_evidence"
                && reference["reference"]
                    .as_str()
                    .unwrap()
                    .starts_with("blob://sha256/")
        }));

    let events_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let events_body = response_json(events_response).await;
    assert_eq!(events_body["events"][0]["kind"], "verification.completed");
    assert_eq!(
        events_body["events"][0]["refs"][0]["label"],
        "verification_evidence"
    );
    assert_eq!(events_body["events"][0]["payload"]["source"], "playwright");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn run_verification_evidence_endpoint_records_failed_browser_result() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    store.write_metadata(&state).unwrap();
    let app = router(ApiState::new(store));

    let response = post_json(
        app.clone(),
        "/api/v3/runs/run-1/verification/evidence",
        json!({
            "status": "failed",
            "source": "playwright",
            "summary": "browser gameplay failed",
            "reason": "Browser console reported errors.",
            "remaining_work": ["Fix the client-side exception."],
            "evidence": {
                "console_messages": ["pageerror: boom"]
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "failed");
    assert_eq!(body["report"]["status"], "failed");
    assert!(body["report"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check
            .as_str()
            .unwrap()
            .contains("verification: failed - Browser console reported errors.")));
    assert!(body["report"]["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|blocker| blocker
            .as_str()
            .unwrap()
            .contains("Verification failed: Browser console reported errors.")));

    let events_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let events_body = response_json(events_response).await;
    assert_eq!(events_body["events"][0]["kind"], "verification.failed");
    assert_eq!(
        events_body["events"][0]["payload"]["remaining_work"][0],
        "Fix the client-side exception."
    );
    assert_eq!(
        events_body["events"][0]["refs"][0]["label"],
        "verification_evidence"
    );
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn run_control_endpoints_record_events_and_cancel_report() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    state.status = RunStatus::Running;
    store.write_metadata(&state).unwrap();
    store
        .append_event(
            &run_id,
            &coder_events::CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
        )
        .unwrap();
    store
        .append_run_content_replacement_record_next(
            &run_id,
            vec![coder_store::ContentReplacementRecord {
                kind: "tool-result".to_owned(),
                tool_use_id: "toolu-resume-1".to_owned(),
                replacement: "<persisted-output>cached result</persisted-output>".to_owned(),
            }],
        )
        .unwrap();
    let api_state = ApiState::new(store);
    let (control_sender, _control_receiver) =
        tokio::sync::watch::channel(WorkflowRunControl::Running);
    api_state
        .active_run_controls
        .lock()
        .unwrap()
        .insert(run_id.to_string(), control_sender);
    let app = router(api_state.clone());

    let heartbeat_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/heartbeat")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(heartbeat_response.status(), StatusCode::OK);
    let heartbeat_body = response_json(heartbeat_response).await;
    assert_eq!(heartbeat_body["status"], "running");
    assert_eq!(heartbeat_body["event_count"], 1);

    let pause_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/runs/run-1/pause")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let pause_body = response_json(pause_response).await;
    assert_eq!(pause_body["status"], "running");
    assert_eq!(pause_body["control_state"], "paused");
    assert_eq!(pause_body["event_count"], 2);

    let resume_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/runs/run-1/resume")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let resume_body = response_json(resume_response).await;
    assert_eq!(resume_body["status"], "running");
    assert_eq!(resume_body["control_state"], "running");
    assert_eq!(resume_body["event_count"], 3);
    assert_eq!(
        resume_body["content_replacement_replay"]["contract"],
        "coder.content_replacement_replay.v1"
    );
    assert_eq!(
        resume_body["content_replacement_replay"]["policy"],
        "resume_tail_replay"
    );
    assert_eq!(resume_body["content_replacement_replay"]["record_count"], 1);
    assert_eq!(
        resume_body["content_replacement_replay"]["replacement_count"],
        1
    );
    assert_eq!(
        resume_body["content_replacement_replay"]["records"][0]["replacements"][0]["toolUseId"],
        "toolu-resume-1"
    );

    api_state
        .active_run_controls
        .lock()
        .unwrap()
        .remove(run_id.as_str());

    let cancel_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/runs/run-1/cancel")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cancel_body = response_json(cancel_response).await;
    assert_eq!(cancel_body["status"], "cancelled");
    assert_eq!(cancel_body["control_state"], "cancelled");
    assert_eq!(cancel_body["event_count"], 4);
    assert!(cancel_body["report_ref"]
        .as_str()
        .unwrap()
        .ends_with("/final-report.json"));

    let detail_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let detail_body = response_json(detail_response).await;
    assert_eq!(detail_body["metadata"]["status"], "cancelled");
    assert_eq!(detail_body["report"]["status"], "cancelled");
    assert_eq!(
        detail_body["events"][2]["payload"]["content_replacement_replay"]["records_url"],
        "/api/v3/runs/run-1/content-replacements"
    );
    assert_eq!(
        detail_body["events"][2]["payload"]["content_replacement_replay"]["record_count"],
        1
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn native_executor_reuses_changed_file_evidence_from_previous_workflow_rounds() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-prior-round-files");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "backend.native_rust.completed",
                json!({"changed_files": ["index.html", "main.js", "style.css"]}),
            ),
        )
        .unwrap();

    let files =
        native_model_backend::recorded_run_changed_files(&ApiState::new(store), &run_id).unwrap();

    assert_eq!(files, vec!["index.html", "main.js", "style.css"]);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn mock_run_endpoint_writes_events_visible_through_events_endpoint() {
    let root = temp_root();
    let app = router(ApiState::new(RunStore::new(&root)));
    let response = post_json(
        app.clone(),
        "/api/v3/runs/mock",
        json!({
            "config": example_config(),
            "workflow_id": "planner-led",
            "task": "summarize the repo"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let events_url = body["events_url"].as_str().unwrap();
    let run_id = body["run_id"].as_str().unwrap();

    let events_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(events_url)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(events_response.status(), StatusCode::OK);
    let events_body = response_json(events_response).await;
    assert_eq!(events_body["events"][0]["kind"], "run.started");

    let detail_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v3/runs/{run_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(detail_response.status(), StatusCode::OK);
    let detail_body = response_json(detail_response).await;
    assert_eq!(detail_body["metadata"]["status"], "completed");
    assert_eq!(detail_body["report"]["status"], "completed");
    assert_eq!(
        detail_body["report"]["evidence_refs"][0]["kind"],
        "event_log"
    );

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = response_json(list_response).await;
    assert_eq!(list_body["runs"].as_array().unwrap().len(), 1);
    assert_eq!(list_body["runs"][0]["run_id"], run_id);
    assert_eq!(list_body["runs"][0]["metadata"]["status"], "completed");
    assert!(list_body["runs"][0]["event_count"].as_u64().unwrap() >= 1);
    assert_eq!(list_body["runs"][0]["has_report"], true);

    let artifact_response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v3/runs/{run_id}/artifacts/final-report.json"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(artifact_response.status(), StatusCode::OK);
    let artifact_body = response_json(artifact_response).await;
    assert_eq!(artifact_body["artifact_name"], "final-report.json");
    assert_eq!(artifact_body["payload"]["status"], "completed");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn run_events_endpoint_supports_incremental_and_tail_pages() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-events-page");
    store
        .write_metadata(&RunState::new(
            run_id.clone(),
            coder_core::WorkflowId::new("workflow"),
        ))
        .unwrap();
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
    let app = router(ApiState::new(store));

    let incremental_response = get_json(
        app.clone(),
        "/api/v3/runs/run-events-page/events?after_sequence=2&limit=2",
    )
    .await;
    assert_eq!(incremental_response.status(), StatusCode::OK);
    let incremental_body = response_json(incremental_response).await;
    assert_eq!(incremental_body["event_count"], 5);
    assert_eq!(incremental_body["returned_count"], 2);
    assert_eq!(incremental_body["truncated"], true);
    assert_eq!(incremental_body["next_after_sequence"], 4);
    assert_eq!(incremental_body["events"][0]["sequence"], 3);
    assert_eq!(incremental_body["events"][1]["sequence"], 4);

    let tail_response = get_json(
        app.clone(),
        "/api/v3/runs/run-events-page/events?tail=true&limit=2",
    )
    .await;
    assert_eq!(tail_response.status(), StatusCode::OK);
    let tail_body = response_json(tail_response).await;
    assert_eq!(tail_body["event_count"], 5);
    assert_eq!(tail_body["returned_count"], 2);
    assert_eq!(tail_body["truncated"], true);
    assert_eq!(tail_body["next_after_sequence"], 5);
    assert_eq!(tail_body["events"][0]["sequence"], 4);
    assert_eq!(tail_body["events"][1]["sequence"], 5);

    let detail_response = get_json(
        app.clone(),
        "/api/v3/runs/run-events-page?include_events=false",
    )
    .await;
    assert_eq!(detail_response.status(), StatusCode::OK);
    let detail_body = response_json(detail_response).await;
    assert_eq!(detail_body["event_count"], 5);
    assert_eq!(detail_body["returned_count"], 0);
    assert_eq!(detail_body["events"].as_array().unwrap().len(), 0);

    let timeline_response = get_json(
        app.clone(),
        "/api/v3/runs/run-events-page/timeline?tail=true&limit=2",
    )
    .await;
    assert_eq!(timeline_response.status(), StatusCode::OK);
    let timeline_body = response_json(timeline_response).await;
    assert_eq!(timeline_body["event_count"], 5);
    assert_eq!(timeline_body["returned_count"], 2);
    assert_eq!(timeline_body["truncated"], true);
    assert_eq!(timeline_body["next_after_sequence"], 5);

    let invalid_response = get_json(app, "/api/v3/runs/run-events-page/events?limit=1001").await;
    assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn run_content_replacements_endpoint_supports_incremental_and_tail_pages() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-content-replacements-page");
    store
        .write_metadata(&RunState::new(
            run_id.clone(),
            coder_core::WorkflowId::new("workflow"),
        ))
        .unwrap();
    for sequence in 1..=3 {
        store
            .append_run_content_replacement_record_next(
                &run_id,
                vec![coder_store::ContentReplacementRecord {
                    kind: "tool-result".to_owned(),
                    tool_use_id: format!("toolu-{sequence}"),
                    replacement: format!(
                        "<persisted-output>replacement {sequence}</persisted-output>"
                    ),
                }],
            )
            .unwrap();
    }
    let app = router(ApiState::new(store));

    let incremental_response = get_json(
        app.clone(),
        "/api/v3/runs/run-content-replacements-page/content-replacements?after_sequence=1&limit=1",
    )
    .await;
    assert_eq!(incremental_response.status(), StatusCode::OK);
    let incremental_body = response_json(incremental_response).await;
    assert_eq!(
        incremental_body["contract"],
        "coder.content_replacement_replay.v1"
    );
    assert_eq!(incremental_body["policy"], "incremental_page");
    assert_eq!(incremental_body["record_count"], 3);
    assert_eq!(incremental_body["returned_count"], 1);
    assert_eq!(incremental_body["replacement_count"], 1);
    assert_eq!(incremental_body["truncated"], true);
    assert_eq!(incremental_body["next_after_sequence"], 2);
    assert_eq!(incremental_body["records"][0]["sequence"], 2);
    assert_eq!(
        incremental_body["records"][0]["replacements"][0]["toolUseId"],
        "toolu-2"
    );

    let tail_response = get_json(
        app.clone(),
        "/api/v3/runs/run-content-replacements-page/content-replacements?tail=true&limit=2",
    )
    .await;
    assert_eq!(tail_response.status(), StatusCode::OK);
    let tail_body = response_json(tail_response).await;
    assert_eq!(tail_body["policy"], "tail_page");
    assert_eq!(tail_body["record_count"], 3);
    assert_eq!(tail_body["returned_count"], 2);
    assert_eq!(tail_body["truncated"], true);
    assert_eq!(tail_body["next_after_sequence"], 3);
    assert_eq!(tail_body["records"][0]["sequence"], 2);
    assert_eq!(tail_body["records"][1]["sequence"], 3);

    let invalid_response = get_json(
        app,
        "/api/v3/runs/run-content-replacements-page/content-replacements?limit=1001",
    )
    .await;
    assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn repo_evidence_endpoint_returns_payload_by_ref() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let reference = store
        .write_repo_evidence(
            &RunId::from_string("run-1"),
            coder_store::RepoEvidenceKind::RepoRead,
            "repo",
            Vec::new(),
            "Read src/app.py.",
            json!({
                "evidence_kind": "repo_evidence",
                "operation": "read_file_range",
                "snippet": {"path": "src/app.py", "text": "safe"}
            }),
        )
        .unwrap();
    let app = router(ApiState::new(store));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v3/repo-evidence/{}", reference.ref_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["ref_id"], reference.ref_id);
    assert_eq!(body["payload"]["operation"], "read_file_range");
    assert_eq!(body["payload"]["snippet"]["path"], "src/app.py");

    let detail_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(detail_response.status(), StatusCode::OK);
    let detail_body = response_json(detail_response).await;
    assert_eq!(detail_body["run_id"], "run-1");
    assert_eq!(detail_body["repo_evidence_count"], 1);
    assert_eq!(detail_body["metadata"], Value::Null);

    let list_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/repo-evidence")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = response_json(list_response).await;
    assert_eq!(list_body["run_id"], "run-1");
    assert_eq!(list_body["evidence"][0]["ref_id"], reference.ref_id);
    assert_eq!(list_body["evidence"][0]["summary"], "Read src/app.py.");
    let _ = fs::remove_dir_all(root);
}

mod model_tool_core;

#[tokio::test]
async fn run_async_notification_drain_delivers_main_thread_only() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-async-notification-drain");
    store
        .write_run_config_snapshot(&run_id, &default_project_config())
        .unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
        )
        .unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                2,
                "model_tool.async_rewake.notification",
                json!({
                    "contract": "coder.model_tool_async_rewake.v1",
                    "source": "coder-server",
                    "async_hook_id": "main-hook",
                    "hook_event": "PreToolUse",
                    "tool_name": "repo_read_file",
                    "tool_use_id": "toolu-main",
                    "agent_id": Value::Null,
                    "agentId": Value::Null,
                    "mode": "task-notification",
                    "priority": "later",
                    "delivery_status": "recorded_not_delivered",
                    "message": "main thread result"
                }),
            ),
        )
        .unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                3,
                "model_tool.async_rewake.notification",
                json!({
                    "contract": "coder.model_tool_async_rewake.v1",
                    "source": "coder-server",
                    "async_hook_id": "child-hook",
                    "hook_event": "PreToolUse",
                    "tool_name": "repo_read_file",
                    "tool_use_id": "toolu-child",
                    "agent_id": "agent-child",
                    "agentId": "agent-child",
                    "mode": "task-notification",
                    "priority": "later",
                    "delivery_status": "recorded_not_delivered",
                    "message": "subagent result"
                }),
            ),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let drain_response = post_json(
        app.clone(),
        "/api/v3/runs/run-async-notification-drain/async-notifications/drain",
        json!({}),
    )
    .await;
    assert_eq!(drain_response.status(), StatusCode::OK);
    let drain_body = response_json(drain_response).await;
    assert_eq!(
        drain_body["contract"],
        "coder.run_async_notification_drain.v1"
    );
    assert_eq!(
        drain_body["policy"],
        "main_thread_idle_queue_task_notification_batch"
    );
    assert_eq!(drain_body["delivery_channel"], "idle_queue_processor");
    assert_eq!(drain_body["mode"], "task-notification");
    assert_eq!(drain_body["processed"], true);
    assert_eq!(drain_body["notification_count"], 2);
    assert_eq!(drain_body["returned_count"], 1);
    assert_eq!(drain_body["next_after_sequence"].as_u64(), Some(2));
    let attachments = drain_body["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["source_uuid"], "main-hook");
    assert_eq!(attachments[0]["commandMode"], "task-notification");
    assert_eq!(attachments[0]["notification_sequence"].as_u64(), Some(2));
    assert_eq!(attachments[0]["agent_id"], Value::Null);
    assert!(attachments[0]["prompt"]
        .as_str()
        .unwrap()
        .contains("main thread result"));

    let delivered_events = store.read_events(&run_id).unwrap();
    assert!(delivered_events.iter().any(|event| {
        event.kind == "model_tool.async_rewake.delivered"
            && event.payload["notification_sequence"].as_u64() == Some(2)
            && event.payload["delivery_channel"].as_str() == Some("idle_queue_processor")
            && event.payload["drain_agent_id"].is_null()
    }));
    assert!(!delivered_events.iter().any(|event| {
        event.kind == "model_tool.async_rewake.delivered"
            && event.payload["notification_sequence"].as_u64() == Some(3)
    }));

    let repeated_drain_response = post_json(
        app.clone(),
        "/api/v3/runs/run-async-notification-drain/async-notifications/drain",
        json!({}),
    )
    .await;
    assert_eq!(repeated_drain_response.status(), StatusCode::OK);
    let repeated_drain_body = response_json(repeated_drain_response).await;
    assert_eq!(repeated_drain_body["processed"], false);
    assert_eq!(
        repeated_drain_body["delivery_status"],
        "no_main_thread_notifications"
    );
    assert_eq!(repeated_drain_body["returned_count"], 0);

    let child_turn_response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-async-notification-drain",
            "harness_id": "native-code-edit",
            "agent_id": "agent-child",
            "tool_uses": [
                {
                    "id": "toolu-child-sleep",
                    "name": "sleep",
                    "input": {
                        "duration_ms": 0
                    }
                }
            ]
        }),
    )
    .await;
    assert_eq!(child_turn_response.status(), StatusCode::OK);
    let child_turn_body = response_json(child_turn_response).await;
    let child_attachments = child_turn_body["attachments"].as_array().unwrap();
    assert_eq!(child_attachments.len(), 1);
    assert_eq!(child_attachments[0]["source_uuid"], "child-hook");
    assert_eq!(child_attachments[0]["agent_id"], "agent-child");
    assert!(child_attachments[0]["prompt"]
        .as_str()
        .unwrap()
        .contains("subagent result"));

    let final_events = store.read_events(&run_id).unwrap();
    assert!(final_events.iter().any(|event| {
        event.kind == "model_tool.async_rewake.delivered"
            && event.payload["notification_sequence"].as_u64() == Some(3)
            && event.payload["delivery_channel"].as_str() == Some("model_tool_turn_attachment")
            && event.payload["drain_agent_id"].as_str() == Some("agent-child")
    }));

    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn run_transcript_compaction_uses_model_summary_and_records_circuit() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-transcript-compact");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({
                    "task": "Improve planner harness",
                    "repo_root": "F:/bbb/coder"
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
                "tool.completed",
                json!({
                    "tool": "repo_read_file",
                    "path": "crates/coder-server/src/lib.rs",
                    "summary": "read provider code"
                }),
            ),
        )
        .unwrap();
    store
        .append_run_content_replacement_record_next(
            &run_id,
            vec![coder_store::ContentReplacementRecord {
                kind: "tool-result".to_owned(),
                tool_use_id: "toolu-large-output".to_owned(),
                replacement: "<persisted-output>cached compaction replay marker</persisted-output>"
                    .to_owned(),
            }],
        )
        .unwrap();
    let (provider_base_url, captured) = spawn_openai_compatible_capture_test_server(json!({
        "choices": [
            {
                "message": {
                    "content": "<analysis>draft details that must not be stored</analysis>\n<summary>1. Primary Request and Intent:\nImprove Coder's planner harness.\n\n9. Optional Next Step:\nContinue compaction wiring.</summary>"
                }
            }
        ]
    }))
    .await;
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "compact-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/runs/run-transcript-compact/transcript/compact",
        json!({
            "custom_instructions": "Keep Claude parameter evidence explicit."
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["contract"], "coder.run_transcript_compaction.v1");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["success"], true);
    assert_eq!(body["provider"], "openai-compatible");
    assert_eq!(body["model"], "compact-model");
    assert!(body["summary_estimated_tokens"].as_u64().unwrap() > 0);
    let summary = body["summary"].as_str().unwrap();
    assert!(summary.starts_with("Summary:\n"));
    assert!(summary.contains("Improve Coder's planner harness"));
    assert!(!summary.contains("draft details"));
    assert_eq!(body["transcript_event_count"], 2);
    assert_eq!(body["transcript_events_included"], 2);
    assert_eq!(body["transcript_truncated"], false);
    assert!(body["artifact_ref"]
        .as_str()
        .unwrap()
        .contains("transcript-compaction-3.json"));
    assert_eq!(body["circuit"]["consecutive_failures"], 0);
    assert_eq!(body["circuit"]["circuit_breaker_open"], false);
    assert_eq!(body["circuit"]["max_consecutive_failures"], 3);

    let captured = captured.lock().unwrap().clone().unwrap();
    assert_eq!(captured["model"], "compact-model");
    assert_eq!(captured["temperature"], 0);
    assert_eq!(captured["max_tokens"], 20_000);
    let prompt = captured["messages"][1]["content"].as_str().unwrap();
    assert!(prompt.starts_with("CRITICAL: Respond with TEXT ONLY."));
    assert!(prompt.contains("<analysis> block followed by a <summary> block"));
    assert!(prompt.contains("Keep Claude parameter evidence explicit."));
    assert!(prompt.contains("[sequence=1; kind=run.started]"));
    assert!(prompt.contains("Improve planner harness"));
    assert!(prompt.contains("[content_replacement_replay]"));
    assert!(prompt.contains("toolu-large-output"));
    assert!(prompt.contains("cached compaction replay marker"));

    let circuit = store
        .read_compaction_circuit_state("run-transcript-run-transcript-compact")
        .unwrap()
        .unwrap();
    assert_eq!(circuit.consecutive_failures, 0);
    assert!(!circuit.circuit_breaker_open);
    let events = store.read_events(&run_id).unwrap();
    let compaction_event = events
        .iter()
        .find(|event| event.kind == "run.transcript_compaction.outcome")
        .unwrap();
    assert_eq!(compaction_event.sequence, 3);
    assert_eq!(compaction_event.payload["success"], true);
    assert_eq!(
        compaction_event.refs[0].label,
        "transcript_summary".to_owned()
    );

    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn run_transcript_compaction_failure_opens_persistent_circuit() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-transcript-compact-fail");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"task": "compact should fail without key"}),
            ),
        )
        .unwrap();
    let state = ApiState::new(store.clone());
    {
        let mut settings = state.provider_settings.lock().unwrap();
        settings.mock_mode = false;
        settings.default_provider = "openai-compatible".to_owned();
        settings.default_model = "compact-model".to_owned();
        settings.base_urls.insert(
            "openai-compatible".to_owned(),
            "http://127.0.0.1:9".to_owned(),
        );
        settings.api_keys.clear();
    }
    let app = router(state);

    let mut last_body = Value::Null;
    for _ in 0..3 {
        let response = post_json(
            app.clone(),
            "/api/v3/runs/run-transcript-compact-fail/transcript/compact",
            json!({}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        last_body = response_json(response).await;
        assert_eq!(last_body["status"], "failed");
        assert_eq!(last_body["success"], false);
        assert!(last_body["error"]
            .as_str()
            .unwrap()
            .contains("requires an API key"));
    }
    assert_eq!(last_body["circuit"]["consecutive_failures"], 3);
    assert_eq!(last_body["circuit"]["circuit_breaker_open"], true);

    let circuit = store
        .read_compaction_circuit_state("run-transcript-run-transcript-compact-fail")
        .unwrap()
        .unwrap();
    assert_eq!(circuit.consecutive_failures, 3);
    assert!(circuit.circuit_breaker_open);

    let circuit_open_response = post_json(
        app,
        "/api/v3/runs/run-transcript-compact-fail/transcript/compact",
        json!({}),
    )
    .await;
    assert_eq!(circuit_open_response.status(), StatusCode::OK);
    let circuit_open_body = response_json(circuit_open_response).await;
    assert_eq!(circuit_open_body["status"], "circuit_open");
    assert_eq!(
        circuit_open_body["error"],
        "Transcript compaction circuit breaker is open."
    );
    let circuit_after_skip = store
        .read_compaction_circuit_state("run-transcript-run-transcript-compact-fail")
        .unwrap()
        .unwrap();
    assert_eq!(circuit_after_skip.consecutive_failures, 3);

    let events = store.read_events(&run_id).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "run.transcript_compaction.outcome")
            .count(),
        4
    );

    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_auto_compacts_large_run_transcript_once() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-auto-transcript-compact");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"task": "large transcript should auto compact"}),
            ),
        )
        .unwrap();
    for index in 0..180 {
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    index + 2,
                    "backend.native.observation",
                    json!({
                        "index": index,
                        "text": "x".repeat(4_000)
                    }),
                ),
            )
            .unwrap();
    }
    let compact_payload = json!({
        "choices": [
            {
                "message": {
                    "content": "<analysis>hidden automatic compaction draft</analysis><summary>1. Primary Request and Intent:\nKeep the large native transcript usable.\n\n8. Current Work:\nA model tool turn triggered automatic compaction.\n\n9. Optional Next Step:\nContinue from the compacted summary.</summary>"
                }
            }
        ]
    });
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        compact_payload.clone(),
        compact_payload,
    ])
    .await;
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "compact-model");
    let app = router(state);

    let first_response = post_json(
        app.clone(),
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-auto-transcript-compact",
            "harness_id": "native-code-edit",
            "tool_uses": [
                {
                    "id": "toolu-auto-compact-sleep",
                    "name": "sleep",
                    "input": {"duration_ms": 0}
                }
            ]
        }),
    )
    .await;
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = response_json(first_response).await;
    let attachments = first_body["attachments"].as_array().unwrap();
    let compaction_attachment = attachments
        .iter()
        .find(|attachment| attachment["type"] == "context_compaction_summary")
        .expect("auto compaction attachment should be present");
    assert_eq!(
        compaction_attachment["contract"],
        "coder.run_transcript_compaction_attachment.v1"
    );
    assert_eq!(compaction_attachment["success"], true);
    assert_eq!(
        compaction_attachment["decision"]["reason"],
        "projected_context_crosses_autocompact_threshold"
    );
    assert!(
        compaction_attachment["decision"]["projected_tokens"]
            .as_u64()
            .unwrap()
            >= compaction_attachment["decision"]["threshold_tokens"]
                .as_u64()
                .unwrap()
    );
    let model_text = compaction_attachment["model_content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(model_text.contains("<system-reminder>"));
    assert!(model_text.contains("Coder automatically compacted the run transcript"));
    assert!(model_text.contains("Keep the large native transcript usable"));
    assert!(!model_text.contains("hidden automatic compaction draft"));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 1);
    assert_eq!(captured_requests[0]["max_tokens"], 20_000);
    assert!(captured_requests[0]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("Automatic model-loop compaction"));

    let second_response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-auto-transcript-compact",
            "harness_id": "native-code-edit",
            "tool_uses": [
                {
                    "id": "toolu-after-boundary-sleep",
                    "name": "sleep",
                    "input": {"duration_ms": 0}
                }
            ]
        }),
    )
    .await;
    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = response_json(second_response).await;
    assert!(!second_body["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .any(|attachment| attachment["type"] == "context_compaction_summary"));
    assert_eq!(captured.lock().unwrap().len(), 1);

    let events = store.read_events(&run_id).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "run.transcript_compaction.outcome")
            .count(),
        1
    );
    let circuit = store
        .read_compaction_circuit_state("run-transcript-run-auto-transcript-compact")
        .unwrap()
        .unwrap();
    assert_eq!(circuit.consecutive_failures, 0);
    assert!(!circuit.circuit_breaker_open);

    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_auto_compaction_restores_recent_files_with_claude_limits() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-auto-transcript-compact-files");
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "stale restored marker\n").unwrap();
    fs::write(repo.join("CURRENT.md"), "current turn marker\n").unwrap();

    let compact_payload = json!({
        "choices": [
            {
                "message": {
                    "content": "<summary>1. Primary Request and Intent:\nKeep context useful.\n\n8. Current Work:\nAutomatic compaction restored recent files.\n\n9. Optional Next Step:\nContinue with restored context.</summary>"
                }
            }
        ]
    });
    let (provider_base_url, captured) =
        spawn_openai_compatible_sequence_capture_test_server(vec![compact_payload]).await;
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "compact-model");
    let app = router(state);

    let previous_read = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-previous-read",
            "tool_name": "repo_read_file",
            "run_id": "run-auto-transcript-compact-files",
            "input": {
                "repo_root": repo,
                "path": "README.md"
            }
        }),
    )
    .await;
    assert_eq!(previous_read.status(), StatusCode::OK);

    fs::write(repo.join("README.md"), "fresh restored marker\n").unwrap();
    let mut sequence = store.event_count(&run_id).unwrap() as u64 + 1;
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                sequence,
                "run.started",
                json!({"task": "large transcript should auto compact and restore files"}),
            ),
        )
        .unwrap();
    sequence += 1;
    for index in 0..180 {
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    sequence + index,
                    "backend.native.observation",
                    json!({
                        "index": index,
                        "text": "x".repeat(4_000)
                    }),
                ),
            )
            .unwrap();
    }

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-auto-transcript-compact-files",
            "harness_id": "native-code-edit",
            "tool_uses": [
                {
                    "id": "toolu-current-read",
                    "name": "repo_read_file",
                    "input": {
                        "repo_root": repo,
                        "path": "CURRENT.md"
                    }
                },
                {
                    "id": "toolu-auto-compact-sleep-files",
                    "name": "sleep",
                    "input": {"duration_ms": 0}
                }
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let compaction_attachment = body["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|attachment| attachment["type"] == "context_compaction_summary")
        .expect("auto compaction attachment should be present");
    let restore = &compaction_attachment["post_compact_file_restore"];
    assert_eq!(restore["contract"], "coder.post_compact_file_restore.v1");
    assert_eq!(restore["max_files"], 5);
    assert_eq!(restore["token_budget"], 50_000);
    assert_eq!(restore["max_tokens_per_file"], 5_000);
    let restored_files = restore["restored_files"].as_array().unwrap();
    assert_eq!(restored_files.len(), 1);
    assert_eq!(restored_files[0]["path"], "README.md");
    assert!(restored_files
        .iter()
        .all(|file| file["path"].as_str() != Some("CURRENT.md")));
    let model_content = compaction_attachment["model_content"].as_array().unwrap();
    assert_eq!(model_content.len(), 2);
    let restore_text = model_content[1]["text"].as_str().unwrap();
    assert!(restore_text.contains("fresh restored marker"));
    assert!(!restore_text.contains("stale restored marker"));
    assert!(!restore_text.contains("current turn marker"));
    assert_eq!(captured.lock().unwrap().len(), 1);

    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_auto_compaction_uses_run_agent_runtime_policy() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-auto-transcript-compact-agent-runtime");
    let mut config = default_project_config();
    let model_id = config.agents.get("executor").unwrap().model.clone();
    let capabilities = &mut config.models.get_mut(&model_id).unwrap().capabilities;
    capabilities.context_window_tokens = Some(32_000);
    capabilities.auto_compact_token_limit = Some(11_000);
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"task": "short transcript should follow executor runtime policy"}),
            ),
        )
        .unwrap();

    let default_decision =
        run_transcript_compaction::run_transcript_auto_compaction_decision(&store, &run_id)
            .unwrap();
    assert_eq!(
        default_decision.runtime_source,
        "default_runtime_no_agent_id"
    );
    assert!(!default_decision.should_compact);

    let compact_payload = json!({
        "choices": [
            {
                "message": {
                    "content": "<summary>1. Primary Request and Intent:\nKeep context useful.\n\n8. Current Work:\nAgent runtime triggered automatic compaction.\n\n9. Optional Next Step:\nContinue with compact context.</summary>"
                }
            }
        ]
    });
    let (provider_base_url, captured) =
        spawn_openai_compatible_sequence_capture_test_server(vec![compact_payload]).await;
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "compact-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-auto-transcript-compact-agent-runtime",
            "harness_id": "native-code-edit",
            "agent_id": "executor",
            "tool_uses": [
                {
                    "id": "toolu-auto-compact-agent-runtime",
                    "name": "sleep",
                    "input": {"duration_ms": 0}
                }
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let compaction_attachment = body["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|attachment| attachment["type"] == "context_compaction_summary")
        .expect("auto compaction attachment should be present");
    let decision = &compaction_attachment["decision"];
    assert_eq!(decision["runtime_source"], "run_config_agent_runtime");
    assert_eq!(decision["runtime_agent_id"], "executor");
    assert_eq!(decision["runtime_context_window_tokens"], 32_000);
    assert_eq!(decision["runtime_auto_compact_token_limit"], 11_000);
    assert_eq!(decision["threshold_tokens"], 11_000);
    assert_eq!(decision["estimated_max_turn_growth_tokens"], 23_000);
    assert_eq!(
        decision["reason"],
        "projected_context_crosses_autocompact_threshold"
    );
    assert_eq!(captured.lock().unwrap().len(), 1);

    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn run_transcript_auto_compaction_infers_latest_node_agent_runtime_policy() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-auto-transcript-compact-latest-node-runtime");
    let mut config = default_project_config();
    let model_id = config.agents.get("executor").unwrap().model.clone();
    let capabilities = &mut config.models.get_mut(&model_id).unwrap().capabilities;
    capabilities.context_window_tokens = Some(32_000);
    capabilities.auto_compact_token_limit = Some(11_000);
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"task": "infer runtime from latest node"}),
            ),
        )
        .unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                2,
                "node.started",
                json!({
                    "node_id": "execute",
                    "agent": "executor",
                    "harness": "native-code-edit"
                }),
            ),
        )
        .unwrap();

    let decision =
        run_transcript_compaction::run_transcript_auto_compaction_decision(&store, &run_id)
            .unwrap();
    assert!(decision.should_compact);
    assert_eq!(decision.runtime_source, "run_config_agent_runtime");
    assert_eq!(decision.runtime_agent_id.as_deref(), Some("executor"));
    assert_eq!(decision.runtime_context_window_tokens, 32_000);
    assert_eq!(decision.runtime_auto_compact_token_limit, Some(11_000));
    assert_eq!(decision.threshold_tokens, 11_000);

    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_auto_compaction_restores_invoked_skills_with_claude_limits() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-auto-transcript-compact-skills");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"task": "large transcript should auto compact and restore skills"}),
            ),
        )
        .unwrap();

    let compact_payload = json!({
        "choices": [
            {
                "message": {
                    "content": "<summary>1. Primary Request and Intent:\nKeep skill context useful.\n\n8. Current Work:\nAutomatic compaction restored invoked skills.\n\n9. Optional Next Step:\nContinue with restored skill instructions.</summary>"
                }
            }
        ]
    });
    let (provider_base_url, captured) =
        spawn_openai_compatible_sequence_capture_test_server(vec![compact_payload]).await;
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "compact-model");
    let app = router(state);

    let invocation = post_json(
        app.clone(),
        "/api/v3/runs/run-auto-transcript-compact-skills/skills/invoked",
        json!({
            "skill_name": "coder.repo-review",
            "skill_path": "builtin://skills/coder.repo-review",
            "content": "skill restore marker\nUse repo evidence before final answers.",
            "agent_id": "agent-child"
        }),
    )
    .await;
    assert_eq!(invocation.status(), StatusCode::OK);
    let invocation_body = response_json(invocation).await;
    assert_eq!(invocation_body["contract"], "coder.invoked_skill.v1");
    assert!(
        invocation_body["content_estimated_tokens"]
            .as_u64()
            .unwrap()
            > 0
    );

    let sequence = store.event_count(&run_id).unwrap() as u64 + 1;
    for index in 0..180 {
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    sequence + index,
                    "backend.native.observation",
                    json!({
                        "index": index,
                        "text": "x".repeat(4_000)
                    }),
                ),
            )
            .unwrap();
    }

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-auto-transcript-compact-skills",
            "harness_id": "native-code-edit",
            "agent_id": "agent-child",
            "tool_uses": [
                {
                    "id": "toolu-auto-compact-sleep-skills",
                    "name": "sleep",
                    "input": {"duration_ms": 0}
                }
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let compaction_attachment = body["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|attachment| attachment["type"] == "context_compaction_summary")
        .expect("auto compaction attachment should be present");
    let restore = &compaction_attachment["post_compact_skill_restore"];
    assert_eq!(restore["contract"], "coder.post_compact_skill_restore.v1");
    assert_eq!(restore["type"], "invoked_skills");
    assert_eq!(restore["token_budget"], 25_000);
    assert_eq!(restore["max_tokens_per_skill"], 5_000);
    let restored_skills = restore["skills"].as_array().unwrap();
    assert_eq!(restored_skills.len(), 1);
    assert_eq!(restored_skills[0]["name"], "coder.repo-review");
    let model_content = compaction_attachment["model_content"].as_array().unwrap();
    assert_eq!(model_content.len(), 2);
    let skill_text = model_content[1]["text"].as_str().unwrap();
    assert!(skill_text.contains("skill restore marker"));
    assert!(skill_text.contains("Use repo evidence before final answers."));
    assert_eq!(captured.lock().unwrap().len(), 1);

    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_auto_compaction_restores_local_skilltool_invocation() {
    let store_root = temp_root();
    let skill_root = temp_root();
    let skill_dir = skill_root.join("local-compact-skill");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: Local Compact Skill
description: Keep local skill instructions alive after compaction.
context: inline
---
# Local Compact Skill

Use ${CLAUDE_SKILL_DIR} during run ${CLAUDE_SESSION_ID}.
Local skill restore marker.
"#,
    )
    .unwrap();

    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-auto-transcript-compact-local-skill");
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"task": "large transcript should restore local SkillTool content"}),
            ),
        )
        .unwrap();

    let compact_payload = json!({
        "choices": [
            {
                "message": {
                    "content": "<summary>1. Primary Request and Intent:\nKeep local SkillTool context useful.\n\n8. Current Work:\nAutomatic compaction restored a local skill loaded through SkillTool.\n\n9. Optional Next Step:\nContinue with restored local skill instructions.</summary>"
                }
            }
        ]
    });
    let (provider_base_url, captured) =
        spawn_openai_compatible_sequence_capture_test_server(vec![compact_payload]).await;
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "compact-model");
    let app = router(state);

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

    let skill_invocation = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-local-compact-skill",
            "tool_name": "Skill",
            "run_id": "run-auto-transcript-compact-local-skill",
            "harness_id": "native-code-edit",
            "agent_id": "agent-local-skill",
            "input": {
                "skill": "local-compact-skill"
            }
        }),
    )
    .await;
    assert_eq!(skill_invocation.status(), StatusCode::OK);
    let skill_body = response_json(skill_invocation).await;
    assert_eq!(skill_body["payload"]["skill_origin"], "local_extra_root");
    assert_eq!(
        skill_body["payload"]["frontmatter"]["description"],
        "Keep local skill instructions alive after compaction."
    );
    let invoked_content = skill_body["payload"]["content"].as_str().unwrap();
    assert!(invoked_content.contains("Base directory for this skill"));
    assert!(invoked_content.contains("run-auto-transcript-compact-local-skill"));
    assert!(!invoked_content.contains("${CLAUDE_SKILL_DIR}"));
    assert!(!invoked_content.contains("description: Keep local skill instructions"));

    let sequence = store.event_count(&run_id).unwrap() as u64 + 1;
    for index in 0..180 {
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    sequence + index,
                    "backend.native.observation",
                    json!({
                        "index": index,
                        "text": "x".repeat(4_000)
                    }),
                ),
            )
            .unwrap();
    }

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-auto-transcript-compact-local-skill",
            "harness_id": "native-code-edit",
            "agent_id": "agent-local-skill",
            "tool_uses": [
                {
                    "id": "toolu-auto-compact-sleep-local-skill",
                    "name": "sleep",
                    "input": {"duration_ms": 0}
                }
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let compaction_attachment = body["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|attachment| attachment["type"] == "context_compaction_summary")
        .expect("auto compaction attachment should be present");
    let restore = &compaction_attachment["post_compact_skill_restore"];
    assert_eq!(restore["contract"], "coder.post_compact_skill_restore.v1");
    let restored_skills = restore["skills"].as_array().unwrap();
    assert_eq!(restored_skills.len(), 1);
    assert_eq!(restored_skills[0]["name"], "local-compact-skill");
    assert_eq!(restored_skills[0]["content_truncated"], false);
    let model_content = compaction_attachment["model_content"].as_array().unwrap();
    assert_eq!(model_content.len(), 2);
    let skill_text = model_content[1]["text"].as_str().unwrap();
    assert!(skill_text.contains("<skill name=\"local-compact-skill\""));
    assert!(skill_text.contains("Local Compact Skill"));
    assert!(skill_text.contains("Local skill restore marker."));
    assert!(skill_text.contains("run-auto-transcript-compact-local-skill"));
    assert!(!skill_text.contains("${CLAUDE_SESSION_ID}"));
    assert!(!skill_text.contains("description: Keep local skill instructions"));
    assert_eq!(captured.lock().unwrap().len(), 1);

    let _ = fs::remove_dir_all(skill_root);
    let _ = fs::remove_dir_all(store_root);
}

mod model_tool_hooks;

#[tokio::test]
async fn model_tool_turn_endpoint_uses_host_harness_context_for_permission_phase() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    fs::write(
        repo.join("change.patch"),
        "\
diff --git a/tracked.txt b/tracked.txt
index df967b9..5ea2ed4 100644
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
    )
    .unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "run_id": "run-model-tool-host-context",
            "harness_id": "workflow-planner",
            "tool_uses": [{
                "id": "toolu-host-context",
                "name": "patch_preview",
                "input": {
                    "repo_root": repo,
                    "patch_file": "change.patch",
                    "harness_id": "workflow-planner"
                }
            }]
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let result = &body["results"].as_array().unwrap()[0];
    assert_eq!(result["status"], "blocked");
    assert_eq!(result["is_error"], true);
    assert_eq!(result["payload"]["blocked_by"], "permission_decision");
    let permission_phase = result["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(permission_phase["status"], "blocked_before_tool_endpoint");
    assert_eq!(permission_phase["required_permission"], "write_files");
    assert_eq!(
        permission_phase["permission_policy_source"]["type"],
        "host_context"
    );
    assert_eq!(
        permission_phase["permission_policy_source"]["harness_id"],
        "workflow-planner"
    );
    assert_eq!(permission_phase["permission_result"]["behavior"], "deny");
    assert_eq!(
        permission_phase["policy_decision_status"],
        "denied_by_policy"
    );
    let tool_execution_phase = result["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("tool_execution"))
        .unwrap();
    assert_eq!(tool_execution_phase["status"], "blocked");
    assert_eq!(tool_execution_phase["blocked_by"], "permission_decision");

    let events = store
        .read_events(&RunId::from_string("run-model-tool-host-context"))
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "model_tool.phase")
            .count(),
        4
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_execute_endpoint_infers_permission_policy_from_run_snapshot() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    fs::write(
        repo.join("change.patch"),
        "\
diff --git a/tracked.txt b/tracked.txt
index df967b9..5ea2ed4 100644
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
    )
    .unwrap();
    let run_id = RunId::from_string("run-model-tool-snapshot");
    store
        .write_run_config_snapshot(&run_id, &default_project_config())
        .unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "node.started",
                json!({
                    "node_id": "review",
                    "harness": "workflow-planner"
                }),
            ),
        )
        .unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-snapshot-policy",
            "tool_name": "patch_preview",
            "run_id": "run-model-tool-snapshot",
            "input": {
                "repo_root": repo,
                "patch_file": "change.patch"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert_eq!(body["is_error"], true);
    assert_eq!(body["payload"]["blocked_by"], "permission_decision");
    let permission_phase = body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(permission_phase["status"], "blocked_before_tool_endpoint");
    assert_eq!(permission_phase["required_permission"], "write_files");
    assert_eq!(
        permission_phase["permission_policy_source"]["type"],
        "run_config_snapshot_event_inferred"
    );
    assert_eq!(
        permission_phase["permission_policy_source"]["harness_id"],
        "workflow-planner"
    );
    assert_eq!(permission_phase["permission_result"]["behavior"], "deny");
    assert_eq!(
        permission_phase["policy_decision_status"],
        "denied_by_policy"
    );
    let tool_execution_phase = body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("tool_execution"))
        .unwrap();
    assert_eq!(tool_execution_phase["status"], "blocked");
    assert_eq!(tool_execution_phase["blocked_by"], "permission_decision");

    let phase_events = store.read_events(&run_id).unwrap();
    assert_eq!(
        phase_events
            .iter()
            .filter(|event| event.kind == "model_tool.phase")
            .count(),
        4
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_permission_deny_blocks_patch_apply_without_editing_file() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store));
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

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-denied-patch-apply",
            "tool_name": "patch_apply",
            "run_id": "run-model-tool-denied-patch-apply",
            "harness_id": "workflow-planner",
            "input": {
                "repo_root": repo,
                "patch_file": "change.patch",
                "source": "model",
                "approved": true,
                "run_id": "run-model-tool-denied-patch-apply"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert_eq!(body["is_error"], true);
    assert_eq!(body["payload"]["blocked_by"], "permission_decision");
    assert_eq!(
        body["payload"]["policy_decision_status"],
        "denied_by_policy"
    );
    assert_eq!(body["payload"]["required_permission"], "write_files");
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt")).unwrap(),
        "base\n"
    );
    let tool_execution_phase = body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("tool_execution"))
        .unwrap();
    assert_eq!(tool_execution_phase["status"], "blocked");
    assert_eq!(tool_execution_phase["blocked_by"], "permission_decision");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_alias_accepts_claude_shape_from_run_snapshot() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-alias");
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.backend = "mock".to_owned();
    harness.tools = vec![
        "agent_subagent".to_owned(),
        "read_file".to_owned(),
        "repo_search_text".to_owned(),
    ];
    config.agents.get_mut("executor").unwrap().tools = vec!["agent_subagent(reviewer)".to_owned()];
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({
                    "workflow_id": "planner-led",
                    "repo_root": ".",
                    "task": "delegate from model tool"
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
                "node.started",
                json!({
                    "round": 1,
                    "node_id": "executor",
                    "agent": "executor",
                    "harness": "native-code-edit",
                    "backend": "mock"
                }),
            ),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-alias",
            "tool_name": "Agent",
            "run_id": "run-model-tool-agent-alias",
            "input": {
                "description": "review plan",
                "prompt": "Inspect the current plan and report back.",
                "subagent_type": "reviewer",
                "approved": true
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(body["payload"]["status"], "completed");
    assert_eq!(body["payload"]["run_id"], "run-model-tool-agent-alias");
    let child_agent_id = body["payload"]["agent_id"].as_str().unwrap();
    let records = store
        .read_subagent_transcript_records(&run_id, child_agent_id)
        .unwrap();
    assert_eq!(records[0].kind, "subagent.started");
    assert_eq!(
        records[0].payload["context"]["parent"]["workflow_id"],
        "planner-led"
    );
    assert_eq!(
        records[0].payload["context"]["parent"]["node_id"],
        "executor"
    );
    assert_eq!(
        records[0].payload["context"]["parent"]["agent_id"],
        "executor"
    );
    assert_eq!(
        records[0].payload["context"]["parent"]["harness_id"],
        "native-code-edit"
    );
    assert_eq!(records[0].payload["context"]["subagent_name"], "reviewer");
    let inherited = records[0].payload["context"]["tools"]["inherited"]
        .as_array()
        .unwrap();
    assert!(inherited.iter().any(|tool| tool == "read_file"));
    assert!(!inherited.iter().any(|tool| tool == "agent_subagent"));
    assert_eq!(
        records[1].payload["task"],
        "Inspect the current plan and report back."
    );
    let permission_phase = body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(
        permission_phase["required_permission"],
        "child_harness_permissions"
    );
    assert_eq!(permission_phase["permission_result"]["behavior"], "ask");
    assert_eq!(
        permission_phase["policy_decision_status"],
        "confirmation_supplied"
    );
    assert_eq!(
        permission_phase["agent_tool_allowed_types"]["allowed_agent_types"],
        json!(["reviewer"])
    );
    assert_eq!(
        permission_phase["agent_tool_allowed_types"]["reason"],
        "subagent_type_allowed"
    );
    assert_eq!(
        permission_phase["permission_policy_source"]["type"],
        "run_config_snapshot_event_inferred"
    );
    assert_eq!(
        permission_phase["permission_policy_source"]["harness_id"],
        "native-code-edit"
    );
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_subagent_inherits_run_plan_context_when_input_omits_backend_context() {
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Authorized Subagent\n").unwrap();
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-plan-context");
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.permissions.child_harness_permissions = ConfigPermissionDecision::Allow;
    harness.tools = vec![
        "agent_subagent".to_owned(),
        "repo_read_file".to_owned(),
        "repo_search_text".to_owned(),
        "git_diff".to_owned(),
        "command_run".to_owned(),
    ];
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({
                    "workflow_id": "planner-led",
                    "repo_root": repo.display().to_string(),
                    "task": "delegate after Start Work",
                    "plan_context": {
                        "start_work_authorized": true,
                        "plan_draft": {
                            "goal": "delegate after Start Work"
                        },
                        "acceptance_criteria": ["subagent can inspect README.md"]
                    }
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
                "node.started",
                json!({
                    "round": 1,
                    "node_id": "executor",
                    "agent": "executor",
                    "harness": "native-code-edit",
                    "backend": "native-rust"
                }),
            ),
        )
        .unwrap();
    let state = ApiState::new(store.clone());
    state.provider_settings.lock().unwrap().mock_mode = true;
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-plan-context",
            "tool_name": "agent_subagent",
            "run_id": "run-model-tool-agent-plan-context",
            "input": {
                "task": "Inspect README.md and report a one-line status.",
                "run_in_background": false
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(body["payload"]["status"], "completed");
    let child_agent_id = body["payload"]["agent_id"].as_str().unwrap();
    let records = store
        .read_subagent_transcript_records(&run_id, child_agent_id)
        .unwrap();
    assert!(records.iter().any(|record| {
        record.kind == "subagent.event" && record.payload["kind"] == "backend.native_rust.completed"
    }));
    let serialized = serde_json::to_string(&records).unwrap();
    assert!(!serialized.contains("missing_start_work_approval"));
    assert!(!serialized.contains("file writes require Start Work approval"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_alias_blocks_disallowed_subagent_type_from_run_snapshot() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-allowlist");
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.backend = "mock".to_owned();
    harness.tools = vec!["agent_subagent".to_owned(), "read_file".to_owned()];
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .tools
        .push("agent_subagent".to_owned());
    config.agents.get_mut("executor").unwrap().tools = vec!["agent_subagent(reviewer)".to_owned()];
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({
                    "workflow_id": "planner-led",
                    "repo_root": ".",
                    "task": "delegate from model tool"
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
                "node.started",
                json!({
                    "round": 1,
                    "node_id": "executor",
                    "agent": "executor",
                    "harness": "native-code-edit",
                    "backend": "mock"
                }),
            ),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-allowlist-deny",
            "tool_name": "Agent",
            "run_id": "run-model-tool-agent-allowlist",
            "input": {
                "description": "execute plan",
                "prompt": "Execute the current plan.",
                "subagent_type": "executor",
                "approved": true
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert_eq!(body["is_error"], true);
    assert_eq!(body["payload"]["blocked_by"], "permission_decision");
    assert_eq!(
        body["payload"]["policy_decision_status"],
        "denied_by_agent_tool_allowlist"
    );
    let permission_phase = body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(
        permission_phase["policy_decision_status"],
        "denied_by_agent_tool_allowlist"
    );
    assert_eq!(
        permission_phase["agent_tool_allowed_types"]["allowed_agent_types"],
        json!(["reviewer"])
    );
    assert_eq!(
        permission_phase["agent_tool_allowed_types"]["requested_subagent_type"],
        "executor"
    );
    assert_eq!(
        permission_phase["agent_tool_allowed_types"]["reason"],
        "subagent_type_not_allowed"
    );
    let tool_execution_phase = body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("tool_execution"))
        .unwrap();
    assert_eq!(tool_execution_phase["status"], "blocked");
    assert_eq!(tool_execution_phase["blocked_by"], "permission_decision");
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_alias_blocks_persisted_agent_type_deny_rule() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-deny-rule");
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.backend = "mock".to_owned();
    harness.tools = vec!["agent_subagent".to_owned(), "read_file".to_owned()];
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let mut settings = coder_config::PermissionSettingsRecord::new(
        coder_config::PermissionUpdateDestination::LocalSettings,
    );
    settings.rules.deny.push(coder_config::PermissionRuleValue {
        tool_name: "Agent".to_owned(),
        rule_content: Some("reviewer".to_owned()),
    });
    store
        .write_permission_settings("localSettings", &settings)
        .unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({
                    "workflow_id": "planner-led",
                    "repo_root": ".",
                    "task": "delegate from model tool"
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
                "node.started",
                json!({
                    "round": 1,
                    "node_id": "executor",
                    "agent": "executor",
                    "harness": "native-code-edit",
                    "backend": "mock"
                }),
            ),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-deny-rule",
            "tool_name": "Agent",
            "run_id": "run-model-tool-agent-deny-rule",
            "input": {
                "description": "review plan",
                "prompt": "Inspect the current plan.",
                "subagent_type": "reviewer",
                "approved": true
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert_eq!(body["payload"]["blocked_by"], "permission_decision");
    assert_eq!(
        body["payload"]["policy_decision_status"],
        "denied_by_agent_type_rule"
    );
    let permission_phase = body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(
        permission_phase["agent_tool_deny_rule"]["destination"],
        "localSettings"
    );
    assert_eq!(
        permission_phase["agent_tool_deny_rule"]["rule"]["toolName"],
        "Agent"
    );
    assert_eq!(
        permission_phase["agent_tool_deny_rule"]["requested_subagent_type"],
        "reviewer"
    );
    let tool_execution_phase = body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("tool_execution"))
        .unwrap();
    assert_eq!(tool_execution_phase["status"], "blocked");
    assert_eq!(tool_execution_phase["blocked_by"], "permission_decision");
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_alias_applies_runtime_session_agent_type_deny_rule() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-session-deny");
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.backend = "mock".to_owned();
    harness.tools = vec!["agent_subagent".to_owned(), "read_file".to_owned()];
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({
                    "workflow_id": "planner-led",
                    "repo_root": ".",
                    "task": "delegate from model tool"
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
                "node.started",
                json!({
                    "round": 1,
                    "node_id": "executor",
                    "agent": "executor",
                    "harness": "native-code-edit",
                    "backend": "mock"
                }),
            ),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let update_response = post_json(
        app.clone(),
        "/api/v3/runs/run-model-tool-agent-session-deny/permissions/updates",
        json!({
            "harness_id": "native-code-edit",
            "source": "test",
            "updates": [{
                "type": "addRules",
                "destination": "session",
                "behavior": "deny",
                "rules": [{
                    "toolName": "Agent",
                    "ruleContent": "reviewer"
                }]
            }]
        }),
    )
    .await;
    assert_eq!(update_response.status(), StatusCode::OK);
    let update_body = response_json(update_response).await;
    assert_eq!(update_body["status"], "completed");
    assert_eq!(update_body["applications"][0]["status"], "skipped");
    assert_eq!(update_body["persistence"][0]["status"], "not_persisted");
    assert!(update_body["config_ref"].is_null());
    assert!(store
        .read_permission_settings::<PermissionSettingsRecord>("session")
        .unwrap()
        .is_none());

    let denied_response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-session-deny",
            "tool_name": "Agent",
            "run_id": "run-model-tool-agent-session-deny",
            "input": {
                "description": "review plan",
                "prompt": "Inspect the current plan.",
                "subagent_type": "reviewer",
                "approved": true
            }
        }),
    )
    .await;
    assert_eq!(denied_response.status(), StatusCode::OK);
    let denied_body = response_json(denied_response).await;
    assert_eq!(denied_body["status"], "blocked");
    assert_eq!(
        denied_body["payload"]["policy_decision_status"],
        "denied_by_agent_type_rule"
    );
    let permission_phase = denied_body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(
        permission_phase["agent_tool_deny_rule"]["destination"],
        "session"
    );
    assert_eq!(
        permission_phase["agent_tool_deny_rule"]["requested_subagent_type"],
        "reviewer"
    );

    let remove_response = post_json(
        app.clone(),
        "/api/v3/runs/run-model-tool-agent-session-deny/permissions/updates",
        json!({
            "harness_id": "native-code-edit",
            "source": "test",
            "updates": [{
                "type": "removeRules",
                "destination": "session",
                "behavior": "deny",
                "rules": [{
                    "toolName": "Agent",
                    "ruleContent": "reviewer"
                }]
            }]
        }),
    )
    .await;
    assert_eq!(remove_response.status(), StatusCode::OK);
    assert_eq!(response_json(remove_response).await["status"], "completed");

    let allowed_response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-session-deny-removed",
            "tool_name": "Agent",
            "run_id": "run-model-tool-agent-session-deny",
            "input": {
                "description": "review plan",
                "prompt": "Inspect the current plan after removal.",
                "subagent_type": "reviewer",
                "approved": true
            }
        }),
    )
    .await;
    assert_eq!(allowed_response.status(), StatusCode::OK);
    let allowed_body = response_json(allowed_response).await;
    assert_eq!(allowed_body["status"], "completed");
    assert_eq!(allowed_body["payload"]["status"], "completed");
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_background_subagent_status_uses_explicit_tool() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-background");
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.backend = "mock".to_owned();
    harness.tools = vec!["agent_subagent".to_owned(), "read_file".to_owned()];
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({
                    "workflow_id": "planner-led",
                    "repo_root": ".",
                    "task": "background delegate from model tool"
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
                "node.started",
                json!({
                    "round": 1,
                    "node_id": "executor",
                    "agent": "executor",
                    "harness": "native-code-edit",
                    "backend": "mock"
                }),
            ),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let launch_response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-background",
            "tool_name": "Task",
            "run_id": "run-model-tool-agent-background",
            "input": {
                "description": "background review",
                "prompt": "Inspect the current plan in the background.",
                "subagent_type": "reviewer",
                "run_in_background": true,
                "approved": true
            }
        }),
    )
    .await;

    assert_eq!(launch_response.status(), StatusCode::OK);
    let launch_body = response_json(launch_response).await;
    assert_eq!(launch_body["status"], "backgrounded");
    let task_id = launch_body["payload"]["background_task"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        launch_body["payload"]["background_task"]["status_url"],
        format!("/api/v3/tools/subagent/background/{task_id}")
    );
    let launch_permission_phase = launch_body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(
        launch_permission_phase["required_permission"],
        "child_harness_permissions"
    );
    assert_eq!(
        launch_permission_phase["permission_result"]["behavior"],
        "ask"
    );
    assert_eq!(
        launch_permission_phase["policy_decision_status"],
        "confirmation_supplied"
    );

    let mut status_body = Value::Null;
    for _ in 0..20 {
        let status_response = post_json(
            app.clone(),
            "/api/v3/tools/model/execute",
            json!({
                "tool_use_id": "toolu-agent-background-status",
                "tool_name": "read_subagent_status",
                "run_id": "run-model-tool-agent-background",
                "input": {
                    "task_id": task_id,
                    "block": true
                }
            }),
        )
        .await;
        assert_eq!(status_response.status(), StatusCode::OK);
        status_body = response_json(status_response).await;
        if status_body["payload"]["status"] == "completed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(status_body["status"], "completed");
    assert_eq!(status_body["payload"]["retrieval_status"], "success");
    assert_eq!(status_body["payload"]["block"], true);
    assert_eq!(status_body["payload"]["timeout_ms"], 30000);
    assert_eq!(
        status_body["payload"]["run_id"],
        "run-model-tool-agent-background"
    );
    assert_eq!(status_body["payload"]["event_count"], 1);
    let child_agent_id = status_body["payload"]["agent_id"].as_str().unwrap();
    let metadata = store
        .read_subagent_metadata(&run_id, child_agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(metadata.status.as_deref(), Some("completed"));
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_permission_ask_without_approval_blocks_before_execution() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-command-needs-confirmation",
            "tool_name": "command_run",
            "input": {
                "repo_root": repo.display().to_string(),
                "cwd": ".",
                "argv": platform_sleep_args(),
                "source": "model",
                "sandbox": true,
                "foreground_timeout_seconds": 1,
                "run_id": "run-model-tool-command-needs-confirmation"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert_eq!(body["is_error"], true);
    assert_eq!(body["payload"]["blocked_by"], "permission_decision");
    assert_eq!(
        body["payload"]["policy_decision_status"],
        "requires_confirmation"
    );
    assert_eq!(body["payload"]["required_permission"], "run_commands");
    assert!(body["payload"].get("background_task").is_none());
    let phases = body["phases"].as_array().unwrap();
    let permission_phase = phases
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(permission_phase["status"], "blocked_before_tool_endpoint");
    assert_eq!(
        permission_phase["policy_decision_status"],
        "requires_confirmation"
    );
    let tool_execution_phase = phases
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("tool_execution"))
        .unwrap();
    assert_eq!(tool_execution_phase["status"], "blocked");
    assert_eq!(tool_execution_phase["blocked_by"], "permission_decision");
    let post_hook_phase = phases
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("post_tool_use_hooks"))
        .unwrap();
    assert_eq!(
        post_hook_phase["status"],
        "skipped_permission_decision_blocked"
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn run_permission_update_allows_followup_model_tool_without_inline_approval() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-permission-update");
    let config = default_project_config();
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"workflow_id": "planner-led"}),
            ),
        )
        .unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                2,
                "node.started",
                json!({"node_id": "executor", "harness": "native-code-edit"}),
            ),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();

    let blocked_response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-command-before-permission-update",
            "tool_name": "command_run",
            "run_id": "run-permission-update",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo.display().to_string(),
                "cwd": ".",
                "argv": platform_echo_args("before-update"),
                "source": "model",
                "sandbox": true
            }
        }),
    )
    .await;
    assert_eq!(blocked_response.status(), StatusCode::OK);
    let blocked_body = response_json(blocked_response).await;
    assert_eq!(
        blocked_body["payload"]["policy_decision_status"],
        "requires_confirmation"
    );

    let update_response = post_json(
        app.clone(),
        "/api/v3/runs/run-permission-update/permissions/updates",
        json!({
            "harness_id": "native-code-edit",
            "source": "test",
            "updates": [{
                "type": "addRules",
                "destination": "session",
                "behavior": "allow",
                "rules": [{
                    "toolName": "run_commands"
                }]
            }]
        }),
    )
    .await;
    assert_eq!(update_response.status(), StatusCode::OK);
    let update_body = response_json(update_response).await;
    assert_eq!(update_body["contract"], "coder.run_permission_update.v1");
    assert_eq!(update_body["status"], "completed");
    assert_eq!(update_body["harness_id"], "native-code-edit");
    assert_eq!(update_body["applications"][0]["status"], "applied");
    assert_eq!(
        update_body["applications"][0]["applied_permissions"][0],
        "run_commands"
    );
    assert!(update_body["config_ref"]
        .as_str()
        .unwrap()
        .contains("project-config.snapshot.json"));
    assert_eq!(update_body["persistence"][0]["destination"], "session");
    assert_eq!(update_body["persistence"][0]["status"], "not_persisted");
    assert!(store
        .read_permission_settings::<PermissionSettingsRecord>("session")
        .unwrap()
        .is_none());

    let updated_config: ProjectConfig = serde_json::from_value(
        store
            .read_run_config_snapshot_json(&run_id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        updated_config
            .harnesses
            .get("native-code-edit")
            .unwrap()
            .permissions
            .run_commands,
        ConfigPermissionDecision::Allow
    );
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "permission.updated"
            && event.payload["contract"] == "coder.run_permission_update.v1"
            && event.payload["applications"][0]["applied_permissions"][0] == "run_commands"
    }));

    let allowed_response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-command-after-permission-update",
            "tool_name": "command_run",
            "run_id": "run-permission-update",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo.display().to_string(),
                "cwd": ".",
                "argv": platform_echo_args("after-update"),
                "source": "model",
                "sandbox": true
            }
        }),
    )
    .await;
    assert_eq!(allowed_response.status(), StatusCode::OK);
    let allowed_body = response_json(allowed_response).await;
    assert_eq!(allowed_body["status"], "completed");
    assert_eq!(allowed_body["is_error"], false);
    assert_eq!(
        allowed_body["payload"]["result"]["requires_approval"],
        false
    );
    assert!(allowed_body["content"]
        .as_str()
        .unwrap()
        .contains("after-update"));
    let permission_phase = allowed_body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(permission_phase["permission_result"]["behavior"], "allow");
    assert_eq!(
        permission_phase["policy_decision_status"],
        "allowed_by_policy"
    );
    let tool_execution_phase = allowed_body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("tool_execution"))
        .unwrap();
    assert_eq!(
        tool_execution_phase["policy_approval_defaults"]["approved"],
        true
    );

    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn run_permission_update_persists_local_settings_destination() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-permission-update-local-settings");
    store
        .write_run_config_snapshot(&run_id, &default_project_config())
        .unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "run.started",
                json!({"workflow_id": "planner-led"}),
            ),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app.clone(),
        "/api/v3/runs/run-permission-update-local-settings/permissions/updates",
        json!({
            "harness_id": "native-code-edit",
            "source": "test",
            "updates": [
                {
                    "type": "addRules",
                    "destination": "localSettings",
                    "behavior": "allow",
                    "rules": [{
                        "toolName": "Bash",
                        "ruleContent": "Bash(*)"
                    }]
                },
                {
                    "type": "addDirectories",
                    "destination": "localSettings",
                    "directories": ["F:/bbb/coder", "F:/bbb/coder"]
                },
                {
                    "type": "setMode",
                    "destination": "localSettings",
                    "mode": "acceptEdits"
                },
                {
                    "type": "addRules",
                    "destination": "session",
                    "behavior": "allow",
                    "rules": [{
                        "toolName": "Read"
                    }]
                }
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");

    let persistence = body["persistence"].as_array().unwrap();
    let local = persistence
        .iter()
        .find(|item| item["destination"].as_str() == Some("localSettings"))
        .unwrap();
    assert_eq!(local["status"], "persisted");
    assert_eq!(local["update_count"], 3);
    assert!(local["settings_ref"]
        .as_str()
        .unwrap()
        .contains("settings://permissions/localSettings.json"));
    let session = persistence
        .iter()
        .find(|item| item["destination"].as_str() == Some("session"))
        .unwrap();
    assert_eq!(session["status"], "not_persisted");

    let settings = store
        .read_permission_settings::<PermissionSettingsRecord>("localSettings")
        .unwrap()
        .unwrap();
    assert_eq!(
        settings.destination,
        coder_config::PermissionUpdateDestination::LocalSettings
    );
    assert_eq!(
        settings.default_mode,
        coder_config::PermissionMode::AcceptEdits
    );
    assert_eq!(settings.rules.allow[0].tool_name, "Bash");
    assert_eq!(
        settings.additional_directories,
        vec!["F:/bbb/coder".to_owned()]
    );
    assert_eq!(settings.last_update_source.as_deref(), Some("test"));
    assert!(store
        .read_permission_settings::<PermissionSettingsRecord>("session")
        .unwrap()
        .is_none());

    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "permission.updated"
            && event.payload["persistence"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| {
                    item["destination"].as_str() == Some("localSettings")
                        && item["status"].as_str() == Some("persisted")
                })
    }));

    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_execute_command_run_defaults_to_background_on_timeout() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();

    let response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-command-background",
            "tool_name": "command_run",
            "input": {
                "repo_root": repo.display().to_string(),
                "cwd": ".",
                "argv": platform_sleep_args(),
                "source": "model",
                "sandbox": true,
                "approved": true,
                "foreground_timeout_seconds": 1,
                "run_id": "run-model-tool-command-bg"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "backgrounded");
    assert_eq!(body["is_error"], false);
    assert_eq!(body["payload"]["result"]["status"], "backgrounded");
    let task_id = body["payload"]["background_task"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(body["payload"]["background_task"]["status"], "running");
    let permission_phase = body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(permission_phase["required_permission"], "run_commands");
    assert_eq!(permission_phase["permission_result"]["behavior"], "ask");
    assert_eq!(
        permission_phase["policy_decision_status"],
        "confirmation_supplied"
    );
    let tool_execution_phase = body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("tool_execution"))
        .unwrap();
    assert_eq!(
        tool_execution_phase["applied_defaults"]["background_on_timeout"],
        true
    );

    let output_response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-command-output-read",
            "tool_name": "read_command_output",
            "input": {
                "task_id": task_id,
                "block": false,
                "run_id": "run-model-tool-command-bg"
            }
        }),
    )
    .await;
    assert_eq!(output_response.status(), StatusCode::OK);
    let output_body = response_json(output_response).await;
    assert_eq!(output_body["payload"]["retrieval_status"], "not_ready");
    assert_eq!(output_body["payload"]["block"], false);
    assert_eq!(output_body["payload"]["timeout_ms"], 5000);
    assert_eq!(output_body["payload"]["status"], "running");
    let output_permission_phase = output_body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(output_permission_phase["required_permission"], "read_files");
    assert_eq!(
        output_permission_phase["permission_result"]["behavior"],
        "allow"
    );

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
async fn model_tool_command_output_can_block_for_completion() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();

    let launch_response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-command-background-delayed",
            "tool_name": "command_background",
            "input": {
                "repo_root": repo.display().to_string(),
                "cwd": ".",
                "argv": platform_delayed_echo_args("done"),
                "source": "model",
                "sandbox": true,
                "approved": true,
                "run_id": "run-model-tool-command-task-output"
            }
        }),
    )
    .await;

    assert_eq!(launch_response.status(), StatusCode::OK);
    let launch_body = response_json(launch_response).await;
    assert_eq!(launch_body["status"], "running");
    let task_id = launch_body["payload"]["task_id"].as_str().unwrap();

    let output_response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-command-task-output-block",
            "tool_name": "read_command_output",
            "input": {
                "task_id": task_id,
                "block": true,
                "timeout": 2000,
                "run_id": "run-model-tool-command-task-output"
            }
        }),
    )
    .await;

    assert_eq!(output_response.status(), StatusCode::OK);
    let output_body = response_json(output_response).await;
    assert_eq!(output_body["status"], "completed");
    assert_eq!(output_body["payload"]["retrieval_status"], "success");
    assert_eq!(output_body["payload"]["block"], true);
    assert_eq!(output_body["payload"]["timeout_ms"], 2000);
    assert_eq!(output_body["payload"]["status"], "completed");
    assert!(output_body["payload"]["output_preview"]
        .as_str()
        .unwrap()
        .contains("done"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_cancel_background_command_uses_explicit_tool() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();

    let launch_response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-command-background-stop-launch",
            "tool_name": "command_background",
            "input": {
                "repo_root": repo.display().to_string(),
                "cwd": ".",
                "argv": platform_sleep_args(),
                "source": "model",
                "sandbox": true,
                "approved": true,
                "run_id": "run-model-tool-command-task-stop"
            }
        }),
    )
    .await;
    assert_eq!(launch_response.status(), StatusCode::OK);
    let launch_body = response_json(launch_response).await;
    assert_eq!(launch_body["status"], "running");
    let task_id = launch_body["payload"]["task_id"].as_str().unwrap();

    let stop_response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-command-task-stop",
            "tool_name": "cancel_command_background",
            "input": {
                "task_id": task_id,
                "approved": true,
                "run_id": "run-model-tool-command-task-stop"
            }
        }),
    )
    .await;
    assert_eq!(stop_response.status(), StatusCode::OK);
    let stop_body = response_json(stop_response).await;
    assert_eq!(stop_body["status"], "cancelled");
    assert_eq!(stop_body["is_error"], false);
    assert_eq!(stop_body["payload"]["task_id"], task_id);
    assert_eq!(stop_body["payload"]["status"], "cancelled");
    assert_eq!(stop_body["payload"]["cancelled"], true);
    let permission_phase = stop_body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(permission_phase["required_permission"], "run_commands");

    let status_body = wait_background_status(app, task_id, &["cancelled"]).await;
    assert_eq!(status_body["status"], "cancelled");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_cancel_subagent_uses_explicit_permission() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-subagent-task-stop");
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("native-code-edit").unwrap();
    harness.backend = "mock".to_owned();
    harness.tools = vec!["agent_subagent".to_owned(), "read_file".to_owned()];
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    store
        .append_event(
            &run_id,
            &CoderEvent::new(
                run_id.clone(),
                1,
                "node.started",
                json!({
                    "node_id": "executor",
                    "agent": "executor",
                    "harness": "native-code-edit",
                    "backend": "mock"
                }),
            ),
        )
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let launch_response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-subagent-stop-launch",
            "tool_name": "Task",
            "run_id": "run-model-tool-subagent-task-stop",
            "input": {
                "description": "background review",
                "prompt": "Inspect the current plan in the background.",
                "subagent_type": "reviewer",
                "run_in_background": true,
                "approved": true
            }
        }),
    )
    .await;
    assert_eq!(launch_response.status(), StatusCode::OK);
    let launch_body = response_json(launch_response).await;
    let task_id = launch_body["payload"]["background_task"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let mut final_status_body = Value::Null;
    for _ in 0..20 {
        let status_response = post_json(
            app.clone(),
            "/api/v3/tools/model/execute",
            json!({
                "tool_use_id": "toolu-subagent-stop-status",
                "tool_name": "read_subagent_status",
                "run_id": "run-model-tool-subagent-task-stop",
                "input": {
                    "task_id": task_id,
                    "block": false
                }
            }),
        )
        .await;
        assert_eq!(status_response.status(), StatusCode::OK);
        let status_body = response_json(status_response).await;
        final_status_body = status_body.clone();
        if status_body["payload"]["status"] == "completed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(final_status_body["payload"]["status"], "completed");

    let stop_response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-subagent-task-stop",
            "tool_name": "cancel_subagent_background",
            "run_id": "run-model-tool-subagent-task-stop",
            "input": {
                "task_id": task_id,
                "approved": true
            }
        }),
    )
    .await;
    assert_eq!(stop_response.status(), StatusCode::OK);
    let stop_body = response_json(stop_response).await;
    assert_eq!(stop_body["status"], "completed");
    assert_eq!(stop_body["is_error"], false);
    assert_eq!(stop_body["payload"]["task_id"], task_id);
    assert_eq!(stop_body["payload"]["status"], "completed");
    assert_eq!(stop_body["payload"]["cancelled"], false);
    let permission_phase = stop_body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(
        permission_phase["required_permission"],
        "child_harness_permissions"
    );
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_execute_endpoint_returns_error_tool_result_for_unknown_tool() {
    let app = test_router();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-missing-1",
            "tool_name": "definitely_missing_tool",
            "input": {}
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["contract"], "coder.model_tool_result.v1");
    assert_eq!(body["type"], "tool_result");
    assert_eq!(body["tool_use_id"], "toolu-missing-1");
    assert_eq!(body["status"], "failed");
    assert_eq!(body["is_error"], true);
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("<tool_use_error>Error: No such tool available"));
}

#[tokio::test]
async fn model_tool_turn_endpoint_returns_ordered_tool_results_with_evidence_refs() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let app = router(ApiState::new(store.clone()));
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("A.md"), "alpha\n").unwrap();
    fs::write(repo.join("B.md"), "beta\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "max_tool_use_concurrency": 10,
            "tool_uses": [
                {
                    "id": "toolu-read-a",
                    "name": "repo_read_file",
                    "input": {
                        "repo_root": repo,
                        "path": "A.md",
                        "run_id": "run-model-tool-turn"
                    }
                },
                {
                    "id": "toolu-read-b",
                    "name": "repo_read_file",
                    "input": {
                        "repo_root": repo,
                        "path": "B.md",
                        "run_id": "run-model-tool-turn"
                    }
                }
            ]
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["contract"], "coder.model_tool_turn.v1");
    assert_eq!(body["source"], "coder-server");
    assert_eq!(body["result_contract"], "coder.model_tool_result.v1");
    assert_eq!(
        body["model_tool_result_bridge"],
        "/api/v3/tools/model/execute"
    );
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["tool_use_id"], "toolu-read-a");
    assert_eq!(results[0]["type"], "tool_result");
    assert_eq!(results[0]["status"], "completed");
    assert!(results[0]["content"].as_str().unwrap().contains("alpha"));
    assert_eq!(results[0]["refs"][0]["label"], "repo_evidence");
    let first_phases = results[0]["phases"].as_array().unwrap();
    assert_eq!(first_phases.len(), 4);
    assert_eq!(first_phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(first_phases[1]["phase"], "permission_decision");
    assert_eq!(first_phases[1]["required_permission"], "read_files");
    assert_eq!(first_phases[2]["phase"], "tool_execution");
    assert_eq!(first_phases[2]["status"], "completed");
    assert_eq!(first_phases[3]["phase"], "post_tool_use_hooks");
    assert_eq!(results[1]["tool_use_id"], "toolu-read-b");
    assert!(results[1]["content"].as_str().unwrap().contains("beta"));
    let second_phases = results[1]["phases"].as_array().unwrap();
    assert_eq!(second_phases.len(), 4);
    assert_eq!(second_phases[1]["phase"], "permission_decision");
    assert_eq!(second_phases[2]["status"], "completed");

    let evidence = store
        .list_repo_evidence(&RunId::from_string("run-model-tool-turn"))
        .unwrap();
    assert_eq!(evidence.len(), 2);
    assert!(evidence
        .iter()
        .all(|item| item.kind == RepoEvidenceKind::RepoRead));
    let events = store
        .read_events(&RunId::from_string("run-model-tool-turn"))
        .unwrap();
    let progress_events = events
        .iter()
        .filter(|event| event.kind == "model_tool.phase.progress")
        .collect::<Vec<_>>();
    assert_eq!(progress_events.len(), 8);
    assert!(progress_events.iter().any(|event| {
        event.payload["tool_use_id"] == "toolu-read-a"
            && event.payload["phase"] == "tool_execution"
            && event.payload["status"] == "started"
    }));
    assert!(progress_events.iter().any(|event| {
        event.payload["tool_use_id"] == "toolu-read-b"
            && event.payload["phase"] == "tool_execution"
            && event.payload["status"] == "started"
    }));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_turn_endpoint_returns_error_result_for_unknown_tool() {
    let app = test_router();

    let response = post_json(
        app,
        "/api/v3/tools/model/turn",
        json!({
            "tool_uses": [
                {
                    "id": "toolu-missing-1",
                    "name": "definitely_missing_tool",
                    "input": {}
                }
            ]
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["contract"], "coder.model_tool_result.v1");
    assert_eq!(results[0]["type"], "tool_result");
    assert_eq!(results[0]["tool_use_id"], "toolu-missing-1");
    assert_eq!(results[0]["status"], "failed");
    assert_eq!(results[0]["is_error"], true);
    assert!(results[0]["content"]
        .as_str()
        .unwrap()
        .contains("<tool_use_error>Error: No such tool available"));
}

#[tokio::test]
async fn repo_evidence_endpoint_reports_missing_and_invalid_refs() {
    let app = test_router();
    let missing_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/repo-evidence/repo-read:missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let invalid_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/repo-evidence/bad*ref")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
    assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn artifact_endpoint_reports_missing_and_invalid_names() {
    let app = test_router();
    let missing_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/artifacts/missing.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let invalid_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/artifacts/bad*name.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
    assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn checkpoint_endpoints_roundtrip_and_validate_names() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    store.write_metadata(&state).unwrap();
    let app = router(ApiState::new(store));

    let write_response = post_json(
        app.clone(),
        "/api/v3/runs/run-1/checkpoints/resume.json",
        json!({"step": 2}),
    )
    .await;
    assert_eq!(write_response.status(), StatusCode::OK);
    let write_body = response_json(write_response).await;
    assert_eq!(write_body["checkpoint_name"], "resume.json");
    assert!(write_body["checkpoint_ref"]
        .as_str()
        .unwrap()
        .ends_with("/resume.json"));

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/checkpoints")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = response_json(list_response).await;
    assert_eq!(list_body["checkpoints"][0]["name"], "resume.json");

    let read_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/checkpoints/resume.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let read_body = response_json(read_response).await;
    assert_eq!(read_body["payload"]["step"], 2);

    let missing_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/checkpoints/missing.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let invalid_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/runs/run-1/checkpoints/bad*name.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
    assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn blob_endpoint_returns_content_by_digest() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let blob_ref = store.write_blob(b"hello blob").unwrap();
    let digest = blob_ref.strip_prefix("blob://sha256/").unwrap().to_owned();
    let app = router(ApiState::new(store));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v3/blobs/sha256/{digest}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(bytes.as_ref(), b"hello blob");

    let missing_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/blobs/sha256/0000000000000000000000000000000000000000000000000000000000000000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    let invalid_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/blobs/sha256/not-a-digest")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
    assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
    let _ = fs::remove_dir_all(root);
}

mod planner_workflow;

mod projections_cache;

#[derive(Default, Clone)]
struct CapturedWebhookRequest {
    body: Option<Value>,
    authorization: Option<String>,
    not_allowed: Option<String>,
}

struct WebhookTestState {
    response: Value,
    captured: Arc<Mutex<CapturedWebhookRequest>>,
}

struct RawWebhookTestState {
    status: StatusCode,
    content_type: String,
    body: String,
    delay: Duration,
    captured: Arc<Mutex<CapturedWebhookRequest>>,
}

#[derive(Clone)]
struct HookTestCommand {
    shell: String,
    command: String,
}

async fn spawn_webhook_test_server(
    response: Value,
) -> (String, Arc<Mutex<CapturedWebhookRequest>>) {
    async fn hook_handler(
        State(state): State<Arc<WebhookTestState>>,
        request: Request<Body>,
    ) -> Json<Value> {
        let authorization = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let not_allowed = request
            .headers()
            .get("x-not-allowed")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice::<Value>(&bytes).ok();
        *state.captured.lock().unwrap() = CapturedWebhookRequest {
            body,
            authorization,
            not_allowed,
        };
        Json(state.response.clone())
    }

    let captured = Arc::new(Mutex::new(CapturedWebhookRequest::default()));
    let state = Arc::new(WebhookTestState {
        response,
        captured: captured.clone(),
    });
    let app = Router::new()
        .route("/hook", post(hook_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/hook"), captured)
}

async fn spawn_raw_webhook_test_server(
    status: StatusCode,
    content_type: &str,
    body: &str,
    delay: Duration,
) -> (String, Arc<Mutex<CapturedWebhookRequest>>) {
    async fn hook_handler(
        State(state): State<Arc<RawWebhookTestState>>,
        request: Request<Body>,
    ) -> impl IntoResponse {
        let authorization = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let not_allowed = request
            .headers()
            .get("x-not-allowed")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice::<Value>(&bytes).ok();
        *state.captured.lock().unwrap() = CapturedWebhookRequest {
            body,
            authorization,
            not_allowed,
        };
        if !state.delay.is_zero() {
            tokio::time::sleep(state.delay).await;
        }
        let mut response = (state.status, state.body.clone()).into_response();
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_str(&state.content_type).unwrap(),
        );
        response
    }

    let captured = Arc::new(Mutex::new(CapturedWebhookRequest::default()));
    let state = Arc::new(RawWebhookTestState {
        status,
        content_type: content_type.to_owned(),
        body: body.to_owned(),
        delay,
        captured: captured.clone(),
    });
    let app = Router::new()
        .route("/hook", post(hook_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/hook"), captured)
}

fn hook_capture_stdin_command(path: &Path) -> HookTestCommand {
    if cfg!(windows) {
        HookTestCommand {
            shell: "powershell".to_owned(),
            command: format!(
                "$inputText = [Console]::In.ReadToEnd(); Set-Content -LiteralPath '{}' -Value $inputText -NoNewline -Encoding UTF8; Write-Output 'hook stdin ok'",
                powershell_single_quote(path)
            ),
        }
    } else {
        HookTestCommand {
            shell: "sh".to_owned(),
            command: format!(
                "cat > '{}'; printf 'hook stdin ok'",
                shell_single_quote(path)
            ),
        }
    }
}

fn hook_emit_file_command(path: &Path) -> HookTestCommand {
    if cfg!(windows) {
        HookTestCommand {
            shell: "powershell".to_owned(),
            command: format!(
                "Get-Content -Raw -LiteralPath '{}'",
                powershell_single_quote(path)
            ),
        }
    } else {
        HookTestCommand {
            shell: "sh".to_owned(),
            command: format!("cat '{}'", shell_single_quote(path)),
        }
    }
}

fn hook_async_capture_stdin_command(capture: &Path, sentinel: &Path) -> HookTestCommand {
    if cfg!(windows) {
        HookTestCommand {
            shell: "powershell".to_owned(),
            command: format!(
                "Start-Sleep -Milliseconds 250; $inputText = [Console]::In.ReadToEnd(); Set-Content -LiteralPath '{}' -Value $inputText -NoNewline -Encoding UTF8; Set-Content -LiteralPath '{}' -Value 'done' -NoNewline -Encoding UTF8",
                powershell_single_quote(capture),
                powershell_single_quote(sentinel)
            ),
        }
    } else {
        HookTestCommand {
            shell: "sh".to_owned(),
            command: format!(
                "sleep 0.25; cat > '{}'; printf done > '{}'",
                shell_single_quote(capture),
                shell_single_quote(sentinel)
            ),
        }
    }
}

fn hook_async_response_command(capture: &Path, sentinel: &Path) -> HookTestCommand {
    let response_json = r#"{"systemMessage":"async system note","hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"async context note"}}"#;
    if cfg!(windows) {
        HookTestCommand {
            shell: "powershell".to_owned(),
            command: format!(
                "Start-Sleep -Milliseconds 250; $inputText = [Console]::In.ReadToEnd(); Set-Content -LiteralPath '{}' -Value $inputText -NoNewline -Encoding UTF8; Set-Content -LiteralPath '{}' -Value 'done' -NoNewline -Encoding UTF8; Write-Output '{}'",
                powershell_single_quote(capture),
                powershell_single_quote(sentinel),
                response_json
            ),
        }
    } else {
        HookTestCommand {
            shell: "sh".to_owned(),
            command: format!(
                "sleep 0.25; cat > '{}'; printf done > '{}'; printf '%s\\n' '{}'",
                shell_single_quote(capture),
                shell_single_quote(sentinel),
                response_json
            ),
        }
    }
}

async fn wait_for_path(path: &std::path::Path) {
    for _ in 0..30 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("path did not appear: {}", path.display());
}

async fn wait_for_events(
    store: &RunStore,
    run_id: &RunId,
    predicate: impl Fn(&[CoderEvent]) -> bool,
) -> Vec<CoderEvent> {
    for _ in 0..30 {
        let events = store.read_events(run_id).unwrap();
        if predicate(&events) {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let events = store.read_events(run_id).unwrap();
    panic!(
        "timed out waiting for run events; saw {} events",
        events.len()
    );
}

fn powershell_single_quote(path: &std::path::Path) -> String {
    path.display().to_string().replace('\'', "''")
}

fn shell_single_quote(path: &std::path::Path) -> String {
    path.display().to_string().replace('\'', "'\"'\"'")
}

fn test_router() -> Router {
    let state = ApiState::new(RunStore::new(temp_root()));
    state.provider_settings.lock().unwrap().mock_mode = true;
    router(state)
}

async fn spawn_openai_compatible_test_server() -> String {
    spawn_openai_compatible_test_server_with_payload(json!({
        "choices": [
            {
                "message": {
                    "content": "Live provider response."
                }
            }
        ]
    }))
    .await
}

fn native_finish_tool_call(call_id: &str, content: &str) -> Value {
    let arguments: Value = serde_json::from_str(content).expect("finish arguments must be JSON");
    assert!(
        arguments.get("files").is_none(),
        "finish cannot carry the removed text file-plan protocol"
    );
    json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": "finish",
            "arguments": content
        }
    })
}

async fn spawn_openai_compatible_test_server_with_payload(payload: Value) -> String {
    async fn chat_completion(State(payload): State<Arc<Value>>) -> Json<Value> {
        Json((*payload).clone())
    }

    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(Arc::new(payload));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn spawn_delayed_openai_compatible_test_server(delay: Duration, payload: Value) -> String {
    async fn chat_completion(State(state): State<Arc<(Duration, Value)>>) -> Json<Value> {
        tokio::time::sleep(state.0).await;
        Json(state.1.clone())
    }

    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(Arc::new((delay, payload)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn spawn_raw_openai_compatible_test_server(
    status: StatusCode,
    content_type: &'static str,
    body: String,
) -> String {
    async fn chat_completion(
        State(state): State<Arc<(StatusCode, &'static str, String)>>,
    ) -> axum::response::Response {
        axum::response::Response::builder()
            .status(state.0)
            .header("content-type", state.1)
            .body(Body::from(state.2.clone()))
            .unwrap()
    }

    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(Arc::new((status, content_type, body)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn spawn_planner_work_concurrency_test_server(executor_delay: Duration) -> String {
    async fn chat_completion(
        State(executor_delay): State<Duration>,
        request: Request<Body>,
    ) -> Json<Value> {
        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice::<Value>(&bytes).unwrap();
        let is_executor_request = body
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| !tools.is_empty());
        if is_executor_request {
            tokio::time::sleep(executor_delay).await;
            if provider_request_has_tool_result(&body, "call-write-parallel") {
                let final_content = json!({
                    "status": "completed",
                    "summary": "Created the requested concurrency fixture.",
                    "checks": ["parallel_planner_test: completed"],
                    "blockers": []
                })
                .to_string();
                return Json(json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [native_finish_tool_call(
                                "call-finish-parallel",
                                &final_content
                            )]
                        }
                    }]
                }));
            }
            let write_args = json!({
                "path": "PARALLEL.md",
                "content": "# Parallel Planner Test\n"
            })
            .to_string();
            return Json(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [{
                            "id": "call-write-parallel",
                            "type": "function",
                            "function": {
                                "name": "write_text_file",
                                "arguments": write_args
                            }
                        }]
                    }
                }]
            }));
        }
        Json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "I can continue planning while the active workflow runs."
                }
            }]
        }))
    }

    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(executor_delay);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn spawn_openai_compatible_capture_test_server(
    payload: Value,
) -> (String, Arc<Mutex<Option<Value>>>) {
    async fn chat_completion(
        State(state): State<CaptureState>,
        request: Request<Body>,
    ) -> Json<Value> {
        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
        *state.1.lock().unwrap() = serde_json::from_slice::<Value>(&bytes).ok();
        Json(state.0.clone())
    }

    let captured = Arc::new(Mutex::new(None));
    let state = Arc::new((payload, captured.clone()));
    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), captured)
}

async fn spawn_openai_compatible_streaming_capture_test_server(
) -> (String, Arc<Mutex<Option<Value>>>) {
    async fn chat_completion(
        State(captured): State<Arc<Mutex<Option<Value>>>>,
        request: Request<Body>,
    ) -> axum::response::Response {
        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
        *captured.lock().unwrap() = serde_json::from_slice::<Value>(&bytes).ok();
        let stream_text = [
            "data: {\"choices\":[{\"delta\":{\"content\":\"Streamed \"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"provider response.\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":41,\"completion_tokens\":7,\"total_tokens\":48,\"prompt_cache_hit_tokens\":11}}\n\n",
            "data: [DONE]\n\n",
        ]
        .join("");
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(Body::from(stream_text))
            .unwrap()
    }

    let captured = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), captured)
}

async fn spawn_openai_compatible_streaming_tool_sequence_test_server(
) -> (String, Arc<Mutex<Vec<Value>>>) {
    async fn chat_completion(
        State(state): State<StreamingSequenceCaptureState>,
        request: Request<Body>,
    ) -> axum::response::Response {
        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
        if let Ok(body) = serde_json::from_slice::<Value>(&bytes) {
            state.1.lock().unwrap().push(body);
        }
        let stream_text = state.0.lock().unwrap().pop_front().unwrap();
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(Body::from(stream_text))
            .unwrap()
    }

    let tool_stream = [
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"stream-read-1\",\"type\":\"function\",\"function\":{\"name\":\"repo_read_file_range\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    ]
    .join("");
    let final_stream = [
        "data: {\"choices\":[{\"delta\":{\"content\":\"Streaming inspection found stream-read-ok.\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    ]
    .join("");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::new((
        Mutex::new(VecDeque::from(vec![tool_stream, final_stream])),
        captured.clone(),
    ));
    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), captured)
}

async fn spawn_delayed_openai_compatible_streaming_test_server() -> String {
    async fn chat_completion() -> axum::response::Response {
        let opened = futures_util::stream::once(async {
            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(b": stream-open\n\n"))
        });
        let delayed = futures_util::stream::once(async {
            tokio::time::sleep(Duration::from_millis(80)).await;
            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"late\"},\"finish_reason\":\"stop\"}]}\n\n",
            ))
        });
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(opened.chain(delayed)))
            .unwrap()
    }

    let app = Router::new().route("/chat/completions", post(chat_completion));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn spawn_openai_compatible_sequence_capture_test_server(
    payloads: Vec<Value>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    async fn chat_completion(
        State(state): State<SequenceCaptureState>,
        request: Request<Body>,
    ) -> Json<Value> {
        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
        if let Ok(body) = serde_json::from_slice::<Value>(&bytes) {
            state.1.lock().unwrap().push(body);
        }
        let payload = state.0.lock().unwrap().pop_front().unwrap_or_else(|| {
            json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [
                                {
                                    "id": "call-agent-structured-output-default",
                                    "type": "function",
                                    "function": {
                                        "name": "StructuredOutput",
                                        "arguments": "{\"ok\": true}"
                                    }
                                }
                            ]
                        }
                    }
                ]
            })
        });
        Json(payload)
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::new((Mutex::new(VecDeque::from(payloads)), captured.clone()));
    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), captured)
}

async fn spawn_native_background_command_tool_loop_test_server(
    background_argv: Vec<String>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    async fn chat_completion(
        State(state): State<CommandLoopState>,
        request: Request<Body>,
    ) -> Json<Value> {
        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice::<Value>(&bytes).unwrap();
        let turn = {
            let mut captured = state.1.lock().unwrap();
            let turn = captured.len();
            captured.push(body.clone());
            turn
        };
        let payload = match turn {
            0 => {
                let background_args = json!({"argv": state.0.clone()}).to_string();
                json!({
                    "choices": [
                        {
                            "message": {
                                "role": "assistant",
                                "content": Value::Null,
                                "tool_calls": [
                                    {
                                        "id": "call-bg",
                                        "type": "function",
                                        "function": {
                                            "name": "command_background",
                                            "arguments": background_args
                                        }
                                    }
                                ]
                            }
                        }
                    ]
                })
            }
            1 => {
                let task_id = native_background_task_id_from_provider_request(&body)
                    .expect("background task id should be visible to the next model turn");
                let read_args = json!({
                    "task_id": task_id,
                    "timeout": 2000,
                    "block": true
                })
                .to_string();
                let write_args = json!({
                    "path": "BACKGROUND.md",
                    "content": "# Background Command\n\nObserved native-bg-done through shared tool execution.\n"
                })
                .to_string();
                json!({
                    "choices": [
                        {
                            "message": {
                                "role": "assistant",
                                "content": Value::Null,
                                "tool_calls": [
                                    {
                                        "id": "call-bg-output",
                                        "type": "function",
                                        "function": {
                                            "name": "read_command_output",
                                            "arguments": read_args
                                        }
                                    },
                                    {
                                        "id": "call-bg-write",
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
                })
            }
            _ => {
                let final_content = json!({
                    "status": "completed",
                    "summary": "Background command output was observed and BACKGROUND.md was written.",
                    "checks": ["command_background: observed"],
                    "blockers": []
                })
                .to_string();
                json!({
                    "choices": [
                        {
                            "message": {
                                "role": "assistant",
                                "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                            }
                        }
                    ]
                })
            }
        };
        Json(payload)
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::new((background_argv, captured.clone()));
    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), captured)
}

async fn spawn_native_background_subagent_status_test_server() -> (String, Arc<Mutex<Vec<Value>>>) {
    async fn chat_completion(
        State(captured): State<Arc<Mutex<Vec<Value>>>>,
        request: Request<Body>,
    ) -> Json<Value> {
        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice::<Value>(&bytes).unwrap();
        captured.lock().unwrap().push(body.clone());

        if provider_request_has_tool_result(&body, "call-bg-agent-write") {
            let final_content = json!({
                "status": "completed",
                "summary": "Background subagent output was observed and BG-SUBAGENT.md was written.",
                "checks": ["background_subagent_status: observed"],
                "blockers": []
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                        }
                    }
                ]
            }));
        }

        if provider_request_has_tool_result(&body, "call-bg-agent-output") {
            let write_args = json!({
                "path": "BG-SUBAGENT.md",
                "content": "# Background Subagent\n\nThe explicit status tool observed the background subagent.\n"
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [
                                {
                                    "id": "call-bg-agent-write",
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
            }));
        }

        if provider_request_has_tool_result(&body, "call-bg-agent") {
            let task_id = native_task_id_from_provider_request(&body, "call-bg-agent")
                .expect("background subagent task id should be visible to the next model turn");
            let output_args = json!({
                "task_id": task_id,
                "block": true,
                "timeout": 2000
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [
                                {
                                    "id": "call-bg-agent-output",
                                    "type": "function",
                                    "function": {
                                        "name": "read_subagent_status",
                                        "arguments": output_args
                                    }
                                }
                            ]
                        }
                    }
                ]
            }));
        }

        if provider_request_content_contains(&body, "Review whether BG-SUBAGENT.md is in scope") {
            let child_final_content = json!({
                "status": "completed",
                "summary": "Background child confirmed BG-SUBAGENT.md is in scope.",
                "checks": ["background_child: completed"],
                "blockers": []
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null, "tool_calls": [native_finish_tool_call("call-child-finish", &child_final_content)]
                        }
                    }
                ]
            }));
        }

        let agent_args = json!({
            "prompt": "Review whether BG-SUBAGENT.md is in scope and return a short status.",
            "description": "background reviewer",
            "subagent_type": "reviewer",
            "run_in_background": true
        })
        .to_string();
        Json(json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-bg-agent",
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
        }))
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), captured)
}

async fn spawn_native_background_subagent_writes_file_test_server(
) -> (String, Arc<Mutex<Vec<Value>>>) {
    async fn chat_completion(
        State(captured): State<Arc<Mutex<Vec<Value>>>>,
        request: Request<Body>,
    ) -> Json<Value> {
        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice::<Value>(&bytes).unwrap();
        captured.lock().unwrap().push(body.clone());

        if provider_request_has_tool_result(&body, "call-child-write") {
            let child_final_content = json!({
                "status": "completed",
                "summary": "Background child wrote CHILD-ONLY.md.",
                "checks": ["background_child: wrote CHILD-ONLY.md"],
                "blockers": []
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null, "tool_calls": [native_finish_tool_call("call-child-finish", &child_final_content)]
                        }
                    }
                ]
            }));
        }

        if provider_request_content_contains(&body, "Child-only subagent file") {
            let write_args = json!({
                "path": "CHILD-ONLY.md",
                "content": "# Child Only\n\nThis file was written by the background subagent.\n"
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [
                                {
                                    "id": "call-child-write",
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
            }));
        }

        if provider_request_has_tool_result(&body, "call-parent-output") {
            let final_content = json!({
                "status": "completed",
                "summary": "Background subagent completed and parent aggregated its report.",
                "checks": ["background_subagent_report: observed"],
                "blockers": []
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                        }
                    }
                ]
            }));
        }

        if provider_request_has_tool_result(&body, "call-parent-agent") {
            let task_id = native_task_id_from_provider_request(&body, "call-parent-agent")
                .expect("background subagent task id should be visible to the next model turn");
            let output_args = json!({
                "task_id": task_id,
                "block": true,
                "timeout": 2000
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [
                                {
                                    "id": "call-parent-output",
                                    "type": "function",
                                    "function": {
                                        "name": "read_subagent_status",
                                        "arguments": output_args
                                    }
                                }
                            ]
                        }
                    }
                ]
            }));
        }

        let agent_args = json!({
            "prompt": "Child-only subagent file task: write CHILD-ONLY.md exactly once, then report completion.",
            "description": "child file writer",
            "subagent_type": "writer",
            "run_in_background": true
        })
        .to_string();
        Json(json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-parent-agent",
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
        }))
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), captured)
}

async fn spawn_native_background_subagent_cancel_test_server(
    child_background_argv: Vec<String>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    async fn chat_completion(
        State(state): State<CommandLoopState>,
        request: Request<Body>,
    ) -> Json<Value> {
        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice::<Value>(&bytes).unwrap();
        state.1.lock().unwrap().push(body.clone());

        if provider_request_has_tool_result(&body, "call-cancel-bg-write") {
            let final_content = json!({
                "status": "completed",
                "summary": "Background subagent was cancelled and BG-SUBAGENT-CANCEL.md was written.",
                "checks": ["background_subagent_cancel: cancelled"],
                "blockers": []
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null, "tool_calls": [native_finish_tool_call("call-finish", &final_content)]
                        }
                    }
                ]
            }));
        }

        if provider_request_has_tool_result(&body, "call-cancel-bg-stop") {
            let write_args = json!({
                "path": "BG-SUBAGENT-CANCEL.md",
                "content": "# Background Subagent Cancelled\n\nThe explicit cancel tool stopped the background subagent.\n"
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [
                                {
                                    "id": "call-cancel-bg-write",
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
            }));
        }

        if provider_request_has_tool_result(&body, "call-cancel-bg-agent") {
            let task_id = native_task_id_from_provider_request(&body, "call-cancel-bg-agent")
                .expect("background subagent task id should be visible to parent model turn");
            let stop_args = json!({
                "task_id": task_id
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [
                                {
                                    "id": "call-cancel-bg-stop",
                                    "type": "function",
                                    "function": {
                                        "name": "cancel_subagent_background",
                                        "arguments": stop_args
                                    }
                                }
                            ]
                        }
                    }
                ]
            }));
        }

        if provider_request_has_tool_result(&body, "call-child-bg-command") {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let child_final_content = json!({
                "status": "completed",
                "summary": "Child cancellation probe would have completed if it had not been cancelled.",
                "checks": ["child_background_command: observed"],
                "blockers": []
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null, "tool_calls": [native_finish_tool_call("call-child-finish", &child_final_content)]
                        }
                    }
                ]
            }));
        }

        if provider_request_is_child_cancellation_probe(&body) {
            let background_args = json!({
                "argv": state.0.clone()
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [
                                {
                                    "id": "call-child-bg-command",
                                    "type": "function",
                                    "function": {
                                        "name": "command_background",
                                        "arguments": background_args
                                    }
                                }
                            ]
                        }
                    }
                ]
            }));
        }

        let agent_args = json!({
            "prompt": "Hold cancellation probe: start a background command and wait until the parent stops this subagent.",
            "description": "cancellable background reviewer",
            "subagent_type": "reviewer",
            "run_in_background": true
        })
        .to_string();
        Json(json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-cancel-bg-agent",
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
        }))
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::new((child_background_argv, captured.clone()));
    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), captured)
}

fn native_background_task_id_from_provider_request(request: &Value) -> Option<String> {
    native_task_id_from_provider_request(request, "call-bg")
}

fn native_task_id_from_provider_request(request: &Value, tool_call_id: &str) -> Option<String> {
    request
        .get("messages")?
        .as_array()?
        .iter()
        .filter(|message| {
            message.get("role").and_then(Value::as_str) == Some("tool")
                && message.get("tool_call_id").and_then(Value::as_str) == Some(tool_call_id)
        })
        .find_map(|message| {
            let content = message.get("content")?.as_str()?;
            let payload = serde_json::from_str::<Value>(content).ok()?;
            payload
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    payload
                        .get("background_task")
                        .and_then(|task| task.get("task_id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
        })
}

fn provider_request_has_tool_result(request: &Value, tool_call_id: &str) -> bool {
    request
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|message| {
            message.get("role").and_then(Value::as_str) == Some("tool")
                && message.get("tool_call_id").and_then(Value::as_str) == Some(tool_call_id)
        })
}

fn provider_request_is_child_cancellation_probe(request: &Value) -> bool {
    provider_request_content_contains(request, "Hold cancellation probe")
}

fn provider_request_content_contains(request: &Value, needle: &str) -> bool {
    request
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|message| {
            message
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(|content| content.contains(needle))
        })
}

#[derive(Clone)]
struct OpenAiCompatibleStatusResponse {
    status: StatusCode,
    content_type: &'static str,
    body: String,
}

async fn spawn_openai_compatible_status_sequence_capture_test_server(
    responses: Vec<OpenAiCompatibleStatusResponse>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    async fn chat_completion(
        State(state): State<StatusSequenceState>,
        request: Request<Body>,
    ) -> axum::response::Response {
        let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
        if let Ok(body) = serde_json::from_slice::<Value>(&bytes) {
            state.1.lock().unwrap().push(body);
        }
        let response =
            state
                .0
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| OpenAiCompatibleStatusResponse {
                    status: StatusCode::OK,
                    content_type: "application/json",
                    body: json!({
                        "choices": [
                            {
                                "finish_reason": "stop",
                                "message": {
                                    "content": "default provider response"
                                }
                            }
                        ]
                    })
                    .to_string(),
                });
        axum::response::Response::builder()
            .status(response.status)
            .header("content-type", response.content_type)
            .body(Body::from(response.body))
            .unwrap()
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::new((Mutex::new(VecDeque::from(responses)), captured.clone()));
    let app = Router::new()
        .route("/chat/completions", post(chat_completion))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), captured)
}

fn configure_test_provider(state: &ApiState, provider_base_url: String, default_model: &str) {
    let mut settings = state.provider_settings.lock().unwrap();
    settings.mock_mode = false;
    settings.default_provider = "openai-compatible".to_owned();
    settings.default_model = default_model.to_owned();
    settings
        .base_urls
        .insert("openai-compatible".to_owned(), provider_base_url);
    settings.api_keys.insert(
        "openai-compatible".to_owned(),
        ProviderKeyState {
            configured: true,
            source: "settings".to_owned(),
            secret: Some("provider-test-token".to_owned()),
        },
    );
}

fn provider_backed_test_app(store_root: &PathBuf, provider_base_url: String) -> Router {
    let state = ApiState::new(RunStore::new(store_root));
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
                secret: Some("provider-test-token".to_owned()),
            },
        );
    }
    router(state)
}

async fn post_json(app: Router, uri: &str, body: Value) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn get_json(app: Router, uri: &str) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn delete_json(app: Router, uri: &str) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("DELETE")
            .uri(uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn wait_background_status(app: Router, task_id: &str, terminal_statuses: &[&str]) -> Value {
    for _ in 0..80 {
        let response = get_json(
            app.clone(),
            &format!("/api/v3/tools/command/background/{task_id}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        if let Some(status) = body["status"].as_str() {
            if terminal_statuses.contains(&status) && !body["result"].is_null() {
                return body;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("background command {task_id} did not reach {terminal_statuses:?}");
}

fn restore_env_var(name: &str, value: Option<std::ffi::OsString>) {
    if let Some(value) = value {
        env::set_var(name, value);
    } else {
        env::remove_var(name);
    }
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn example_config() -> Value {
    serde_yaml::from_str::<ProjectConfig>(include_str!("../../../examples/coder.yaml"))
        .map(|config| serde_json::to_value(config).unwrap())
        .unwrap()
}

fn skill_context_modifier_fixture(allowed_tools: impl IntoIterator<Item = &'static str>) -> Value {
    let allowed_tools = allowed_tools.into_iter().collect::<Vec<_>>();
    json!({
        "contract": "coder.model_tool_turn_attachment.v1",
        "source": "coder-server",
        "type": "skill_context_modifier",
        "modifier_contract": "coder.skill_context_modifier.v1",
        "tool_use_id": "toolu-fixture-skill",
        "tool_name": "Skill",
        "skill_name": "fixture-skill",
        "display_name": "Fixture Skill",
        "applies_to": "next_model_turn",
        "application_status": "propagated_for_next_model_turn",
        "modifier": {
            "allowed_tools": allowed_tools,
            "model": Value::Null,
            "effort": Value::Null
        }
    })
}

fn temp_root() -> PathBuf {
    static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    test_tmp_root().join(format!("coder-server-{}-{}", std::process::id(), id))
}

fn test_tmp_root() -> PathBuf {
    std::env::var_os("CODER_TEST_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn run_git(repo: &PathBuf, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_git_capture(repo: &PathBuf, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn platform_echo_args(text: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            "cmd.exe".to_owned(),
            "/C".to_owned(),
            "echo".to_owned(),
            text.to_owned(),
        ]
    } else {
        vec!["sh".to_owned(), "-c".to_owned(), format!("printf {text}")]
    }
}

fn platform_write_file_args(path: &str, text: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            "powershell.exe".to_owned(),
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!("Set-Content -LiteralPath {path:?} -Value {text:?} -NoNewline -Encoding UTF8"),
        ]
    } else {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("printf '%s' '{}' > '{}'", text, path),
        ]
    }
}

fn platform_delayed_echo_args(text: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            "powershell.exe".to_owned(),
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!("Start-Sleep -Milliseconds 300; Write-Output {text}"),
        ]
    } else {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("sleep 0.3; printf {text}"),
        ]
    }
}

fn platform_stdin_echo_args() -> Vec<String> {
    if cfg!(windows) {
        vec![
            "powershell.exe".to_owned(),
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "$line = [Console]::In.ReadLine(); Write-Output ('got:' + $line)".to_owned(),
        ]
    } else {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "IFS= read -r line; printf 'got:%s\\n' \"$line\"".to_owned(),
        ]
    }
}

fn platform_spawn_child_args() -> Vec<String> {
    if cfg!(windows) {
        vec![
            "powershell.exe".to_owned(),
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "$p = Start-Process powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 30' -WindowStyle Hidden -PassThru; Set-Content -LiteralPath 'child.pid' -Value $p.Id -NoNewline; Start-Sleep -Seconds 30".to_owned(),
        ]
    } else {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "sleep 30 & echo $! > child.pid; wait".to_owned(),
        ]
    }
}

fn process_is_running(pid: u32) -> bool {
    if cfg!(windows) {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
    } else {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }
}

fn platform_sleep_args() -> Vec<String> {
    if cfg!(windows) {
        vec![
            "powershell.exe".to_owned(),
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "Start-Sleep -Seconds 5".to_owned(),
        ]
    } else {
        vec!["sleep".to_owned(), "5".to_owned()]
    }
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}
