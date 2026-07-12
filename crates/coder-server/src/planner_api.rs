use std::{path::PathBuf, time::SystemTime};

use axum::{
    extract::{Path, State},
    Json,
};
use coder_config::ProjectConfig;
use coder_core::{FinalReport, ReportStatus, RunId};
use coder_workflow::{WorkflowRunControl, WorkflowRunOptions};
use serde_json::{json, Value};

use crate::api_types::{
    planner_chat_assistant_turn, planner_chat_user_turn, PlanDraft, PlannerChatSession,
    PlannerChatSessionCreateRequest, PlannerChatSessionResponse, PlannerChatTurnRequest,
    PlannerChatTurnResponse, PlannerConversationEngine, PlannerConversationRequest,
    PlannerReadiness, PlannerStartWorkRequest, PlannerStartWorkResponse, ProviderSettings,
};
use crate::planner_conversation::{
    concise_plan_summary, message_is_pure_plan_confirmation, normalize_planner_mode,
    planner_provider_setup_required_response,
};
use crate::planner_history::{
    planner_history_compaction_attempt, record_planner_history_compaction_outcome,
};
use crate::planner_provider_dispatch::{
    model_provider_config_error, ModelPlannerConversationEngine,
};
use crate::planner_runtime::resolve_planner_runtime;
use crate::planner_session::{
    append_planner_session_record, planner_turn_events, prune_planner_sessions,
    start_work_clarification, store_planner_session_snapshot, trim_planner_session_turns,
    StoredPlannerChatSession,
};
use crate::provider_settings::apply_provider_settings_to_project_config;
use crate::run_control::request_run_cancel;
use crate::{default_project_config, public_preview, workflow_runner_for_api, ApiError, ApiState};

fn normalized_repo_root(repo: Option<String>) -> Option<String> {
    repo.map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn start_work_provider_config_error(
    config: &ProjectConfig,
    workflow_id: &str,
    settings: &ProviderSettings,
) -> Option<String> {
    if settings.mock_mode {
        return None;
    }
    let workflow = config.workflows.get(workflow_id)?;
    for node in &workflow.nodes {
        let harness = config.harnesses.get(&node.harness)?;
        if harness.backend != "planner-model" {
            continue;
        }
        let agent = config.agents.get(&node.agent)?;
        let model = config.models.get(&agent.model)?;
        if let Some(message) = model_provider_config_error(settings, model) {
            return Some(message);
        }
    }
    None
}

fn planner_run_context(plan: &PlanDraft) -> Value {
    json!({
        "plan_draft": {
            "execution_mode": &plan.execution_mode,
            "review_mode": &plan.review_mode,
            "scope": sanitized_plan_items(&plan.scope),
            "non_goals": sanitized_plan_items(&plan.non_goals),
            "assumptions": sanitized_plan_items(&plan.assumptions),
            "steps": sanitized_plan_items(&plan.steps),
            "affected_paths": &plan.affected_paths,
            "acceptance_criteria": sanitized_plan_items(&plan.acceptance_criteria),
            "risks": sanitized_plan_items(&plan.risks)
        },
        "start_work_authorized": true
    })
}

fn task_from_plan(plan: &PlanDraft) -> String {
    let goal = sanitize_start_work_gate(&plan.goal);
    if goal.trim().is_empty() {
        "Execute the approved plan.".to_owned()
    } else {
        goal
    }
}

fn sanitized_plan_items(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|item| sanitize_start_work_gate(item))
        .filter(|item| !item.trim().is_empty())
        .collect()
}

fn sanitize_start_work_gate(text: &str) -> String {
    let mut sanitized = text.to_owned();
    for phrase in [
        "Do not execute until Start Work.",
        "Do not execute until Start Work",
        "Do not execute until `Start Work`.",
        "Do not execute until `Start Work`",
        "Do not execute until the user clicks Start Work.",
        "Do not execute until the user clicks Start Work",
        "Do not execute until the user clicks `Start Work`.",
        "Do not execute until the user clicks `Start Work`",
        "Do not execute until Start Work is clicked.",
        "Do not execute until Start Work is clicked",
        "I will not execute this until Start Work.",
        "I will not execute this until Start Work",
        "Execute only after Start Work through the native executor.",
        "Execute only after Start Work through the native executor",
    ] {
        sanitized = sanitized.replace(phrase, "");
    }
    sanitized
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) async fn create_planner_chat_session(
    State(state): State<ApiState>,
    Json(request): Json<PlannerChatSessionCreateRequest>,
) -> Result<Json<PlannerChatSessionResponse>, ApiError> {
    let session_id = format!("pcs_{}", RunId::new());
    let workflow_id = request
        .workflow_id
        .unwrap_or_else(|| "planner-led".to_owned());
    let mut config = request.config.unwrap_or_else(default_project_config);
    let provider_settings = state.provider_settings.lock().unwrap().clone();
    apply_provider_settings_to_project_config(&mut config, &provider_settings);
    let runtime =
        resolve_planner_runtime(&config, &workflow_id, request.planner_agent_id.as_deref())?;
    let session = PlannerChatSession {
        session_id: session_id.clone(),
        workflow_id: workflow_id.clone(),
        repo_root: normalized_repo_root(request.repo),
        mode: normalize_planner_mode(request.mode.as_deref()),
        runtime: Some(runtime),
        ready: false,
        readiness: PlannerReadiness::NeedsClarification,
        plan_draft: None,
        open_questions: vec!["What exact outcome should this plan target?".to_owned()],
        acceptance_criteria: Vec::new(),
        risks: Vec::new(),
        work_in_progress: false,
        active_run_id: None,
        latest_run_id: None,
        turns: Vec::new(),
    };
    {
        let now = SystemTime::now();
        let mut sessions = state.planner_sessions.lock().unwrap();
        store_planner_session_snapshot(&mut sessions, session.clone(), now);
    }
    append_planner_session_record(&state.store, &session, "session.created", json!({}))?;
    Ok(Json(PlannerChatSessionResponse { session }))
}

pub(crate) async fn get_planner_chat_session(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<PlannerChatSessionResponse>, ApiError> {
    let now = SystemTime::now();
    let session = {
        let mut sessions = state.planner_sessions.lock().unwrap();
        let session = {
            let stored = sessions.get_mut(&session_id).ok_or_else(|| {
                ApiError::not_found(format!("session '{session_id}' was not found"))
            })?;
            stored.last_accessed = now;
            trim_planner_session_turns(&mut stored.session);
            stored.session.clone()
        };
        prune_planner_sessions(&mut sessions, now, Some(&session_id));
        session
    };
    Ok(Json(PlannerChatSessionResponse { session }))
}

fn handle_active_run_planner_turn(
    state: &ApiState,
    session_id: &str,
    request: &PlannerChatTurnRequest,
) -> Result<Option<PlannerChatTurnResponse>, ApiError> {
    use crate::api_types::PlannerTurnOperation;

    if request.operation == PlannerTurnOperation::Chat {
        return Ok(None);
    }
    let session_snapshot = state
        .planner_sessions
        .lock()
        .unwrap()
        .get(session_id)
        .map(|stored| stored.session.clone())
        .ok_or_else(|| ApiError::not_found(format!("session '{session_id}' was not found")))?;
    let active_run_id = session_snapshot
        .active_run_id
        .as_deref()
        .filter(|_| session_snapshot.work_in_progress);
    let run_id = active_run_id.or(session_snapshot.latest_run_id.as_deref());

    let mut guidance_queued = false;
    let assistant_message = match request.operation {
        PlannerTurnOperation::Chat => unreachable!("chat operations return before run control"),
        PlannerTurnOperation::Status => {
            planner_run_status_message(state, run_id, &request.message)?
        }
        PlannerTurnOperation::Interrupt => {
            let Some(active_run_id) = active_run_id else {
                return Ok(Some(store_local_planner_turn(
                    state,
                    session_id,
                    request.message.clone(),
                    localized_message(
                        &request.message,
                        "There is no active workflow to cancel.",
                        "当前没有正在运行的任务可取消。",
                    ),
                    "active_run.cancel.not_applicable",
                    None,
                )?));
            };
            request_run_cancel(state, &RunId::from_string(active_run_id))?;
            localized_message(
                &request.message,
                "Cancellation was requested. The active model or tool step is being stopped, and Coder will keep the evidence already recorded.",
                "已请求取消。Coder 正在停止当前模型或工具步骤，并会保留已经记录的证据。",
            )
        }
        PlannerTurnOperation::UserInput => {
            let Some(active_run_id) = active_run_id else {
                return Ok(Some(store_local_planner_turn(
                    state,
                    session_id,
                    request.message.clone(),
                    localized_message(
                        &request.message,
                        "There is no active workflow to receive this input.",
                        "当前没有正在运行的工作流可以接收这条补充要求。",
                    ),
                    "active_run.guidance.not_applicable",
                    None,
                )?));
            };
            let guidance = public_preview(&request.message, 2_000);
            match crate::model_tool_async_attachments::queue_planner_user_guidance(
                state,
                &RunId::from_string(active_run_id),
                &guidance,
            )
            .map_err(ApiError::internal)?
            {
                Some(_) => {
                    guidance_queued = true;
                    localized_message(
                        &request.message,
                        "I attached this requirement to the active workflow. The executor will apply it at the next safe model turn without restarting the task.",
                        "这条要求已追加到当前工作流；执行器会在下一个安全模型轮次应用，不会从头重启任务。",
                    )
                }
                None => localized_message(
                    &request.message,
                    "The workflow finished before this requirement could be attached. I kept it in the conversation for the next plan.",
                    "工作流已在补充要求送达前结束；这条要求已保留在对话中，可用于下一次计划。",
                ),
            }
        }
    };

    let event_kind = match request.operation {
        PlannerTurnOperation::Chat => unreachable!("chat operations return before run control"),
        PlannerTurnOperation::Status => "active_run.status.reported",
        PlannerTurnOperation::Interrupt => "active_run.cancel.requested",
        PlannerTurnOperation::UserInput if guidance_queued => "active_run.guidance.queued",
        PlannerTurnOperation::UserInput => "active_run.guidance.not_queued",
    };
    Ok(Some(store_local_planner_turn(
        state,
        session_id,
        request.message.clone(),
        assistant_message,
        event_kind,
        None,
    )?))
}

fn planner_run_status_message(
    state: &ApiState,
    run_id: Option<&str>,
    user_message: &str,
) -> Result<String, ApiError> {
    let Some(run_id) = run_id else {
        return Ok(localized_message(
            user_message,
            "There is no active or recent workflow in this Planner session.",
            "当前 Planner 会话没有正在运行或最近完成的工作流。",
        ));
    };
    let run_id = RunId::from_string(run_id);
    let metadata = state.store.read_metadata(&run_id)?;
    let events = state.store.read_events(&run_id)?;
    let status = metadata
        .map(|metadata| format!("{:?}", metadata.status).to_lowercase())
        .unwrap_or_else(|| "starting".to_owned());
    let phase = events
        .last()
        .map(|event| public_run_progress_label(&event.kind))
        .unwrap_or("initializing");
    if user_message.is_ascii() {
        Ok(format!(
            "Run {} is {}. Coder has recorded {} progress events; the latest phase is {}.",
            run_id.as_str(),
            status,
            events.len(),
            phase
        ))
    } else {
        Ok(format!(
            "当前任务 {} 的状态是 {}，已记录 {} 条进度事件；最近阶段：{}。",
            run_id.as_str(),
            status,
            events.len(),
            phase
        ))
    }
}

fn public_run_progress_label(kind: &str) -> &'static str {
    match kind {
        "run.started" | "workflow.started" | "round.started" => "initializing the workflow",
        "node.started" => "starting an execution step",
        "model.tool_turn.started" | "model.tool_call.started" => "running a model tool step",
        "model.tool_call.completed" => "processing completed tool evidence",
        "file.written" => "recording file changes",
        "command.started" | "command.completed" => "running verification",
        "report.created" => "preparing the final report",
        "run.completed" => "completed",
        "run.blocked" => "blocked",
        "run.failed" => "failed",
        "run.cancel_requested" => "stopping the active step",
        "run.cancelled" => "cancelled",
        _ => "working through the current step",
    }
}

fn localized_message(user_message: &str, english: &str, chinese: &str) -> String {
    if user_message.is_ascii() {
        english.to_owned()
    } else {
        chinese.to_owned()
    }
}

fn store_local_planner_turn(
    state: &ApiState,
    session_id: &str,
    user_message: String,
    assistant_message: String,
    event_kind: &'static str,
    mode: Option<&str>,
) -> Result<PlannerChatTurnResponse, ApiError> {
    let now = SystemTime::now();
    let session = {
        let mut sessions = state.planner_sessions.lock().unwrap();
        let stored = sessions
            .get_mut(session_id)
            .ok_or_else(|| ApiError::not_found(format!("session '{session_id}' was not found")))?;
        if let Some(mode) = mode {
            stored.session.mode = normalize_planner_mode(Some(mode));
        }
        stored
            .session
            .turns
            .push(planner_chat_user_turn(user_message));
        stored.session.turns.push(planner_chat_assistant_turn(
            assistant_message.clone(),
            Vec::new(),
            false,
        ));
        trim_planner_session_turns(&mut stored.session);
        stored.last_accessed = now;
        stored.revision = stored.revision.saturating_add(1);
        stored.session.clone()
    };
    let plan_draft = session.plan_draft.clone();
    let concise_summary = concise_plan_summary(plan_draft.as_ref(), &assistant_message);
    let response = PlannerChatTurnResponse {
        session: session.clone(),
        assistant_message: assistant_message.clone(),
        plan_draft,
        readiness: session.readiness,
        open_questions: session.open_questions.clone(),
        acceptance_criteria: session.acceptance_criteria.clone(),
        risks: session.risks.clone(),
        suggested_mode: session.mode.clone(),
        should_start_workflow: false,
        ready: session.ready,
        ready_for_start_work: session.ready,
        missing_information: session.open_questions.clone(),
        concise_plan_summary: concise_summary,
        execution_allowed: false,
        run_preview: None,
        response_truncated: false,
        artifacts: Vec::new(),
        structured_artifacts: Vec::new(),
        large_artifacts: false,
        provider_trace: None,
        events: vec![json!({
            "type": event_kind,
            "run_id": session.active_run_id.as_deref().or(session.latest_run_id.as_deref())
        })],
    };
    append_planner_session_record(
        &state.store,
        &session,
        "session.turn.local_control",
        json!({"intent": event_kind}),
    )?;
    Ok(response)
}

pub(crate) async fn planner_chat_turn(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Json(request): Json<PlannerChatTurnRequest>,
) -> Result<Json<PlannerChatTurnResponse>, ApiError> {
    if let Some(response) = handle_active_run_planner_turn(&state, &session_id, &request)? {
        return Ok(Json(response));
    }
    if message_is_pure_plan_confirmation(&request.message) {
        let can_confirm_locally = state
            .planner_sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .is_some_and(|stored| {
                stored.session.ready
                    && stored.session.plan_draft.is_some()
                    && stored.session.open_questions.is_empty()
                    && !stored.session.work_in_progress
            });
        if can_confirm_locally {
            let response = store_local_planner_turn(
                &state,
                &session_id,
                request.message,
                "I'm ready. Click Start Work and I'll execute this through the native executor."
                    .to_owned(),
                "planner.confirmation.local",
                request.mode.as_deref().or(Some("work")),
            )?;
            return Ok(Json(response));
        }
    }
    let requested_mode = request.mode.clone();
    let confirmed = request.confirmed.unwrap_or(false);
    let provider_settings = state.provider_settings.lock().unwrap().clone();
    let conversation_request = {
        let now = SystemTime::now();
        let mut sessions = state.planner_sessions.lock().unwrap();
        let conversation_request = {
            let stored = sessions.get_mut(&session_id).ok_or_else(|| {
                ApiError::not_found(format!("session '{session_id}' was not found"))
            })?;
            stored.last_accessed = now;
            trim_planner_session_turns(&mut stored.session);
            let session = &mut stored.session;
            if let Some(repo_root) = normalized_repo_root(request.repo.clone()) {
                session.repo_root = Some(repo_root);
            }
            let mode = requested_mode
                .as_deref()
                .map(|mode| normalize_planner_mode(Some(mode)))
                .unwrap_or_else(|| normalize_planner_mode(Some(&session.mode)));
            session.mode = mode.clone();
            if request.config.is_some()
                || request.planner_agent_id.is_some()
                || session.runtime.is_none()
            {
                let mut config = request
                    .config
                    .clone()
                    .unwrap_or_else(default_project_config);
                apply_provider_settings_to_project_config(&mut config, &provider_settings);
                session.runtime = Some(resolve_planner_runtime(
                    &config,
                    &session.workflow_id,
                    request.planner_agent_id.as_deref(),
                )?);
            }
            let runtime = session
                .runtime
                .clone()
                .ok_or_else(|| ApiError::bad_request("planner runtime is not configured"))?;
            PlannerConversationRequest {
                session_id: session.session_id.clone(),
                workflow_id: session.workflow_id.clone(),
                repo_root: session.repo_root.clone(),
                runtime,
                mode: mode.clone(),
                message: request.message.clone(),
                confirmed,
                history: session.turns.clone(),
                current_plan: session.plan_draft.clone(),
                provider_settings,
            }
        };
        prune_planner_sessions(&mut sessions, now, Some(&session_id));
        conversation_request
    };

    let history_compaction_attempt = planner_history_compaction_attempt(&conversation_request);
    let engine = ModelPlannerConversationEngine::new(state.clone());
    let planner_response = engine
        .respond(conversation_request)
        .await
        .map_err(ApiError::internal)?;

    let now = SystemTime::now();
    let mut sessions = state.planner_sessions.lock().unwrap();
    let (events, session_snapshot, ready_for_start_work, missing_information, concise_plan_summary) = {
        let stored = sessions
            .get_mut(&session_id)
            .ok_or_else(|| ApiError::not_found(format!("session '{session_id}' was not found")))?;
        stored.last_accessed = now;
        let session = &mut stored.session;
        let mode = requested_mode
            .as_deref()
            .map(|mode| normalize_planner_mode(Some(mode)))
            .unwrap_or_else(|| normalize_planner_mode(Some(&session.mode)));
        session.mode = mode;
        session.turns.push(planner_chat_user_turn(request.message));
        session.turns.push(planner_chat_assistant_turn(
            planner_response.assistant_message.clone(),
            planner_response.artifacts.clone(),
            planner_response.response_truncated,
        ));
        session.plan_draft = planner_response.plan_draft.clone();
        session.readiness = planner_response.readiness;
        session.ready = planner_response.readiness == PlannerReadiness::Ready;
        session.open_questions = planner_response.open_questions.clone();
        session.acceptance_criteria = planner_response.acceptance_criteria.clone();
        session.risks = planner_response.risks.clone();
        trim_planner_session_turns(session);
        stored.revision = stored.revision.saturating_add(1);
        let events = planner_turn_events(session, &planner_response);
        let session_snapshot = session.clone();
        let ready_for_start_work = session.ready;
        let missing_information = planner_response.open_questions.clone();
        let concise_plan_summary = concise_plan_summary(
            planner_response.plan_draft.as_ref(),
            &planner_response.assistant_message,
        );
        (
            events,
            session_snapshot,
            ready_for_start_work,
            missing_information,
            concise_plan_summary,
        )
    };
    prune_planner_sessions(&mut sessions, now, Some(&session_id));
    let response = PlannerChatTurnResponse {
        session: session_snapshot.clone(),
        assistant_message: planner_response.assistant_message,
        plan_draft: planner_response.plan_draft,
        readiness: planner_response.readiness,
        open_questions: planner_response.open_questions,
        acceptance_criteria: planner_response.acceptance_criteria,
        risks: planner_response.risks,
        suggested_mode: planner_response.suggested_mode,
        should_start_workflow: false,
        ready: session_snapshot.ready,
        ready_for_start_work,
        missing_information,
        concise_plan_summary,
        execution_allowed: false,
        run_preview: None,
        response_truncated: planner_response.response_truncated,
        artifacts: planner_response.artifacts.clone(),
        structured_artifacts: planner_response.artifacts,
        large_artifacts: planner_response.large_artifacts,
        provider_trace: planner_response.provider_trace.clone(),
        events,
    };
    drop(sessions);
    let history_compaction_outcome = history_compaction_attempt
        .map(|attempt| record_planner_history_compaction_outcome(&state.store, attempt))
        .transpose()?;
    let mut turn_record_extra = json!({
        "should_start_workflow": false,
        "execution_allowed": false
    });
    if let Some(provider_trace) = &response.provider_trace {
        turn_record_extra["provider_trace"] =
            serde_json::to_value(provider_trace).unwrap_or(Value::Null);
    }
    if let Some(outcome) = history_compaction_outcome {
        turn_record_extra["history_compaction"] = outcome;
    }
    append_planner_session_record(
        &state.store,
        &session_snapshot,
        "session.turn.completed",
        turn_record_extra,
    )?;
    Ok(Json(response))
}

pub(crate) async fn start_planner_chat_work(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Json(request): Json<PlannerStartWorkRequest>,
) -> Result<Json<PlannerStartWorkResponse>, ApiError> {
    let now = SystemTime::now();
    let (mut session, session_revision) = {
        let mut sessions = state.planner_sessions.lock().unwrap();
        let snapshot = {
            let stored = sessions.get_mut(&session_id).ok_or_else(|| {
                ApiError::not_found(format!("session '{session_id}' was not found"))
            })?;
            if stored.session.work_in_progress {
                return Err(ApiError::conflict(
                    "this Planner session already has work in progress",
                ));
            }
            stored.last_accessed = now;
            trim_planner_session_turns(&mut stored.session);
            (stored.session.clone(), stored.revision)
        };
        prune_planner_sessions(&mut sessions, now, Some(&session_id));
        snapshot
    };
    let workflow_id = request
        .workflow_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| session.workflow_id.clone());
    let mut config = request
        .config
        .clone()
        .unwrap_or_else(default_project_config);
    let provider_settings = state.provider_settings.lock().unwrap().clone();
    apply_provider_settings_to_project_config(&mut config, &provider_settings);
    let runtime =
        resolve_planner_runtime(&config, &workflow_id, request.planner_agent_id.as_deref())?;
    session.workflow_id = workflow_id.clone();
    session.mode = "discuss".to_owned();
    session.runtime = Some(runtime);

    if session.plan_draft.is_none()
        || session.readiness != PlannerReadiness::Ready
        || !session.open_questions.is_empty()
    {
        let assistant_message = start_work_clarification(&session);
        session.turns.push(planner_chat_assistant_turn(
            assistant_message.clone(),
            Vec::new(),
            false,
        ));
        session.ready = false;
        session.readiness = PlannerReadiness::NeedsClarification;
        session = {
            let mut sessions = state.planner_sessions.lock().unwrap();
            store_planner_session_snapshot(&mut sessions, session.clone(), SystemTime::now())
        };
        let response = PlannerStartWorkResponse {
            session: session.clone(),
            assistant_message: Some(assistant_message),
            run_id: None,
            status: "needs_clarification".to_owned(),
            events_url: None,
            timeline_url: None,
        };
        append_planner_session_record(
            &state.store,
            &session,
            "session.work.needs_clarification",
            json!({"status": response.status.clone()}),
        )?;
        return Ok(Json(response));
    }

    if let Some(message) =
        start_work_provider_config_error(&config, &workflow_id, &provider_settings)
    {
        let planner_response = planner_provider_setup_required_response(message);
        session.turns.push(planner_chat_assistant_turn(
            planner_response.assistant_message.clone(),
            planner_response.artifacts.clone(),
            planner_response.response_truncated,
        ));
        session.ready = false;
        session.readiness = planner_response.readiness;
        session.open_questions = planner_response.open_questions;
        session.acceptance_criteria = planner_response.acceptance_criteria;
        session.risks = planner_response.risks;
        session = {
            let mut sessions = state.planner_sessions.lock().unwrap();
            store_planner_session_snapshot(&mut sessions, session.clone(), SystemTime::now())
        };
        let response = PlannerStartWorkResponse {
            session: session.clone(),
            assistant_message: Some(planner_response.assistant_message),
            run_id: None,
            status: "blocked".to_owned(),
            events_url: None,
            timeline_url: None,
        };
        append_planner_session_record(
            &state.store,
            &session,
            "session.work.blocked",
            json!({"status": response.status.clone()}),
        )?;
        return Ok(Json(response));
    }

    let active_run_id = RunId::new();
    let active_run_id_text = active_run_id.to_string();
    let (control_sender, control_receiver) =
        tokio::sync::watch::channel(WorkflowRunControl::Running);
    let work_revision = {
        let now = SystemTime::now();
        let mut sessions = state.planner_sessions.lock().unwrap();
        let stored = sessions
            .get_mut(&session_id)
            .ok_or_else(|| ApiError::not_found(format!("session '{session_id}' was not found")))?;
        if stored.revision != session_revision {
            return Err(ApiError::conflict(
                "the Planner session changed while Start Work was preparing; review the latest plan and start again",
            ));
        }
        if stored.session.work_in_progress {
            return Err(ApiError::conflict(
                "this Planner session already has work in progress",
            ));
        }
        stored.session.workflow_id = session.workflow_id.clone();
        stored.session.mode = session.mode.clone();
        stored.session.runtime = session.runtime.clone();
        stored.session.work_in_progress = true;
        stored.session.active_run_id = Some(active_run_id_text.clone());
        stored.last_accessed = now;
        stored.revision = stored.revision.saturating_add(1);
        session = stored.session.clone();
        stored.revision
    };
    if let Err(error) = append_planner_session_record(
        &state.store,
        &session,
        "session.work.started",
        json!({"status": "running"}),
    ) {
        let _ =
            clear_planner_work_in_progress(&state, &session_id, work_revision, SystemTime::now());
        return Err(error.into());
    }
    state
        .active_run_controls
        .lock()
        .unwrap()
        .insert(active_run_id_text.clone(), control_sender);

    let plan = session
        .plan_draft
        .clone()
        .ok_or_else(|| ApiError::bad_request("planner session has no plan draft"))?;
    let repo_root = normalized_repo_root(request.repo.clone())
        .or_else(|| session.repo_root.clone())
        .unwrap_or_else(|| ".".to_owned());
    let plan_context = planner_run_context(&plan);
    let task = task_from_plan(&plan);
    let token_budget = coder_config::resolve_workflow_cost_policy(&config, &workflow_id)
        .map(|policy| policy.token_budget);
    crate::run_token_budget::initialize_run_token_budget(&state, &active_run_id, token_budget);
    let mut options = WorkflowRunOptions::new(&workflow_id, &task);
    options.repo_root = PathBuf::from(&repo_root);
    options.plan_context = Some(plan_context);
    options.run_id = Some(active_run_id);
    options.control = Some(control_receiver);
    let runner = workflow_runner_for_api(config, state.store.clone(), state.clone());
    let state_for_run = state.clone();
    let session_id_for_run = session_id.clone();
    let workflow_id_for_run = workflow_id.clone();
    let active_run_id_for_run = active_run_id_text.clone();
    tokio::spawn(async move {
        let run_result = runner.run(options).await;
        let unapplied_guidance =
            crate::model_tool_async_attachments::finalize_planner_user_guidance(
                &state_for_run,
                &RunId::from_string(&active_run_id_for_run),
            );
        crate::run_token_budget::clear_run_token_budget_if_inactive(
            &state_for_run,
            &RunId::from_string(&active_run_id_for_run),
        );
        let finalization = match run_result {
            Ok(output) => complete_planner_work(
                &state_for_run,
                &session_id_for_run,
                work_revision,
                &workflow_id_for_run,
                output,
                &unapplied_guidance,
            ),
            Err(error) => fail_planner_work(
                &state_for_run,
                &session_id_for_run,
                work_revision,
                &active_run_id_for_run,
                &workflow_id_for_run,
                &error.to_string(),
                &unapplied_guidance,
            ),
        };
        if let Err(error) = finalization {
            eprintln!("failed to finalize Planner work: {error:?}");
        }
    });

    let response = PlannerStartWorkResponse {
        session: session.clone(),
        assistant_message: None,
        run_id: Some(active_run_id_text.clone()),
        status: "running".to_owned(),
        events_url: Some(format!("/api/v3/runs/{active_run_id_text}/events")),
        timeline_url: Some(format!("/api/v3/runs/{active_run_id_text}/timeline")),
    };
    Ok(Json(response))
}

fn complete_planner_work(
    state: &ApiState,
    session_id: &str,
    work_revision: u64,
    workflow_id: &str,
    output: coder_workflow::WorkflowRunOutput,
    unapplied_guidance: &[String],
) -> Result<(), ApiError> {
    let run_id = output.run_id.to_string();
    let status = format!("{:?}", output.report.status).to_lowercase();
    let assistant_message =
        start_work_result_message(workflow_id, &output.report, unapplied_guidance.len());
    let session = {
        let mut sessions = state.planner_sessions.lock().unwrap();
        let stored = sessions
            .get_mut(session_id)
            .ok_or_else(|| ApiError::not_found(format!("session '{session_id}' was not found")))?;
        merge_planner_work_completion(
            stored,
            work_revision,
            &run_id,
            assistant_message,
            SystemTime::now(),
        )
    };
    append_planner_session_record(
        &state.store,
        &session,
        "session.work.completed",
        json!({
            "run_id": run_id,
            "status": status,
            "events_url": format!("/api/v3/runs/{run_id}/events"),
            "timeline_url": format!("/api/v3/runs/{run_id}/timeline")
        }),
    )?;
    Ok(())
}

fn fail_planner_work(
    state: &ApiState,
    session_id: &str,
    work_revision: u64,
    run_id: &str,
    workflow_id: &str,
    error: &str,
    unapplied_guidance: &[String],
) -> Result<(), ApiError> {
    let mut assistant_message = format!(
        "Work failed for workflow '{workflow_id}': {}",
        public_preview(error, 400)
    );
    append_unapplied_guidance_notice(&mut assistant_message, unapplied_guidance.len());
    let session = {
        let mut sessions = state.planner_sessions.lock().unwrap();
        let stored = sessions
            .get_mut(session_id)
            .ok_or_else(|| ApiError::not_found(format!("session '{session_id}' was not found")))?;
        merge_planner_work_completion(
            stored,
            work_revision,
            run_id,
            assistant_message,
            SystemTime::now(),
        )
    };
    append_planner_session_record(
        &state.store,
        &session,
        "session.work.failed",
        json!({"run_id": run_id, "status": "failed", "error": error}),
    )?;
    Ok(())
}

fn clear_planner_work_in_progress(
    state: &ApiState,
    session_id: &str,
    work_revision: u64,
    now: SystemTime,
) -> Result<PlannerChatSession, ApiError> {
    let mut sessions = state.planner_sessions.lock().unwrap();
    let stored = sessions
        .get_mut(session_id)
        .ok_or_else(|| ApiError::not_found(format!("session '{session_id}' was not found")))?;
    if stored.revision >= work_revision {
        stored.session.work_in_progress = false;
        stored.session.active_run_id = None;
        stored.last_accessed = now;
        stored.revision = stored.revision.saturating_add(1);
    }
    Ok(stored.session.clone())
}

fn merge_planner_work_completion(
    stored: &mut StoredPlannerChatSession,
    work_revision: u64,
    run_id: &str,
    assistant_message: String,
    now: SystemTime,
) -> PlannerChatSession {
    let plan_unchanged_during_work = stored.revision == work_revision;
    stored.session.work_in_progress = false;
    stored.session.active_run_id = None;
    stored.session.latest_run_id = Some(run_id.to_owned());
    if plan_unchanged_during_work {
        stored.session.ready = false;
        stored.session.readiness = PlannerReadiness::NeedsClarification;
    }
    stored.session.turns.push(planner_chat_assistant_turn(
        assistant_message,
        Vec::new(),
        false,
    ));
    trim_planner_session_turns(&mut stored.session);
    stored.last_accessed = now;
    stored.revision = stored.revision.saturating_add(1);
    stored.session.clone()
}

fn start_work_result_message(
    workflow_id: &str,
    report: &FinalReport,
    unapplied_guidance_count: usize,
) -> String {
    let mut message = match report.status {
        ReportStatus::Completed => format!("Work completed for workflow '{workflow_id}'."),
        ReportStatus::Blocked => {
            let reason = report
                .blockers
                .first()
                .map(String::as_str)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("the executor reported a blocked status");
            format!("Start Work is blocked: {reason}.")
        }
        ReportStatus::Failed => format!("Work failed for workflow '{workflow_id}'."),
        ReportStatus::Cancelled => format!("Work was cancelled for workflow '{workflow_id}'."),
    };
    append_unapplied_guidance_notice(&mut message, unapplied_guidance_count);
    message
}

fn append_unapplied_guidance_notice(message: &mut String, count: usize) {
    if count > 0 {
        message.push_str(&format!(
            " {count} requirement(s) arrived too late for another executor turn and were not applied; they remain in this Planner conversation for the next plan."
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::{planner_chat_assistant_turn, planner_chat_user_turn, PlannerReadiness};

    fn plan_with_start_work_gate() -> PlanDraft {
        PlanDraft {
            goal: "Create README.md with a short note. Do not execute until Start Work.".to_owned(),
            execution_mode: crate::api_types::PlanExecutionMode::MustWrite,
            review_mode: crate::api_types::PlanReviewMode::Standard,
            scope: vec!["README.md".to_owned()],
            non_goals: Vec::new(),
            assumptions: Vec::new(),
            steps: vec![
                "Write README.md after Start Work is clicked.".to_owned(),
                "Do not execute until Start Work.".to_owned(),
            ],
            affected_paths: vec!["README.md".to_owned()],
            acceptance_criteria: vec!["README.md exists".to_owned()],
            risks: Vec::new(),
            open_questions: Vec::new(),
            selected_workflow_id: "planner-led".to_owned(),
            memory_proposals: Vec::new(),
        }
    }

    #[test]
    fn start_work_task_handoff_strips_planner_only_gate() {
        let task = task_from_plan(&plan_with_start_work_gate());

        assert_eq!(task, "Create README.md with a short note.");
        assert!(!task.contains("Start Work"));
    }

    #[test]
    fn start_work_plan_context_is_compact_and_execution_authorized() {
        let context = planner_run_context(&plan_with_start_work_gate());

        assert_eq!(context["start_work_authorized"], true);
        assert!(context.get("original_user_request").is_none());
        assert!(context["plan_draft"].get("goal").is_none());
        assert_eq!(
            context["plan_draft"]["affected_paths"],
            json!(["README.md"])
        );
        assert_eq!(
            context["plan_draft"]["acceptance_criteria"],
            json!(["README.md exists"])
        );
        assert_eq!(
            context["plan_draft"]["steps"],
            json!(["Write README.md after Start Work is clicked."])
        );
    }

    #[test]
    fn work_completion_merges_without_overwriting_parallel_planner_turns() {
        let mut sessions = std::collections::BTreeMap::new();
        let mut session = PlannerChatSession {
            session_id: "pcs_parallel".to_owned(),
            workflow_id: "planner-led".to_owned(),
            repo_root: Some("F:/repo".to_owned()),
            mode: "discuss".to_owned(),
            runtime: None,
            ready: true,
            readiness: PlannerReadiness::Ready,
            plan_draft: Some(plan_with_start_work_gate()),
            open_questions: Vec::new(),
            acceptance_criteria: Vec::new(),
            risks: Vec::new(),
            work_in_progress: true,
            active_run_id: Some("run-active".to_owned()),
            latest_run_id: None,
            turns: vec![planner_chat_user_turn("Original task".to_owned())],
        };
        session.turns.push(planner_chat_assistant_turn(
            "Original plan is ready".to_owned(),
            Vec::new(),
            false,
        ));
        store_planner_session_snapshot(&mut sessions, session, SystemTime::now());
        let stored = sessions.get_mut("pcs_parallel").unwrap();
        stored.revision = stored.revision.saturating_add(1);
        let work_revision = stored.revision;

        stored.session.turns.push(planner_chat_user_turn(
            "Plan the next task while work runs".to_owned(),
        ));
        stored.session.turns.push(planner_chat_assistant_turn(
            "The next task is ready".to_owned(),
            Vec::new(),
            false,
        ));
        stored.session.plan_draft.as_mut().unwrap().goal = "Next task".to_owned();
        stored.session.ready = true;
        stored.session.readiness = PlannerReadiness::Ready;
        stored.revision = stored.revision.saturating_add(1);

        let merged = merge_planner_work_completion(
            stored,
            work_revision,
            "run-parallel",
            "Work completed for the original task".to_owned(),
            SystemTime::now(),
        );

        assert!(!merged.work_in_progress);
        assert_eq!(merged.latest_run_id.as_deref(), Some("run-parallel"));
        assert!(merged.ready);
        assert_eq!(merged.readiness, PlannerReadiness::Ready);
        assert_eq!(merged.plan_draft.unwrap().goal, "Next task");
        assert!(merged
            .turns
            .iter()
            .any(|turn| turn.content == "Plan the next task while work runs"));
        assert_eq!(
            merged.turns.last().unwrap().content,
            "Work completed for the original task"
        );
    }
}
