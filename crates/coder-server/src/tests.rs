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
use super::planner_provider_dispatch::{planner_event_text, NativePlannerContextAdapter};
use super::planner_provider_recovery::PlannerProviderRequestMode;
use super::planner_provider_runtime::{
    parse_live_planner_response_with_idle_timeout, planner_chat_completion_body,
    planner_provider_trace,
};
use super::planner_session::{
    store_planner_session_snapshot, trim_planner_session_turns, PLANNER_SESSION_CACHE_LIMIT,
    PLANNER_SESSION_MAX_TURNS,
};
use super::provider_runtime::{
    provider_api_key, provider_chat_completions_endpoint,
    provider_chat_completions_endpoint_for_display, provider_http_client_builder,
    provider_proxy_mode, provider_proxy_url_for_url, provider_should_bypass_proxy,
};
use super::provider_settings::{apply_provider_settings_patch, provider_test_chat_completion_body};
use super::*;

type CaptureState = Arc<(Value, Arc<Mutex<Option<Value>>>)>;
type SequenceCaptureState = Arc<(Mutex<VecDeque<Value>>, Arc<Mutex<Vec<Value>>>)>;
type CommandLoopState = Arc<(Vec<String>, Arc<Mutex<Vec<Value>>>)>;
type StatusSequenceState = Arc<(
    Mutex<VecDeque<OpenAiCompatibleStatusResponse>>,
    Arc<Mutex<Vec<Value>>>,
)>;

fn planner_session_fixture(session_id: impl Into<String>) -> PlannerChatSession {
    PlannerChatSession {
        session_id: session_id.into(),
        workflow_id: "planner-led".to_owned(),
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
    assert_eq!(
        body["workflow"]["nodes"][1]["harness"],
        "browser-verification"
    );
    assert_eq!(body["workflow"]["nodes"][2]["harness"], "workflow-planner");
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
fn planner_chat_runtime_prefers_chat_planner_over_workflow_planner() {
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
        .contains("output_contract 'planner_conversation'"));
}

#[test]
fn native_planner_context_adapter_uses_no_execution_tools() {
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
    assert_eq!(
        context["planner_tool_policy"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
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
    assert!(system_text.contains("native executor"));
    assert!(system_text.contains("traceable to an observable acceptance criterion"));
    assert!(!system_text.contains("repo_search"));
    assert!(!system_text.contains("git_diff"));
    assert!(!system_text.contains("tools: memory_read"));
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

#[tokio::test]
async fn mcp_manifest_validate_endpoint_forces_defaults_off() {
    let app = test_router();
    let response = post_json(
        app,
        "/api/v3/mcp/manifests/validate",
        json!({
            "manifest": {
                "server_id": "github",
                "name": "GitHub",
                "enabled_by_default": true,
                "operations": [
                    {
                        "name": "search_issues",
                        "risk": "low",
                        "side_effect": "read",
                        "enabled_by_default": true
                    }
                ]
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["manifest"]["enabled_by_default"], false);
    assert_eq!(
        body["manifest"]["operations"][0]["enabled_by_default"],
        false
    );
    assert!(body["warnings"].as_array().unwrap().len() >= 2);
}

#[tokio::test]
async fn mcp_server_and_tool_endpoints_show_disabled_mock_baseline() {
    let app = test_router();
    let servers_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/mcp/servers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let tools_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/mcp/tools")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(servers_response.status(), StatusCode::OK);
    assert_eq!(tools_response.status(), StatusCode::OK);
    let servers = response_json(servers_response).await;
    let tools = response_json(tools_response).await;
    assert_eq!(servers["servers"][0]["server_id"], "local-mock");
    assert_eq!(servers["servers"][0]["enabled"], false);
    assert_eq!(tools["tools"][0]["enabled"], false);
    assert_eq!(tools["tools"][0]["requires_approval"], true);
    assert!(tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "mock.echo"));
}

#[tokio::test]
async fn mcp_tool_invoke_blocks_unapproved_and_records_approval_events() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    store.write_metadata(&state).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/mcp/tools/invoke",
        json!({
            "server_id": "local-mock",
            "tool_name": "mock.echo",
            "args": {"message": "hello"},
            "run_id": "run-1",
            "approved": false
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert_eq!(body["requires_approval"], true);
    assert_eq!(body["approval_key"], "mcp:local-mock:mock.echo");
    let events = store.read_events(&run_id).unwrap();
    let kinds = events
        .iter()
        .map(|event| event.kind.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            "mcp.server.registered",
            "mcp.tool.discovered",
            "mcp.approval.requested",
            "mcp.tool.blocked"
        ]
    );
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn mcp_tool_invoke_completes_echo_redacts_secrets_and_records_events() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    store.write_metadata(&state).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/mcp/tools/invoke",
        json!({
            "server_id": "local-mock",
            "tool_name": "mock.echo",
            "args": {"message": "hello", "api_key": "sk-secret-value"},
            "run_id": "run-1",
            "approved": true
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(body["output"]["echo"]["message"], "hello");
    assert_eq!(body["output"]["echo"]["api_key"], "[REDACTED]");
    assert!(!body.to_string().contains("sk-secret-value"));
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| event.kind == "mcp.tool.started"));
    assert!(events
        .iter()
        .any(|event| event.kind == "mcp.tool.completed"));
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("sk-secret-value"));
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn mcp_tool_invoke_failure_large_output_unknown_and_external_effect_are_safe() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
    store.write_metadata(&state).unwrap();
    let app = router(ApiState::new(store.clone()));

    let failure = post_json(
        app.clone(),
        "/api/v3/mcp/tools/invoke",
        json!({
            "server_id": "local-mock",
            "tool_name": "mock.fail",
            "args": {},
            "run_id": "run-1",
            "approved": true
        }),
    )
    .await;
    let large = post_json(
        app.clone(),
        "/api/v3/mcp/tools/invoke",
        json!({
            "server_id": "local-mock",
            "tool_name": "mock.large_output",
            "args": {},
            "run_id": "run-1",
            "approved": true
        }),
    )
    .await;
    let unknown = post_json(
        app.clone(),
        "/api/v3/mcp/tools/invoke",
        json!({
            "server_id": "local-mock",
            "tool_name": "mock.unknown",
            "args": {},
            "run_id": "run-1",
            "approved": true
        }),
    )
    .await;
    let external_unapproved = post_json(
        app,
        "/api/v3/mcp/tools/invoke",
        json!({
            "server_id": "local-mock",
            "tool_name": "mock.external_effect",
            "args": {},
            "run_id": "run-1",
            "approved": false
        }),
    )
    .await;

    let failure_body = response_json(failure).await;
    let large_body = response_json(large).await;
    let unknown_body = response_json(unknown).await;
    let external_body = response_json(external_unapproved).await;
    assert_eq!(failure_body["status"], "failed");
    assert!(failure_body["evidence_ref"]
        .as_str()
        .unwrap()
        .starts_with("blob://sha256/"));
    assert_eq!(large_body["status"], "completed");
    assert!(large_body["evidence_ref"]
        .as_str()
        .unwrap()
        .starts_with("blob://sha256/"));
    assert_eq!(large_body["output"]["truncated"], true);
    assert!(!large_body.to_string().contains(&"x".repeat(2048)));
    assert_eq!(unknown_body["status"], "failed");
    assert_eq!(external_body["status"], "blocked");
    assert_eq!(external_body["requires_approval"], true);

    let events = store.read_events(&run_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == "mcp.tool.failed" && !event.refs.is_empty()));
    let large_event = events
        .iter()
        .find(|event| {
            event.kind == "mcp.tool.completed" && event.payload["tool_name"] == "mock.large_output"
        })
        .unwrap();
    assert!(large_event.payload["evidence_ref"]
        .as_str()
        .unwrap()
        .starts_with("blob://sha256/"));
    assert!(!large_event.payload.to_string().contains(&"x".repeat(2048)));
    let _ = fs::remove_dir_all(root);
}

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

#[tokio::test]
async fn harness_tools_endpoint_filters_code_worker_tools() {
    let app = test_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/harness/tools?harness_id=code-worker-harness")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let tool_names = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["capability"]["name"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(tool_names.contains("run_command_sandbox"));
    assert!(tool_names.contains("command_background"));
    assert!(tool_names.contains("read_command_output"));
    assert!(tool_names.contains("cancel_command_background"));
    assert!(tool_names.contains("agent_subagent"));
    assert!(tool_names.contains("read_subagent_status"));
    assert!(tool_names.contains("cancel_subagent_background"));
    assert!(!tool_names.contains("inspect_run_state"));
    let background_tool = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["capability"]["name"] == "command_background")
        .unwrap();
    assert_eq!(background_tool["requires_approval"], true);
    assert_eq!(background_tool["required_permission"], "run_commands");
    assert_eq!(background_tool["evidence_emitted"], "command_evidence");
    assert_eq!(background_tool["timeline_item"], "background_command_start");
    let patch_tool = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["capability"]["name"] == "apply_patch_sandbox")
        .unwrap();
    assert_eq!(patch_tool["requires_approval"], true);
    assert_eq!(patch_tool["required_permission"], "write_files");
    assert_eq!(
        patch_tool["evidence_emitted"],
        "repo_evidence + patch_evidence"
    );
    assert_eq!(patch_tool["timeline_item"], "file_change / approval");
}

#[tokio::test]
async fn provider_settings_endpoints_store_secret_refs_without_returning_keys() {
    let app = router(ApiState::new(RunStore::new(temp_root())));
    let initial = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/providers/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let initial_body = response_json(initial).await;
    assert_eq!(initial_body["settings"]["default_provider"], "deepseek");
    assert_eq!(
        initial_body["settings"]["default_model"],
        "deepseek-v4-flash"
    );
    assert_eq!(initial_body["settings"]["mock_mode"], false);
    assert_eq!(
        initial_body["settings"]["proxy_modes"]["deepseek"],
        Value::Null
    );

    let save = post_json(
        app.clone(),
        "/api/v3/providers/settings",
        json!({
            "default_provider": "deepseek",
            "default_model": "deepseek-chat",
            "base_urls": {"deepseek": "https://api.deepseek.com"},
            "proxy_urls": {"deepseek": "http://127.0.0.1:7890"},
            "api_keys": {"deepseek": "sk-secret-value"},
            "mock_mode": false
        }),
    )
    .await;
    assert_eq!(save.status(), StatusCode::OK);
    let save_body = response_json(save).await;
    assert_eq!(save_body["settings"]["default_provider"], "deepseek");
    assert_eq!(
        save_body["settings"]["api_keys"]["deepseek"]["configured"],
        true
    );
    assert_eq!(
        save_body["settings"]["api_keys"]["deepseek"]["source"],
        "settings"
    );
    assert!(!save_body.to_string().contains("sk-secret-value"));
    assert_eq!(save_body["status"]["default_model"], "deepseek-chat");
    assert_eq!(
        save_body["status"]["default_status"]["base_url"],
        "https://api.deepseek.com"
    );
    assert_eq!(
        save_body["status"]["default_status"]["proxy_url"],
        "http://127.0.0.1:7890/"
    );
    assert_eq!(
        save_body["status"]["default_status"]["proxy_mode"],
        "explicit"
    );

    let test = post_json(
        app.clone(),
        "/api/v3/providers/test",
        json!({"provider": "deepseek", "mock": true}),
    )
    .await;
    let test_body = response_json(test).await;
    assert_eq!(test_body["status"]["providers"][0]["provider"], "deepseek");
    assert_eq!(
        test_body["status"]["providers"][0]["credential_configured"],
        true
    );
    assert_eq!(test_body["test"]["ok"], true);
    assert_eq!(test_body["test"]["mode"], "mock");
    assert_eq!(test_body["test"]["model"], "deepseek-chat");
    assert_eq!(test_body["test"]["endpoint"], Value::Null);
    assert!(!test_body.to_string().contains("sk-secret-value"));

    let remove = post_json(
        app,
        "/api/v3/providers/settings",
        json!({
            "api_keys": {"deepseek": null}
        }),
    )
    .await;
    let remove_body = response_json(remove).await;
    assert!(remove_body["settings"]["api_keys"]["deepseek"].is_null());
}

#[test]
fn provider_settings_patch_updates_clears_and_overrides_env_fallback() {
    let env_name = "CODER_TEST_PROVIDER_KEY_OVERRIDE";
    let previous = env::var_os(env_name);
    env::set_var(env_name, "env-key-value");
    let mut settings = ProviderSettings::default();

    apply_provider_settings_patch(
        &mut settings,
        ProviderSettingsPatch {
            default_provider: Some("openai-compatible".to_owned()),
            default_model: None,
            base_urls: None,
            proxy_urls: None,
            proxy_modes: None,
            api_keys: Some(BTreeMap::from([(
                "openai-compatible".to_owned(),
                json!("settings-key-value"),
            )])),
            mock_mode: None,
        },
    );
    assert_eq!(
        provider_api_key(&settings, "openai-compatible", Some(env_name)),
        Some(("settings-key-value".to_owned(), "settings".to_owned()))
    );

    apply_provider_settings_patch(
        &mut settings,
        ProviderSettingsPatch {
            default_provider: None,
            default_model: None,
            base_urls: None,
            proxy_urls: None,
            proxy_modes: None,
            api_keys: Some(BTreeMap::from([(
                "openai-compatible".to_owned(),
                json!("updated-settings-key"),
            )])),
            mock_mode: None,
        },
    );
    assert_eq!(
        provider_api_key(&settings, "openai-compatible", Some(env_name)),
        Some(("updated-settings-key".to_owned(), "settings".to_owned()))
    );

    apply_provider_settings_patch(
        &mut settings,
        ProviderSettingsPatch {
            default_provider: None,
            default_model: None,
            base_urls: None,
            proxy_urls: None,
            proxy_modes: None,
            api_keys: Some(BTreeMap::from([(
                "openai-compatible".to_owned(),
                Value::Null,
            )])),
            mock_mode: None,
        },
    );
    assert_eq!(
        provider_api_key(&settings, "openai-compatible", Some(env_name)),
        Some(("env-key-value".to_owned(), "environment".to_owned()))
    );
    if let Some(previous) = previous {
        env::set_var(env_name, previous);
    } else {
        env::remove_var(env_name);
    }
}

#[test]
fn provider_proxy_modes_isolate_deepseek_and_allow_explicit_proxy() {
    let mut settings = ProviderSettings::default();
    let deepseek_url = "https://api.deepseek.com/chat/completions";
    let generic_url = "https://api.example.invalid/chat/completions";
    let proxy_env_name = "CODER_PROXY_TEST_PROVIDER_PROXY_URL";
    let previous_proxy = env::var_os(proxy_env_name);
    let previous_no_proxy = env::var_os("no_proxy");
    env::set_var(proxy_env_name, "http://env-proxy.invalid:8080");
    env::remove_var("no_proxy");

    assert_eq!(provider_proxy_mode(&settings, "deepseek"), "direct");
    assert_eq!(
        provider_proxy_url_for_url(&settings, "deepseek", Some(deepseek_url)),
        None
    );
    assert_eq!(
        provider_proxy_mode(&settings, "openai-compatible"),
        "environment"
    );

    settings
        .proxy_urls
        .insert("deepseek".to_owned(), "http://127.0.0.1:7890".to_owned());
    assert_eq!(provider_proxy_mode(&settings, "deepseek"), "explicit");
    assert_eq!(
        provider_proxy_url_for_url(&settings, "deepseek", Some(deepseek_url)),
        Some("http://127.0.0.1:7890".to_owned())
    );

    settings
        .proxy_modes
        .insert("deepseek".to_owned(), "direct".to_owned());
    assert_eq!(provider_proxy_mode(&settings, "deepseek"), "direct");
    assert_eq!(
        provider_proxy_url_for_url(&settings, "deepseek", Some(deepseek_url)),
        None
    );

    settings.proxy_urls.insert(
        "openai-compatible".to_owned(),
        "http://127.0.0.1:8080".to_owned(),
    );
    assert_eq!(
        provider_proxy_url_for_url(&settings, "openai-compatible", Some(generic_url)),
        Some("http://127.0.0.1:8080".to_owned())
    );

    assert_eq!(
        provider_proxy_mode(&settings, "proxy-test-provider"),
        "environment"
    );
    assert_eq!(
        provider_proxy_url_for_url(
            &settings,
            "proxy-test-provider",
            Some("https://api.proxy-test.invalid/v1/chat/completions")
        ),
        Some("http://env-proxy.invalid:8080".to_owned())
    );
    env::set_var("no_proxy", "api.proxy-test.invalid");
    assert_eq!(
        provider_proxy_url_for_url(
            &settings,
            "proxy-test-provider",
            Some("https://api.proxy-test.invalid/v1/chat/completions")
        ),
        None
    );
    restore_env_var(proxy_env_name, previous_proxy);
    restore_env_var("no_proxy", previous_no_proxy);
}

#[test]
fn provider_no_proxy_matching_follows_claude_style_rules() {
    assert!(provider_should_bypass_proxy(
        "https://api.deepseek.com/chat/completions",
        Some("api.deepseek.com")
    ));
    assert!(provider_should_bypass_proxy(
        "https://sub.internal.example/v1",
        Some(".internal.example")
    ));
    assert!(provider_should_bypass_proxy(
        "https://api.example.com:8443/v1",
        Some("api.example.com:8443")
    ));
    assert!(!provider_should_bypass_proxy(
        "https://notinternal.example/v1",
        Some(".internal.example")
    ));
}

#[test]
fn provider_key_state_serialization_redacts_secret() {
    let mut settings = ProviderSettings::default();
    settings.api_keys.insert(
        "openai-compatible".to_owned(),
        ProviderKeyState {
            configured: true,
            source: "settings".to_owned(),
            secret: Some("sk-secret-value".to_owned()),
        },
    );

    let serialized = serde_json::to_string(&settings).unwrap();

    assert!(serialized.contains("\"configured\":true"));
    assert!(serialized.contains("\"source\":\"settings\""));
    assert!(!serialized.contains("sk-secret-value"));
    assert!(!serialized.contains("secret"));
}

#[test]
fn provider_test_endpoint_display_redacts_url_credentials() {
    assert_eq!(
        provider_chat_completions_endpoint(
            "https://user:secret@api.deepseek.com/v1?token=secret#fragment",
        ),
        "https://api.deepseek.com/v1/chat/completions"
    );
    assert_eq!(
        provider_chat_completions_endpoint_for_display(
            "https://user:secret@api.deepseek.com/v1?token=secret#fragment",
        ),
        "https://api.deepseek.com/v1/chat/completions"
    );
}

#[test]
fn provider_test_body_disables_deepseek_thinking_for_short_probe() {
    let deepseek = provider_test_chat_completion_body("deepseek", "deepseek-v4-flash");
    assert_eq!(deepseek["model"], "deepseek-v4-flash");
    assert_eq!(deepseek["max_tokens"], 32);
    assert_eq!(deepseek["thinking"]["type"], "disabled");

    let generic = provider_test_chat_completion_body("openai-compatible", "gpt-compatible-test");
    assert_eq!(generic["model"], "gpt-compatible-test");
    assert!(generic.get("thinking").is_none());
}

#[test]
fn planner_chat_body_bounds_tokens_and_disables_deepseek_thinking() {
    let messages = vec![json!({
        "role": "user",
        "content": "challenge question"
    })];
    let deepseek = planner_chat_completion_body(
        "deepseek",
        "deepseek-v4-flash",
        messages.clone(),
        PLANNER_CHAT_MAX_OUTPUT_TOKENS_DEFAULT,
        None,
    );
    assert_eq!(deepseek["model"], "deepseek-v4-flash");
    assert_eq!(deepseek["temperature"], 0.2);
    assert_eq!(deepseek["max_tokens"], 900);
    assert_eq!(deepseek["thinking"]["type"], "disabled");
    assert_eq!(deepseek["response_format"]["type"], "json_object");
    assert_eq!(deepseek["messages"], json!(messages));

    let generic = planner_chat_completion_body(
        "openai-compatible",
        "gpt-compatible-test",
        Vec::new(),
        1_200,
        Some("max"),
    );
    assert_eq!(generic["model"], "gpt-compatible-test");
    assert_eq!(generic["max_tokens"], 1_200);
    assert_eq!(generic["reasoning_effort"], "xhigh");
    assert!(generic.get("thinking").is_none());
    assert!(generic.get("response_format").is_none());

    let deepseek_effort = planner_chat_completion_body(
        "deepseek",
        "deepseek-v4-flash",
        Vec::new(),
        1_200,
        Some("medium"),
    );
    assert_eq!(deepseek_effort["thinking"]["type"], "enabled");
    assert!(deepseek_effort.get("reasoning_effort").is_none());
}

#[test]
fn planner_provider_retention_policy_matches_claude_prompt_dump_bounds() {
    assert_eq!(CLAUDE_DUMP_PROMPTS_MAX_CACHED_REQUESTS, 5);
    assert_eq!(PLANNER_PROVIDER_RETAINED_FULL_REQUEST_BODIES, 1);
    assert_eq!(PLANNER_PROVIDER_RESPONSE_MAX_BYTES, 2 * 1024 * 1024);
    assert_eq!(
        PLANNER_PROVIDER_STREAM_PENDING_MAX_BYTES,
        PLANNER_PROVIDER_RESPONSE_MAX_BYTES
    );
}

#[tokio::test]
async fn planner_provider_rejects_oversized_non_streaming_response() {
    let oversized_content = "x".repeat(PLANNER_PROVIDER_RESPONSE_MAX_BYTES + 1);
    let provider_base_url = spawn_openai_compatible_test_server_with_payload(json!({
        "choices": [
            {
                "message": {
                    "content": oversized_content
                }
            }
        ]
    }))
    .await;
    let url = provider_chat_completions_endpoint(&provider_base_url);
    let response = provider_http_client_builder(&url, None)
        .unwrap()
        .build()
        .unwrap()
        .post(url)
        .json(&json!({}))
        .send()
        .await
        .unwrap();

    let error = parse_live_planner_response_with_idle_timeout(
        response,
        &[],
        planner_provider_trace(true, "unknown", false, None),
        Duration::from_secs(90),
    )
    .await
    .unwrap_err();

    assert!(
        error.contains("planner model response exceeded"),
        "unexpected planner response error: {error}"
    );
    assert!(error.contains(&PLANNER_PROVIDER_RESPONSE_MAX_BYTES.to_string()));
}

#[tokio::test]
async fn planner_provider_rejects_oversized_streaming_pending_line() {
    let provider_base_url = spawn_raw_openai_compatible_test_server(
        StatusCode::OK,
        "text/event-stream",
        format!(
            "data: {}",
            "x".repeat(PLANNER_PROVIDER_STREAM_PENDING_MAX_BYTES + 1)
        ),
    )
    .await;
    let url = provider_chat_completions_endpoint(&provider_base_url);
    let response = provider_http_client_builder(&url, None)
        .unwrap()
        .build()
        .unwrap()
        .post(url)
        .json(&json!({}))
        .send()
        .await
        .unwrap();

    let error = parse_live_planner_response_with_idle_timeout(
        response,
        &[],
        planner_provider_trace(true, "unknown", false, None),
        Duration::from_secs(90),
    )
    .await
    .unwrap_err();

    assert!(
        error.contains("planner streaming response pending line exceeded"),
        "unexpected planner streaming error: {error}"
    );
    assert!(error.contains(&PLANNER_PROVIDER_STREAM_PENDING_MAX_BYTES.to_string()));
}

#[test]
fn provider_settings_resolve_alias_models_without_overwriting_explicit_models_or_secrets() {
    let mut config = default_project_config();
    config.models.insert(
        "economy_alias".to_owned(),
        ConfigModelSpec {
            provider: "openai-compatible".to_owned(),
            model: "economy".to_owned(),
            base_url_env: Some("SECONDARY_BASE_URL".to_owned()),
            api_key_env: Some("SECONDARY_API_KEY".to_owned()),
        },
    );
    config.models.insert(
        "hook_agent".to_owned(),
        ConfigModelSpec {
            provider: "openai-compatible".to_owned(),
            model: "agent-hook-model".to_owned(),
            base_url_env: None,
            api_key_env: None,
        },
    );
    let mut settings = ProviderSettings {
        default_provider: "deepseek".to_owned(),
        default_model: "deepseek-v4-flash".to_owned(),
        ..ProviderSettings::default()
    };
    settings.api_keys.insert(
        "deepseek".to_owned(),
        ProviderKeyState {
            configured: true,
            source: "settings".to_owned(),
            secret: Some("sk-secret-value".to_owned()),
        },
    );

    apply_provider_settings_to_project_config(&mut config, &settings);

    for model_id in ["default", "planner_chat", "economy_alias"] {
        let model = config.models.get(model_id).unwrap();
        assert_eq!(model.provider, "deepseek", "{model_id}");
        assert_eq!(model.model, "deepseek-v4-flash", "{model_id}");
    }
    let hook_agent = config.models.get("hook_agent").unwrap();
    assert_eq!(hook_agent.provider, "openai-compatible");
    assert_eq!(hook_agent.model, "agent-hook-model");
    assert!(!serde_json::to_string(&config)
        .unwrap()
        .contains("sk-secret-value"));
}

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

#[tokio::test]
async fn subagent_run_endpoint_spawns_child_and_records_sidechain() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let state = ApiState::new(store.clone());
    state.provider_settings.lock().unwrap().mock_mode = true;
    let app = router(state.clone());
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("review-only").unwrap();
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
            "parent_harness_id": "review-only",
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
    let harness = config.harnesses.get_mut("review-only").unwrap();
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
            "parent_harness_id": "review-only",
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
    let harness = config.harnesses.get_mut("review-only").unwrap();
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
            "parent_harness_id": "review-only",
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
        parent_harness_id: "review-only".to_owned(),
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
        parent_harness_id: "review-only".to_owned(),
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
    assert!(storage["claude_sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source.as_str() == Some("src/utils/toolResultStorage.ts")));

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
                    "workflow_id": "planner-led",
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
                    "workflow_id": "planner-led",
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
    config.agents.insert(
        "general-purpose".to_owned(),
        coder_config::AgentSpec {
            role: "executor".to_owned(),
            model: "default".to_owned(),
            system: "General purpose skill subagent.".to_owned(),
            tools: Default::default(),
            disallowed_tools: Default::default(),
            memory: Default::default(),
            output_contract: "implementation_report".to_owned(),
            runtime: Default::default(),
        },
    );
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
            claude_sources: Vec::new(),
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
    assert!(phases[1]["claude_sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source.as_str().unwrap().contains("toolExecution.ts")));

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
    let executor = config.agents.get_mut("executor").unwrap();
    executor.runtime.context_window_tokens = 32_000;
    executor.runtime.autocompact_buffer_tokens = 1_000;
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
    assert_eq!(decision["runtime_autocompact_buffer_tokens"], 1_000);
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
    let executor = config.agents.get_mut("executor").unwrap();
    executor.runtime.context_window_tokens = 32_000;
    executor.runtime.autocompact_buffer_tokens = 1_000;
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
    assert_eq!(decision.runtime_autocompact_buffer_tokens, 1_000);
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

#[tokio::test]
async fn model_tool_command_hooks_receive_claude_style_stdin() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-hook-stdin");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Hook stdin\n").unwrap();
    let stdin_capture = repo.join("hook-input.json");
    let hook_command = hook_capture_stdin_command(&stdin_capture);
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("repo_read_file".to_owned()),
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
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-hook-stdin",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-hook-stdin"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    let captured = fs::read_to_string(&stdin_capture).unwrap();
    let hook_input =
        serde_json::from_str::<Value>(captured.trim_start_matches('\u{feff}').trim()).unwrap();
    assert_eq!(hook_input["hook_event_name"], "PreToolUse");
    assert_eq!(hook_input["tool_name"], "repo_read_file");
    assert_eq!(hook_input["tool_use_id"], "toolu-hook-stdin");
    assert_eq!(hook_input["tool_input"]["path"], "README.md");
    assert_eq!(hook_input["session_id"], "run-model-tool-hook-stdin");
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(
        phases[0]["hook_results"][0]["stdin_protocol"],
        "claude.hook_input.v1"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "plain_text"
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[test]
fn model_tool_hook_output_parser_applies_pretool_effects() {
    let output = json!({
        "continue": false,
        "stopReason": "policy stopped follow-up",
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "ask",
            "permissionDecisionReason": "needs review",
            "updatedInput": {
                "path": "after.txt"
            },
            "additionalContext": "rewrote path"
        }
    })
    .to_string();

    let parsed = crate::model_tool_hook_output::parse_model_tool_hook_output(
        &output,
        coder_config::HookEvent::PreToolUse,
        "hook-command",
    );

    assert_eq!(parsed.kind, "hook_json");
    assert_eq!(parsed.validation_error, None);
    assert_eq!(parsed.blocking_error, None);
    assert_eq!(parsed.effects.permission_behavior, Some("ask"));
    assert_eq!(
        parsed.effects.permission_decision_reason.as_deref(),
        Some("needs review")
    );
    assert_eq!(parsed.effects.updated_input.unwrap()["path"], "after.txt");
    assert_eq!(
        parsed.effects.additional_context.unwrap(),
        json!("rewrote path")
    );
    assert!(parsed.effects.prevent_continuation);
    assert_eq!(
        parsed.effects.stop_reason.as_deref(),
        Some("policy stopped follow-up")
    );
}

#[test]
fn model_tool_hook_output_parser_rejects_non_object_updated_input() {
    let output = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "updatedInput": "not an object"
        }
    })
    .to_string();

    let parsed = crate::model_tool_hook_output::parse_model_tool_hook_output(
        &output,
        coder_config::HookEvent::PreToolUse,
        "hook-command",
    );

    assert_eq!(parsed.kind, "invalid_hook_json");
    assert_eq!(
        parsed.validation_error.as_deref(),
        Some("hookSpecificOutput.updatedInput must be an object")
    );
    assert_eq!(parsed.blocking_error, None);
}

#[test]
fn model_tool_command_hooks_parse_async_response_output() {
    let parsed = crate::model_tool_command_hooks::parse_async_hook_response_output(
        "debug line\n{\"systemMessage\":\"async note\"}\n",
    )
    .unwrap();
    assert_eq!(parsed.kind, "hook_json");
    assert_eq!(parsed.response["systemMessage"], "async note");

    let plain =
        crate::model_tool_command_hooks::parse_async_hook_response_output("plain hook output")
            .unwrap();
    assert_eq!(plain.kind, "plain_text");
    assert_eq!(plain.response, json!({}));

    assert!(crate::model_tool_command_hooks::parse_async_hook_response_output("   ").is_none());
}

#[test]
fn model_tool_command_hooks_build_shell_argv() {
    assert_eq!(
        crate::model_tool_command_hooks::CLAUDE_DEFAULT_HOOK_SHELL,
        "bash"
    );
    let bash = crate::model_tool_command_hooks::shell_command_hook_argv("echo ok", Some("bash"));
    assert_eq!(bash[1..], ["-lc", "echo ok"]);
    assert_ne!(bash[0], "cmd.exe");
    if !cfg!(windows) {
        assert_eq!(bash[0], "bash");
    }

    let powershell = crate::model_tool_command_hooks::shell_command_hook_argv(
        "Write-Output ok",
        Some("powershell"),
    );
    assert_eq!(
        powershell,
        vec![
            if cfg!(windows) {
                "powershell.exe"
            } else {
                "pwsh"
            },
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Write-Output ok",
        ]
    );

    assert_eq!(
        crate::model_tool_command_hooks::shell_command_hook_argv("Write-Output ok", Some("pwsh")),
        vec![
            "pwsh",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Write-Output ok",
        ]
    );

    let default = crate::model_tool_command_hooks::shell_command_hook_argv("echo ok", None);
    assert_eq!(default[1..], ["-lc", "echo ok"]);
    assert_ne!(default[0], "cmd.exe");
    if !cfg!(windows) {
        assert_eq!(default[0], "bash");
    }

    let unknown =
        crate::model_tool_command_hooks::shell_command_hook_argv("echo ok", Some("unknown"));
    assert_eq!(unknown[1..], ["-lc", "echo ok"]);
    assert_ne!(unknown[0], "cmd.exe");
}

#[test]
fn model_tool_command_hooks_follow_claude_windows_path_rules() {
    assert_eq!(
        crate::model_tool_command_hooks::windows_path_to_posix_path(r"C:\Users\foo"),
        "/c/Users/foo"
    );
    assert_eq!(
        crate::model_tool_command_hooks::windows_path_to_posix_path(r"D:\Work\project"),
        "/d/Work/project"
    );
    assert_eq!(
        crate::model_tool_command_hooks::windows_path_to_posix_path(r"\\server\share\dir"),
        "//server/share/dir"
    );
    assert_eq!(
        crate::model_tool_command_hooks::windows_path_to_posix_path(r"src\main.ts"),
        "src/main.ts"
    );
}

#[test]
fn model_tool_command_hooks_derive_git_bash_from_git_executable() {
    let git_path = std::path::Path::new("/opt/git/cmd/git.exe");
    let bash_path =
        crate::model_tool_command_hooks::git_bash_path_from_git_executable(git_path).unwrap();
    assert_eq!(bash_path, std::path::Path::new("/opt/git/bin/bash.exe"));
}

#[tokio::test]
async fn model_tool_pre_hook_json_updated_input_rewrites_tool_execution() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-hook-updated-input");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("before.txt"), "before\n").unwrap();
    fs::write(repo.join("after.txt"), "after\n").unwrap();
    let hook_output = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "updatedInput": {
                "repo_root": repo,
                "path": "after.txt",
                "run_id": "run-model-tool-hook-updated-input"
            },
            "additionalContext": "rewrote path"
        }
    });
    let hook_output_path = repo.join("hook-output.json");
    fs::write(
        &hook_output_path,
        serde_json::to_string(&hook_output).unwrap(),
    )
    .unwrap();
    let hook_command = hook_emit_file_command(&hook_output_path);
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
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
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-hook-updated-input",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "before.txt",
                "run_id": "run-model-tool-hook-updated-input"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"].as_str().unwrap().contains("after"));
    assert!(!body["content"].as_str().unwrap().contains("before"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["updated_input_applied"], true);
    assert_eq!(phases[0]["additional_contexts"][0], "rewrote path");
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "hook_json"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["updated_input"]["path"],
        "after.txt"
    );
    assert_eq!(phases[2]["phase"], "tool_execution");
    assert_eq!(phases[2]["status"], "completed");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_prompt_pre_hook_blocks_when_model_returns_not_ok() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-prompt-hook-block");
    let provider_base_url = spawn_openai_compatible_test_server_with_payload(json!({
        "choices": [
            {
                "message": {
                    "content": "{\"ok\": false, \"reason\": \"verifier rejected tool input\"}"
                }
            }
        ]
    }))
    .await;
    let mut config = default_project_config();
    config.models.insert(
        "hook_verifier".to_owned(),
        ConfigModelSpec {
            provider: "openai-compatible".to_owned(),
            model: "prompt-hook-model".to_owned(),
            base_url_env: None,
            api_key_env: None,
        },
    );
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Prompt {
                prompt: "Verify this tool input: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: Some(5),
                model: Some("hook_verifier".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Prompt hook\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-prompt-hook-block",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-prompt-hook-block"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("verifier rejected tool input"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "blocked");
    assert_eq!(phases[0]["prompt_hook_count"], 1);
    assert_eq!(phases[0]["unsupported_hook_count"], 0);
    assert_eq!(phases[0]["hook_results"][0]["type"], "prompt");
    assert_eq!(phases[0]["hook_results"][0]["outcome"], "blocking");
    assert_eq!(phases[0]["hook_results"][0]["model"], "prompt-hook-model");
    assert_eq!(
        phases[0]["hook_results"][0]["model_source"],
        "hook_config_model"
    );
    assert_eq!(phases[0]["hook_results"][0]["default_timeout_seconds"], 30);
    assert_eq!(
        phases[0]["hook_results"][0]["request_protocol"],
        "claude.hook_input.v1"
    );
    assert_eq!(phases[1]["phase"], "permission_decision");
    assert_eq!(phases[1]["status"], "skipped_pre_tool_use_hook_blocked");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_pre_hook_blocks_when_structured_output_not_ok() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-hook-block");
    let provider_base_url = spawn_openai_compatible_test_server_with_payload(json!({
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [
                        {
                            "id": "call-agent-structured-output",
                            "type": "function",
                            "function": {
                                "name": "StructuredOutput",
                                "arguments": "{\"ok\": false, \"reason\": \"agent rejected tool input\"}"
                            }
                        }
                    ]
                }
            }
        ]
    }))
    .await;
    let mut config = default_project_config();
    config.models.insert(
        "hook_agent".to_owned(),
        ConfigModelSpec {
            provider: "openai-compatible".to_owned(),
            model: "agent-hook-model".to_owned(),
            base_url_env: None,
            api_key_env: None,
        },
    );
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Agent hook block\n").unwrap();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Agent {
                prompt: "Verify this tool input: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: None,
                model: Some("hook_agent".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-hook-block",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-agent-hook-block"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("agent rejected tool input"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "blocked");
    assert_eq!(phases[0]["agent_hook_count"], 1);
    assert_eq!(phases[0]["unsupported_hook_count"], 0);
    assert_eq!(phases[0]["executed_hook_count"], 1);
    assert_eq!(phases[0]["execution_status"], "blocked");
    let hook_result = &phases[0]["hook_results"][0];
    assert_eq!(hook_result["type"], "agent");
    assert_eq!(hook_result["outcome"], "blocking");
    assert_eq!(hook_result["default_timeout_seconds"], 60);
    assert_eq!(hook_result["max_agent_turns"], 50);
    assert_eq!(hook_result["model"], "agent-hook-model");
    assert_eq!(hook_result["model_source"], "hook_config_model");
    assert_eq!(hook_result["structured_output_tool"], "StructuredOutput");
    assert_eq!(
        hook_result["hook_output_kind"],
        "agent_structured_output_tool"
    );
    assert_eq!(hook_result["hook_json_output"]["ok"], false);
    assert_eq!(
        hook_result["runtime_contract"]["isolated_agent_id_prefix"],
        "hook-agent-"
    );
    assert!(hook_result["processed_prompt_preview"]
        .as_str()
        .unwrap()
        .contains("\"path\":\"README.md\""));
    assert!(
        hook_result["available_tools_policy"]["filtered_tool_families"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str().unwrap().contains("agent_subagent"))
    );
    assert_eq!(phases[1]["phase"], "permission_decision");
    assert_eq!(phases[1]["status"], "skipped_pre_tool_use_hook_blocked");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_hook_sends_structured_output_tool_request_and_allows_ok() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-hook-request");
    let (provider_base_url, captured) = spawn_openai_compatible_capture_test_server(json!({
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [
                        {
                            "id": "call-agent-structured-output-ok",
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
    }))
    .await;
    let mut config = default_project_config();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Agent {
                prompt: "Check argument payload: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: None,
                model: Some("literal-agent-hook-model".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Agent hook request\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-hook-request",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-agent-hook-request"
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
        .contains("Agent hook request"));
    let captured = captured.lock().unwrap().clone().unwrap();
    assert_eq!(captured["model"], "literal-agent-hook-model");
    assert_eq!(captured["temperature"], 0);
    assert_eq!(captured["max_tokens"], 256);
    assert_eq!(captured["tool_choice"], "auto");
    assert_eq!(captured["tools"].as_array().unwrap().len(), 8);
    let tool_names = captured["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"StructuredOutput"));
    assert!(tool_names.contains(&"repo_read_file"));
    assert!(!tool_names.contains(&"agent_subagent"));
    assert!(!tool_names.contains(&"command_run"));
    assert_eq!(
        captured["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["function"]["name"] == "StructuredOutput")
            .unwrap()["function"]["parameters"]["required"],
        json!(["ok"])
    );
    let user_prompt = captured["messages"][1]["content"].as_str().unwrap();
    assert!(user_prompt.contains("Check argument payload:"));
    assert!(user_prompt.contains("\"hook_event_name\":\"PreToolUse\""));
    assert!(user_prompt.contains("\"tool_use_id\":\"toolu-agent-hook-request\""));
    assert!(user_prompt.contains("\"path\":\"README.md\""));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["hook_results"][0]["type"], "agent");
    assert_eq!(phases[0]["hook_results"][0]["outcome"], "success");
    assert_eq!(
        phases[0]["hook_results"][0]["model_source"],
        "hook_literal_model"
    );
    assert_eq!(phases[0]["hook_results"][0]["default_timeout_seconds"], 60);
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_hook_can_use_read_only_tool_before_structured_output() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-hook-tool-loop");
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Agent hook tool loop\n").unwrap();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-agent-read-file",
                                "type": "function",
                                "function": {
                                    "name": "repo_read_file",
                                    "arguments": json!({
                                        "repo_root": repo,
                                        "path": "README.md"
                                    }).to_string()
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
                                "id": "call-agent-structured-output-after-read",
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
        }),
    ])
    .await;
    let mut config = default_project_config();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Agent {
                prompt: "Inspect before allowing: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: None,
                model: Some("literal-agent-hook-model".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-hook-tool-loop",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-agent-hook-tool-loop"
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
        .contains("Agent hook tool loop"));
    let captured = captured.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    let second_messages = captured[1]["messages"].as_array().unwrap();
    assert!(second_messages.iter().any(|message| {
        message["role"] == "tool"
            && message["tool_call_id"] == "call-agent-read-file"
            && message["content"]
                .as_str()
                .unwrap()
                .contains("Agent hook tool loop")
    }));
    let phases = body["phases"].as_array().unwrap();
    let hook_result = &phases[0]["hook_results"][0];
    assert_eq!(hook_result["type"], "agent");
    assert_eq!(hook_result["outcome"], "success");
    assert_eq!(hook_result["assistant_turns"], 2);
    assert_eq!(hook_result["tool_call_count"], 2);
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_hook_malformed_structured_output_is_non_blocking() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-hook-malformed");
    let provider_base_url = spawn_openai_compatible_test_server_with_payload(json!({
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [
                        {
                            "id": "call-agent-malformed-structured-output",
                            "type": "function",
                            "function": {
                                "name": "StructuredOutput",
                                "arguments": "{\"reason\": \"missing ok\"}"
                            }
                        }
                    ]
                }
            }
        ]
    }))
    .await;
    let mut config = default_project_config();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Agent {
                prompt: "Malformed output should not block: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: Some(5),
                model: Some("literal-agent-hook-model".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Agent hook malformed\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-hook-malformed",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-agent-hook-malformed"
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
        .contains("Agent hook malformed"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["hook_results"][0]["type"], "agent");
    assert_eq!(
        phases[0]["hook_results"][0]["outcome"],
        "non_blocking_error"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "invalid_agent_hook_structured_output"
    );
    assert!(phases[0]["hook_results"][0]["hook_output_validation_error"]
        .as_str()
        .unwrap()
        .contains("boolean field 'ok'"));
    assert_eq!(phases[0]["hook_results"][0]["assistant_turns"], 1);
    assert_eq!(phases[1]["phase"], "permission_decision");
    assert_eq!(phases[1]["status"], "delegated_to_tool_endpoint");
    assert_eq!(phases[2]["phase"], "tool_execution");
    assert_eq!(phases[2]["status"], "completed");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_hook_provider_timeout_is_non_blocking() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-hook-timeout");
    let provider_base_url = spawn_delayed_openai_compatible_test_server(
        Duration::from_millis(1_500),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-agent-timeout-structured-output",
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
        }),
    )
    .await;
    let mut config = default_project_config();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Agent {
                prompt: "Timeout should not block: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: Some(1),
                model: Some("literal-agent-hook-model".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Agent hook timeout\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-hook-timeout",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-agent-hook-timeout"
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
        .contains("Agent hook timeout"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["hook_results"][0]["type"], "agent");
    assert_eq!(
        phases[0]["hook_results"][0]["outcome"],
        "non_blocking_error"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "request_timeout"
    );
    assert_eq!(phases[0]["hook_results"][0]["timeout_seconds"], 1);
    let validation_error = phases[0]["hook_results"][0]["hook_output_validation_error"]
        .as_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(validation_error.contains("timed out after 1 second(s)"));
    assert_eq!(phases[2]["phase"], "tool_execution");
    assert_eq!(phases[2]["status"], "completed");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_prompt_hook_sends_claude_arguments_and_schema_request() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-prompt-hook-request");
    let (provider_base_url, captured) = spawn_openai_compatible_capture_test_server(json!({
        "choices": [
            {
                "message": {
                    "content": "{\"ok\": true}"
                }
            }
        ]
    }))
    .await;
    let mut config = default_project_config();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Prompt {
                prompt: "Check argument payload: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: None,
                model: Some("literal-hook-model".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Prompt hook request\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-prompt-hook-request",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-prompt-hook-request"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    let captured = captured.lock().unwrap().clone().unwrap();
    assert_eq!(captured["model"], "literal-hook-model");
    assert_eq!(captured["temperature"], 0);
    assert_eq!(captured["max_tokens"], 256);
    assert_eq!(captured["response_format"]["type"], "json_schema");
    assert_eq!(
        captured["response_format"]["json_schema"]["schema"]["required"],
        json!(["ok"])
    );
    let user_prompt = captured["messages"][1]["content"].as_str().unwrap();
    assert!(user_prompt.contains("Check argument payload:"));
    assert!(user_prompt.contains("\"hook_event_name\":\"PreToolUse\""));
    assert!(user_prompt.contains("\"tool_use_id\":\"toolu-prompt-hook-request\""));
    assert!(user_prompt.contains("\"path\":\"README.md\""));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["hook_results"][0]["type"], "prompt");
    assert_eq!(phases[0]["hook_results"][0]["outcome"], "success");
    assert_eq!(
        phases[0]["hook_results"][0]["model_source"],
        "hook_literal_model"
    );
    assert_eq!(phases[0]["hook_results"][0]["default_timeout_seconds"], 30);
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_webhook_pre_hook_updates_input_and_receives_claude_payload() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("before.txt"), "before\n").unwrap();
    fs::write(repo.join("after.txt"), "after-webhook\n").unwrap();
    let hook_response = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "updatedInput": {
                "repo_root": repo,
                "path": "after.txt",
                "run_id": "run-model-tool-webhook"
            },
            "additionalContext": "webhook context"
        }
    });
    let (hook_url, captured) = spawn_webhook_test_server(hook_response).await;
    let token_env = "CODER_WEBHOOK_TEST_TOKEN";
    let previous_token = std::env::var_os(token_env);
    std::env::set_var(token_env, "secret-token");
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: hook_url,
                if_condition: None,
                timeout: Some(5),
                headers: BTreeMap::from([
                    ("Authorization".to_owned(), format!("Bearer ${token_env}")),
                    (
                        "X-Not-Allowed".to_owned(),
                        "$CODER_WEBHOOK_DENIED".to_owned(),
                    ),
                ]),
                allowed_env_vars: vec![token_env.to_owned()],
                status_message: None,
                once: false,
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
            "tool_use_id": "toolu-webhook",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "before.txt",
                "run_id": "run-model-tool-webhook"
            }
        }),
    )
    .await;
    restore_env_var(token_env, previous_token);

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"].as_str().unwrap().contains("after-webhook"));
    assert!(!body["content"].as_str().unwrap().contains("before"));
    let captured = captured.lock().unwrap();
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer secret-token")
    );
    assert_eq!(captured.not_allowed.as_deref(), Some(""));
    let hook_input = captured.body.as_ref().unwrap();
    assert_eq!(hook_input["hook_event_name"], "PreToolUse");
    assert_eq!(hook_input["tool_name"], "repo_read_file");
    assert_eq!(hook_input["tool_use_id"], "toolu-webhook");
    assert_eq!(hook_input["tool_input"]["path"], "before.txt");
    drop(captured);
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["updated_input_applied"], true);
    assert_eq!(phases[0]["webhook_hook_count"], 1);
    assert_eq!(phases[0]["hook_results"][0]["type"], "webhook");
    assert_eq!(phases[0]["hook_results"][0]["hook_transport"], "webhook");
    assert_eq!(
        phases[0]["hook_results"][0]["request_protocol"],
        "claude.hook_input.v1"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "hook_json"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["ssrf_guard"]["mode"],
        "ip_literal"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["ssrf_guard"]["socket_bound"],
        true
    );
    assert_eq!(
        phases[0]["hook_results"][0]["updated_input"]["path"],
        "after.txt"
    );
    assert_eq!(phases[0]["additional_contexts"][0], "webhook context");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_webhook_hooks_respect_allowed_url_policy() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook-url-policy");
    let mut config = default_project_config();
    config.allowed_webhook_urls = Some(vec!["https://hooks.example.com/*".to_owned()]);
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# URL policy\n").unwrap();
    let (hook_url, captured) = spawn_webhook_test_server(json!({})).await;
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: hook_url.clone(),
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
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-webhook-url-policy",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-webhook-url-policy"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"].as_str().unwrap().contains("URL policy"));
    assert!(captured.lock().unwrap().body.is_none());
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["hook_results"][0]["type"], "webhook");
    assert_eq!(
        phases[0]["hook_results"][0]["outcome"],
        "non_blocking_error"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "webhook_policy_blocked"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["policy"],
        "allowed_webhook_urls"
    );
    assert!(phases[0]["hook_results"][0]["hook_output_validation_error"]
        .as_str()
        .unwrap()
        .contains("allowedWebhookUrls"));
    assert_eq!(phases[0]["executed_hook_count"], 1);
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[test]
fn model_tool_webhook_transport_policy_requires_https_for_external_urls() {
    assert!(crate::model_tool_webhook_hooks::enforce_webhook_url_policy(
        "https://hooks.example.com/coder",
        None,
    )
    .is_ok());
    assert!(crate::model_tool_webhook_hooks::enforce_webhook_url_policy(
        "http://localhost:8765/hooks",
        None,
    )
    .is_ok());
    assert!(crate::model_tool_webhook_hooks::enforce_webhook_url_policy(
        "http://127.0.0.1:8765/hooks",
        None,
    )
    .is_ok());
    assert!(crate::model_tool_webhook_hooks::enforce_webhook_url_policy(
        "http://[::1]:8765/hooks",
        None,
    )
    .is_ok());

    let error = crate::model_tool_webhook_hooks::enforce_webhook_url_policy(
        "http://hooks.example.com/coder",
        Some(&["*".to_owned()]),
    )
    .unwrap_err();
    assert!(error.contains("must use https://"));
    assert!(error.contains("loopback"));
}

#[tokio::test]
async fn model_tool_webhook_hooks_block_private_metadata_addresses() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook-ssrf");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# SSRF\n").unwrap();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: "https://169.254.169.254/latest/meta-data".to_owned(),
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
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-webhook-ssrf",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-webhook-ssrf"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"].as_str().unwrap().contains("SSRF"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["hook_results"][0]["type"], "webhook");
    assert_eq!(phases[0]["hook_results"][0]["policy"], "ssrf_guard");
    assert!(phases[0]["hook_results"][0]["hook_output_validation_error"]
        .as_str()
        .unwrap()
        .contains("private/link-local"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[test]
fn model_tool_webhook_ssrf_resolver_validates_connection_addresses() {
    let blocked_metadata = std::net::SocketAddr::from(([169, 254, 169, 254], 0));
    let blocked_cgnat = std::net::SocketAddr::from(([100, 100, 100, 200], 0));
    let blocked_mapped_v6 =
        std::net::SocketAddr::new(std::net::IpAddr::V6("::ffff:a9fe:a9fe".parse().unwrap()), 0);
    let allowed_loopback = std::net::SocketAddr::from(([127, 0, 0, 1], 0));

    for addr in [blocked_metadata, blocked_cgnat, blocked_mapped_v6] {
        let error = crate::model_tool_webhook_hooks::validate_webhook_resolved_socket_addrs(
            "metadata.example",
            vec![addr],
        )
        .unwrap_err();
        assert!(error.contains("private/link-local"));
    }

    assert!(
        crate::model_tool_webhook_hooks::validate_webhook_resolved_socket_addrs(
            "localhost",
            vec![allowed_loopback],
        )
        .is_ok()
    );
}

#[test]
fn model_tool_webhook_proxy_policy_matches_claude_env_proxy_rules() {
    assert!(
        crate::model_tool_webhook_hooks::webhook_should_bypass_proxy(
            "https://api.example.com/v1",
            Some("*")
        )
    );
    assert!(
        crate::model_tool_webhook_hooks::webhook_should_bypass_proxy(
            "https://api.example.com/v1",
            Some(".example.com")
        )
    );
    assert!(
        crate::model_tool_webhook_hooks::webhook_should_bypass_proxy(
            "https://example.com/v1",
            Some(".example.com")
        )
    );
    assert!(
        !crate::model_tool_webhook_hooks::webhook_should_bypass_proxy(
            "https://notexample.com/v1",
            Some(".example.com")
        )
    );
    assert!(
        crate::model_tool_webhook_hooks::webhook_should_bypass_proxy(
            "https://api.example.com:8443/v1",
            Some("api.example.com:8443")
        )
    );
    assert!(
        !crate::model_tool_webhook_hooks::webhook_should_bypass_proxy(
            "https://api.example.com:8443/v1",
            Some("api.example.com:443")
        )
    );

    let env_map = BTreeMap::from([
        ("HTTP_PROXY".to_owned(), "http://upper-http:8080".to_owned()),
        (
            "HTTPS_PROXY".to_owned(),
            "http://upper-https:8080".to_owned(),
        ),
        (
            "https_proxy".to_owned(),
            "http://lower-https:8080".to_owned(),
        ),
        ("NO_PROXY".to_owned(), "blocked.example".to_owned()),
        ("no_proxy".to_owned(), ".internal.example".to_owned()),
    ]);
    let proxied = crate::model_tool_webhook_hooks::webhook_proxy_policy_for_env(
        "https://external.example/hook",
        &env_map,
    );
    assert!(proxied.uses_proxy());
    let proxied_report = proxied.report();
    assert_eq!(proxied_report["mode"], "env_proxy");
    assert_eq!(proxied_report["proxy_source"], "https_proxy");
    assert_eq!(proxied_report["no_proxy_source"], "no_proxy");

    let bypassed = crate::model_tool_webhook_hooks::webhook_proxy_policy_for_env(
        "https://api.internal.example/hook",
        &env_map,
    );
    assert!(!bypassed.uses_proxy());
    let bypassed_report = bypassed.report();
    assert_eq!(bypassed_report["mode"], "env_proxy_bypassed");
    assert_eq!(bypassed_report["proxy_configured"], true);
    assert_eq!(bypassed_report["proxy_bypassed"], true);
}

#[tokio::test]
async fn model_tool_webhook_header_env_vars_intersect_project_policy() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook-env-policy");
    let mut config = default_project_config();
    config.webhook_allowed_env_vars = Some(Vec::new());
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Env policy\n").unwrap();
    let (hook_url, captured) = spawn_webhook_test_server(json!({})).await;
    let token_env = "CODER_WEBHOOK_GLOBAL_DENY_TOKEN";
    let previous_token = std::env::var_os(token_env);
    std::env::set_var(token_env, "secret-token");
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: hook_url,
                if_condition: None,
                timeout: Some(5),
                headers: BTreeMap::from([("X-Not-Allowed".to_owned(), format!("${token_env}"))]),
                allowed_env_vars: vec![token_env.to_owned()],
                status_message: None,
                once: false,
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
            "tool_use_id": "toolu-webhook-env-policy",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-webhook-env-policy"
            }
        }),
    )
    .await;
    restore_env_var(token_env, previous_token);

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    let captured = captured.lock().unwrap();
    assert_eq!(captured.not_allowed.as_deref(), Some(""));
    drop(captured);
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(
        phases[0]["hook_results"][0]["effective_allowed_env_vars"],
        json!([])
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_webhook_hooks_respect_network_permission() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook-deny");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Deny;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Network denied\n").unwrap();
    let (hook_url, captured) = spawn_webhook_test_server(json!({})).await;
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
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
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-webhook-deny",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-webhook-deny"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(captured.lock().unwrap().body.is_none());
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "permission_blocked");
    assert_eq!(phases[0]["execution_status"], "skipped_permission_required");
    assert_eq!(phases[0]["webhook_hook_count"], 1);
    assert_eq!(phases[0]["executed_hook_count"], 0);
    assert_eq!(
        phases[0]["hook_results"][0]["outcome"],
        "skipped_permission_required"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["required_permission"],
        "network"
    );
    assert_eq!(phases[0]["hook_results"][0]["permission_behavior"], "deny");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_webhook_non_json_success_is_non_blocking() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook-non-json");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Non JSON hook\n").unwrap();
    let (hook_url, captured) = spawn_raw_webhook_test_server(
        StatusCode::OK,
        "text/plain",
        "plain hook output",
        Duration::ZERO,
    )
    .await;
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
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
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-webhook-non-json",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-webhook-non-json"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"].as_str().unwrap().contains("Non JSON hook"));
    assert!(captured.lock().unwrap().body.is_some());
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["webhook_hook_count"], 1);
    assert_eq!(phases[0]["executed_hook_count"], 1);
    assert_eq!(
        phases[0]["hook_results"][0]["outcome"],
        "non_blocking_error"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "invalid_webhook_json"
    );
    assert_eq!(phases[0]["hook_results"][0]["status_code"], 200);
    assert_eq!(
        phases[0]["hook_results"][0]["output_preview"],
        "plain hook output"
    );
    assert!(phases[0]["hook_results"][0]["hook_output_validation_error"]
        .as_str()
        .unwrap()
        .contains("must return JSON"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_webhook_timeout_is_non_blocking() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook-timeout");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Webhook timeout\n").unwrap();
    let (hook_url, captured) = spawn_raw_webhook_test_server(
        StatusCode::OK,
        "application/json",
        "{}",
        Duration::from_secs(2),
    )
    .await;
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: hook_url,
                if_condition: None,
                timeout: Some(1),
                headers: BTreeMap::new(),
                allowed_env_vars: Vec::new(),
                status_message: None,
                once: false,
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
            "tool_use_id": "toolu-webhook-timeout",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-webhook-timeout"
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
        .contains("Webhook timeout"));
    assert!(captured.lock().unwrap().body.is_some());
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["webhook_hook_count"], 1);
    assert_eq!(phases[0]["executed_hook_count"], 1);
    assert_eq!(phases[0]["hook_results"][0]["outcome"], "execution_error");
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "request_timeout"
    );
    assert_eq!(phases[0]["hook_results"][0]["aborted"], true);
    assert_eq!(phases[0]["hook_results"][0]["timeout_seconds"], 1);
    assert_eq!(
        phases[0]["hook_results"][0]["default_timeout_seconds"],
        CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS
    );
    let error_text = phases[0]["hook_results"][0]["error"]
        .as_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(error_text.contains("timeout") || error_text.contains("timed out"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_post_hook_json_updated_output_rewrites_model_result() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-hook-updated-output");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "original tool output\n").unwrap();
    let hook_output = json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedMCPToolOutput": {
                "replacement": "post hook output"
            },
            "additionalContext": "post context"
        }
    });
    let hook_output_path = repo.join("post-hook-output.json");
    fs::write(
        &hook_output_path,
        serde_json::to_string(&hook_output).unwrap(),
    )
    .unwrap();
    let hook_command = hook_emit_file_command(&hook_output_path);
    config.hooks = coder_config::HookSettings {
        post_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
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
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-hook-updated-output",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-hook-updated-output"
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
        .contains("post hook output"));
    assert!(!body["content"]
        .as_str()
        .unwrap()
        .contains("original tool output"));
    assert_eq!(body["payload"]["hook_updated_output"], true);
    assert_eq!(body["payload"]["output"]["replacement"], "post hook output");
    assert!(body["payload"]["original_payload"]
        .to_string()
        .contains("original tool output"));
    assert_eq!(body["refs"][0]["label"], "repo_evidence");
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[3]["phase"], "post_tool_use_hooks");
    assert_eq!(phases[3]["status"], "completed");
    assert_eq!(phases[3]["updated_tool_output_applied"], true);
    assert_eq!(phases[3]["additional_contexts"][0], "post context");
    assert_eq!(
        phases[3]["hook_results"][0]["updated_tool_output"]["replacement"],
        "post hook output"
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

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
            "harness_id": "review-only",
            "tool_uses": [{
                "id": "toolu-host-context",
                "name": "patch_preview",
                "input": {
                    "repo_root": repo,
                    "patch_file": "change.patch",
                    "harness_id": "native-code-edit"
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
        "review-only"
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
                    "harness": "review-only"
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
        "review-only"
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
            "harness_id": "review-only",
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
    let harness = config.harnesses.get_mut("review-only").unwrap();
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
                    "harness": "review-only",
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
        "review-only"
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
        "review-only"
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
    let harness = config.harnesses.get_mut("review-only").unwrap();
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
                    "harness": "review-only",
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
    let harness = config.harnesses.get_mut("review-only").unwrap();
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
                    "harness": "review-only",
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
    let harness = config.harnesses.get_mut("review-only").unwrap();
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
                    "harness": "review-only",
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
            "harness_id": "review-only",
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
            "harness_id": "review-only",
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
async fn model_tool_agent_alias_background_status_uses_task_output_alias() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-background");
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("review-only").unwrap();
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
                    "harness": "review-only",
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
                "tool_name": "TaskOutput",
                "run_id": "run-model-tool-agent-background",
                "input": {
                    "task_id": task_id
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
                "run_id": "run-model-tool-command-bg"
            }
        }),
    )
    .await;
    assert_eq!(output_response.status(), StatusCode::OK);
    let output_body = response_json(output_response).await;
    assert_eq!(output_body["payload"]["retrieval_status"], "not_ready");
    assert_eq!(output_body["payload"]["block"], false);
    assert_eq!(output_body["payload"]["timeout_ms"], 30000);
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

    let task_output_response = post_json(
        app.clone(),
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-command-task-output-read",
            "tool_name": "TaskOutput",
            "input": {
                "task_id": task_id,
                "block": false,
                "run_id": "run-model-tool-command-bg"
            }
        }),
    )
    .await;
    assert_eq!(task_output_response.status(), StatusCode::OK);
    let task_output_body = response_json(task_output_response).await;
    assert_eq!(task_output_body["payload"]["retrieval_status"], "not_ready");
    assert_eq!(task_output_body["payload"]["block"], false);
    assert_eq!(task_output_body["payload"]["status"], "running");
    assert_eq!(
        task_output_body["payload"]["task_output_policy"]["default_block_for_task_output_alias"],
        true
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
async fn model_tool_task_output_alias_blocks_for_background_command_completion() {
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
            "tool_name": "TaskOutput",
            "input": {
                "task_id": task_id,
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
async fn model_tool_task_stop_alias_stops_background_command() {
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
            "tool_name": "TaskStop",
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
    assert_eq!(stop_body["status"], "completed");
    assert_eq!(stop_body["is_error"], false);
    assert_eq!(stop_body["payload"]["task_id"], task_id);
    assert_eq!(stop_body["payload"]["task_type"], "local_bash");
    assert_eq!(stop_body["payload"]["task_status"], "cancelled");
    assert_eq!(stop_body["payload"]["cancelled"], true);
    assert_eq!(
        stop_body["payload"]["task_stop_policy"]["running_status_required"],
        false
    );
    assert_eq!(
        stop_body["payload"]["task_stop_policy"]["terminal_status_behavior"],
        "completed_noop"
    );
    let permission_phase = stop_body["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["phase"].as_str() == Some("permission_decision"))
        .unwrap();
    assert_eq!(permission_phase["required_permission"], "run_commands");
    assert_eq!(
        permission_phase["task_stop_resolution"]["task_type"],
        "local_bash"
    );
    assert_eq!(
        permission_phase["task_stop_resolution"]["required_permission"],
        "run_commands"
    );

    let status_body = wait_background_status(app, task_id, &["cancelled"]).await;
    assert_eq!(status_body["status"], "cancelled");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_task_stop_alias_resolves_subagent_permission() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-subagent-task-stop");
    let mut config = default_project_config();
    let harness = config.harnesses.get_mut("review-only").unwrap();
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
                    "harness": "review-only",
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
                "tool_name": "TaskOutput",
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
            "tool_name": "KillShell",
            "run_id": "run-model-tool-subagent-task-stop",
            "input": {
                "shell_id": task_id,
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
    assert_eq!(stop_body["payload"]["task_type"], "local_agent");
    assert_eq!(stop_body["payload"]["task_status"], "completed");
    assert_eq!(stop_body["payload"]["cancelled"], false);
    assert_eq!(
        stop_body["payload"]["task_stop_policy"]["terminal_status_behavior"],
        "completed_noop"
    );
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
    assert_eq!(
        permission_phase["task_stop_resolution"]["task_type"],
        "local_agent"
    );
    assert_eq!(
        permission_phase["task_stop_resolution"]["required_permission"],
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
    let handoff_goal = events[0].payload["plan_context"]["plan_draft"]["goal"]
        .as_str()
        .unwrap();
    assert_eq!(handoff_goal, handoff_task);
    assert!(!handoff_goal.contains("Do not execute until Start Work"));
    assert!(report
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
        json!({"message": "What is the current task status?"}),
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
        json!({"message": "For the current task, also add keyboard controls."}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["provider_trace"], Value::Null);
    assert_eq!(body["session"]["work_in_progress"], true);
    let second_response = post_json(
        app,
        "/api/v3/planner-chat/sessions/pcs_guidance/turn",
        json!({"message": "For the current task, also add touch controls."}),
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
                        "source_node_id": "verifier",
                        "signal": "completed"
                    }
                },
                "workflow_loop": {
                    "round": round,
                    "max_rounds": max_rounds
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
        json!({"message": "Stop the current task."}),
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
async fn workflow_run_uses_provider_backed_native_file_write_executor() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let native_plan = json!({
        "status": "completed",
        "summary": "Wrote the requested README from a provider-generated file plan.",
        "files": [
            {
                "path": "README.md",
                "content": "# Native Model Executor\n\nCreated by the provider-backed native file writer.\n"
            }
        ],
        "checks": ["provider_file_plan: emitted"],
        "blockers": []
    });
    let (provider_base_url, captured) = spawn_openai_compatible_capture_test_server(json!({
        "choices": [
            {
                "message": {
                    "content": native_plan.to_string()
                }
            }
        ],
        "usage": {
            "prompt_tokens": 1200,
            "completion_tokens": 300,
            "total_tokens": 1500,
            "prompt_cache_hit_tokens": 800
        }
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
            .is_some_and(|text| text.contains("provider_file_plan: emitted"))));
    assert_eq!(
        fs::read_to_string(repo_root.join("README.md")).unwrap(),
        "# Native Model Executor\n\nCreated by the provider-backed native file writer.\n"
    );

    let run_id = RunId::from_string(body["run_id"].as_str().unwrap());
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "backend.native_rust.started"
            && event.payload["implementation"] == "native-model-file-write"
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
            && event.payload["implementation"] == "native-model-file-write"
            && event.payload["path"] == "README.md"
    }));

    let captured_body = captured
        .lock()
        .unwrap()
        .clone()
        .expect("provider request body should be captured");
    assert_eq!(captured_body["model"], "test-model");
    assert_eq!(captured_body["max_tokens"], 8000);
    assert!(captured_body.get("thinking").is_none());
    assert!(captured_body["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("strict JSON"));
    assert!(captured_body["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("\"start_work_authorized\":true"));
    assert!(!captured_body.to_string().contains("provider-test-token"));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn workflow_run_uses_provider_backed_planner_for_open_ended_quality_goal() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let native_plan = json!({
        "status": "completed",
        "summary": "Created the requested README.",
        "files": [{
            "path": "README.md",
            "content": "# Product\n\nA concise project overview.\n"
        }],
        "checks": ["README.md created"],
        "blockers": []
    });
    let planner_decision = json!({
        "decision": "finish",
        "summary": "The verified README satisfies the requested quality target.",
        "improvements": [],
        "expected_gain": "none",
        "blockers": []
    });
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({"choices": [{"message": {"content": native_plan.to_string()}}]}),
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
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0]["max_tokens"], 8000);
    assert_eq!(captured[1]["max_tokens"], 900);
    assert!(captured[1]["messages"][0]["content"]
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
    let native_plan = json!({
        "status": "completed",
        "summary": "Created the requested README.",
        "files": [{
            "path": "README.md",
            "content": "# Product\n\nA concise project overview.\n"
        }],
        "checks": ["README.md created"],
        "blockers": []
    });
    let (provider_base_url, captured) =
        spawn_openai_compatible_status_sequence_capture_test_server(vec![
            OpenAiCompatibleStatusResponse {
                status: StatusCode::OK,
                content_type: "application/json",
                body: json!({
                    "choices": [{"message": {"content": native_plan.to_string()}}]
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
    assert_eq!(captured.lock().unwrap().len(), 2);
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
                        "content": final_content
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
            && event.payload["tool_call_count"] == 3
    }));

    let captured_requests = captured.lock().unwrap().clone();
    assert_eq!(captured_requests.len(), 4);
    assert!(captured_requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["function"]["name"] == "write_text_file"));
    assert!(captured_requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["function"]["name"] == "edit_text_file"));
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
                "message": {"role": "assistant", "content": final_content}
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
            && message["content"].as_str().is_some_and(
                |content| content.contains("Use edit_text_file for a small exact change")
            )));

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
                        "content": final_content
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
            && event.payload["tool_call_count"] == 3
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
                        "content": final_content
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
                        "content": child_final_content
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
                        "content": final_content
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
async fn workflow_run_uses_provider_native_background_subagent_task_output_alias() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let (provider_base_url, captured) =
        spawn_native_background_subagent_task_output_test_server().await;
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
        .any(|check| check.as_str() == Some("background_subagent_task_output: observed")));
    assert_eq!(
        fs::read_to_string(repo_root.join("BG-SUBAGENT.md")).unwrap(),
        "# Background Subagent\n\nTaskOutput observed the background subagent through shared tool execution.\n"
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
            panic!("TaskOutput alias should resolve to shared subagent status: {events:?}")
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
async fn workflow_run_uses_provider_native_background_subagent_task_stop_alias() {
    let store_root = temp_root();
    let repo_root = temp_root();
    fs::create_dir_all(&repo_root).unwrap();
    let (provider_base_url, captured) =
        spawn_native_background_subagent_task_stop_test_server(platform_sleep_args()).await;
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
        .any(|check| check.as_str() == Some("background_subagent_task_stop: cancelled")));
    assert_eq!(
        fs::read_to_string(repo_root.join("BG-SUBAGENT-CANCEL.md")).unwrap(),
        "# Background Subagent Cancelled\n\nTaskStop cancelled the background subagent through shared tool execution.\n"
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
                && event.payload["tool_name"] == "task_stop"
                && event.payload["status"] == "completed"
        })
        .expect("TaskStop alias should stop the background subagent");
    let stop_summary = stop_event.payload["summary"].as_str().unwrap();
    let stop_payload = serde_json::from_str::<Value>(stop_summary).unwrap();
    assert_eq!(stop_payload["task_type"], "local_agent");
    assert_eq!(
        stop_payload["required_permission"],
        "child_harness_permissions"
    );
    assert_eq!(stop_payload["task_status"], "cancelled");

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
                        "content": final_content
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
                        "content": final_content
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
                        "content": final_content
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
                        "content": final_content
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
                        "content": final_content
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
                        "content": final_content
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
                        "content": final_content
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
                        "content": final_content
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
            && event.payload["implementation"] == "native-model-file-write"
            && event.payload["reason"] == "missing_start_work_approval"
    }));

    let _ = fs::remove_dir_all(store_root);
    let _ = fs::remove_dir_all(repo_root);
}

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
    let browser_runtime = store_root
        .join("tmp")
        .join("runtime-cache")
        .join("browser-verifier")
        .join("node_modules")
        .join("playwright");
    fs::create_dir_all(&browser_runtime).unwrap();
    fs::write(browser_runtime.join("package.json"), b"{}").unwrap();
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
    assert!(!body["browser_verifier"]["browsers_path"]
        .as_str()
        .unwrap()
        .is_empty());
    assert!(body["browser_verifier"]["resolved_node_modules"]
        .as_str()
        .unwrap()
        .contains("browser-verifier"));
    assert!(
        body["browser_verifier"]["candidate_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(
        body["browser_verifier"]["runtime_cache"]["entries"]
            .as_u64()
            .unwrap(),
        1
    );
    assert_eq!(
        body["browser_verifier"]["runtime_cache"]["truncated"],
        false
    );
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
            return Json(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": json!({
                            "status": "completed",
                            "summary": "Created the requested concurrency fixture.",
                            "files": [{
                                "path": "PARALLEL.md",
                                "content": "# Parallel Planner Test\n"
                            }],
                            "checks": ["parallel_planner_test: completed"],
                            "blockers": []
                        }).to_string()
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
                                "content": final_content
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

async fn spawn_native_background_subagent_task_output_test_server(
) -> (String, Arc<Mutex<Vec<Value>>>) {
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
                "checks": ["background_subagent_task_output: observed"],
                "blockers": []
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": final_content
                        }
                    }
                ]
            }));
        }

        if provider_request_has_tool_result(&body, "call-bg-agent-output") {
            let write_args = json!({
                "path": "BG-SUBAGENT.md",
                "content": "# Background Subagent\n\nTaskOutput observed the background subagent through shared tool execution.\n"
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
                                        "name": "TaskOutput",
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
                            "content": child_final_content
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
                            "content": child_final_content
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
                            "content": final_content
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
                                        "name": "TaskOutput",
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

async fn spawn_native_background_subagent_task_stop_test_server(
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
                "checks": ["background_subagent_task_stop: cancelled"],
                "blockers": []
            })
            .to_string();
            return Json(json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": final_content
                        }
                    }
                ]
            }));
        }

        if provider_request_has_tool_result(&body, "call-cancel-bg-stop") {
            let write_args = json!({
                "path": "BG-SUBAGENT-CANCEL.md",
                "content": "# Background Subagent Cancelled\n\nTaskStop cancelled the background subagent through shared tool execution.\n"
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
                                        "name": "TaskStop",
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
                            "content": child_final_content
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
