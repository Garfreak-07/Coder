use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Write,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use coder_config::{
    validate_project_config, AgentSpec as ConfigAgentSpec, HarnessSpec as ConfigHarnessSpec,
    MemoryScope as ConfigMemoryScope, ModelSpec as ConfigModelSpec,
    PermissionDecision as ConfigPermissionDecision, ProjectConfig, ValidationIssue,
    ValidationLevel, ValidationReport, WorkflowNodeSpec as ConfigWorkflowNodeSpec,
};
use coder_core::{FinalReport, RunId, RunState, RunStatus};
use coder_extensions::{
    builtin_plugin_manifests, builtin_remote_skill_entries, discover_skills_payload,
    extension_search, installed_skills_payload, remote_skill_summary, validate_plugin_manifest,
    validate_skill_manifest, DiscoverSkillsPayload, ExtensionManifestSummary,
    InstalledSkillsPayload, PluginManifest, PluginManifestValidation, RemoteSkillEntry,
    SkillManifestValidation, SkillSummary, SkillUpdateInfo,
};
use coder_harness::{
    find_mock_mcp_tool, invoke_mock_mcp_tool, mock_mcp_servers, mock_mcp_tools,
    validate_mcp_manifest, McpManifestValidation, McpServerSummary, McpToolCallRequest,
    McpToolCallResult, McpToolSummary, ToolRegistry, ToolRegistryEntry,
};
use coder_memory::{
    append_project_memory_record, ensure_memory_write_allowed, import_text_knowledge_source,
    load_project_memory_file, memory_read_event, memory_write_confirmed_event,
    memory_write_proposed_event, retrieve_knowledge_hints, AgentMemoryRole, KnowledgeChunk,
    KnowledgeRetrievalHit, KnowledgeRetrievalRequest, KnowledgeSource, KnowledgeStore,
    KnowledgeTextImportRequest, MemoryAllowedContext, MemoryError, MemoryPurpose, MemoryRecord,
    MemoryScope, MemorySensitivity, ProjectMemoryFile, RetrievalBackendKind,
};
use coder_store::{
    RepoEvidenceKind, RepoEvidenceRef, RunCheckpointRef, RunStore, StoreError, StoredRunSummary,
};
use coder_tools::{
    apply_patch_file, find_files, git_diff, git_status, preview_command, preview_patch_file,
    read_file, read_file_range, run_command, search_text, CommandPreview, CommandRunEvidence,
    CommandRunRequest, GitDiffEvidence, GitStatusEvidence, PatchApplyEvidence,
    PatchApplyRequest as ToolPatchApplyRequest, PatchPreviewEvidence, RepoFileEvidence,
    RepoFileRef, RepoReadSnippet, RepoSearchMatch, RepoToolConfig, RepoToolError,
};
use coder_workflow::{MockWorkflowRunner, WorkflowError, WorkflowRunOptions, WorkflowRunner};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const MCP_OUTPUT_INLINE_LIMIT: usize = 1024;

#[derive(Debug, Clone)]
pub struct ApiState {
    pub store: RunStore,
    library_workflows: Arc<Mutex<BTreeMap<String, Value>>>,
    planner_sessions: Arc<Mutex<BTreeMap<String, PlannerChatSession>>>,
    installed_skills: Arc<Mutex<BTreeMap<String, InstalledSkillRecord>>>,
    plugin_marketplaces: Arc<Mutex<BTreeMap<String, PluginMarketplace>>>,
    skill_extra_roots: Arc<Mutex<Vec<SkillExtraRoot>>>,
    provider_settings: Arc<Mutex<ProviderSettings>>,
}

impl ApiState {
    pub fn new(store: RunStore) -> Self {
        Self {
            store,
            library_workflows: Arc::new(Mutex::new(BTreeMap::new())),
            planner_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            installed_skills: Arc::new(Mutex::new(BTreeMap::new())),
            plugin_marketplaces: Arc::new(Mutex::new(BTreeMap::from([(
                "builtin".to_owned(),
                PluginMarketplace {
                    name: "builtin".to_owned(),
                    url: "builtin://plugins".to_owned(),
                    enabled: true,
                },
            )]))),
            skill_extra_roots: Arc::new(Mutex::new(Vec::new())),
            provider_settings: Arc::new(Mutex::new(ProviderSettings::default())),
        }
    }
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/v3/health", get(health))
        .route("/api/v3/capabilities", get(capabilities))
        .route("/api/v3/agent-role-cards", get(agent_role_cards))
        .route("/api/v3/memory/project/load", post(load_project_memory))
        .route(
            "/api/v3/memory/project/propose-write",
            post(propose_project_memory_write),
        )
        .route(
            "/api/v3/memory/project/confirm-write",
            post(confirm_project_memory_write),
        )
        .route(
            "/api/v3/knowledge-sources/import-text",
            post(import_knowledge_text),
        )
        .route("/api/v3/knowledge-sources", get(list_knowledge_sources))
        .route(
            "/api/v3/knowledge-sources/{source_id}/chunks",
            get(list_knowledge_source_chunks),
        )
        .route("/api/v3/knowledge/retrieve", post(retrieve_knowledge))
        .route("/api/v3/config/validate", post(validate_config))
        .route("/api/v3/workflows/default", get(default_workflow))
        .route("/api/v3/workflows/validate", post(validate_workflow))
        .route("/api/v3/workflows/preview", post(preview_run))
        .route("/api/v3/workflows/run", post(run_workflow))
        .route("/api/v3/library", get(get_library))
        .route("/api/v3/library/workflows", post(save_library_workflow))
        .route(
            "/api/v3/library/workflows/{workflow_id}",
            get(get_library_workflow),
        )
        .route(
            "/api/v3/planner-chat/sessions",
            post(create_planner_chat_session),
        )
        .route(
            "/api/v3/planner-chat/sessions/{session_id}",
            get(get_planner_chat_session),
        )
        .route(
            "/api/v3/planner-chat/sessions/{session_id}/turn",
            post(planner_chat_turn),
        )
        .route(
            "/api/v3/planner-chat/sessions/{session_id}/start-work",
            post(start_planner_chat_work),
        )
        .route("/api/v3/mcp/servers", get(list_mcp_servers))
        .route("/api/v3/mcp/servers/validate", post(validate_mcp))
        .route("/api/v3/mcp/tools", get(list_mcp_tools))
        .route("/api/v3/mcp/tools/invoke", post(invoke_mcp_tool))
        .route("/api/v3/mcp/manifests/validate", post(validate_mcp))
        .route("/api/v3/extensions/plugins", get(list_extension_plugins))
        .route(
            "/api/v3/extensions/plugins/validate",
            post(validate_extension_plugin),
        )
        .route("/api/v3/extensions/skills", get(list_extension_skills))
        .route(
            "/api/v3/extensions/installed",
            get(list_extensions_installed),
        )
        .route("/api/v3/extensions/search", get(search_extensions_endpoint))
        .route(
            "/api/v3/extensions/skills/validate",
            post(validate_extension_skill),
        )
        .route("/api/v3/skills/installed", get(list_installed_skills))
        .route("/api/v3/skills/discover", get(discover_skills_endpoint))
        .route("/api/v3/skills/updates", get(list_skill_updates))
        .route("/api/v3/skills/install", post(install_skill))
        .route("/api/v3/skills/auto-update", post(auto_update_skills))
        .route(
            "/api/v3/skills/developer-import",
            post(developer_import_skill),
        )
        .route("/api/v3/skills/{skill_id}/update", post(update_skill))
        .route("/api/v3/skills/{skill_id}/enable", post(enable_skill))
        .route("/api/v3/skills/{skill_id}/disable", post(disable_skill))
        .route(
            "/api/v3/skills/{skill_id}",
            axum::routing::delete(remove_skill),
        )
        .route("/api/v3/skills/{skill_id}/pin", post(pin_skill))
        .route("/api/v3/skills/{skill_id}/unpin", post(unpin_skill))
        .route("/api/v3/skills/{skill_id}/rollback", post(rollback_skill))
        .route(
            "/api/v3/skills/{skill_id}/update-policy",
            post(set_skill_update_policy),
        )
        .route(
            "/api/v3/plugins/marketplaces",
            get(list_plugin_marketplaces).post(add_plugin_marketplace),
        )
        .route(
            "/api/v3/plugins/marketplaces/{name}",
            axum::routing::delete(remove_plugin_marketplace),
        )
        .route(
            "/api/v3/plugins/marketplaces/{name}/upgrade",
            post(upgrade_plugin_marketplace),
        )
        .route("/api/v3/plugins", get(list_plugins))
        .route("/api/v3/plugins/installed", get(list_installed_plugins))
        .route("/api/v3/plugins/{plugin_id}", get(read_plugin))
        .route(
            "/api/v3/plugins/{plugin_id}/skills/{skill_name}",
            get(read_plugin_skill),
        )
        .route(
            "/api/v3/skills/extra-roots",
            get(list_skill_extra_roots).post(add_skill_extra_root),
        )
        .route("/api/v3/hooks", get(list_hooks))
        .route("/api/v3/cache/status", get(cache_status))
        .route("/api/v3/cache/clear", post(cache_clear))
        .route("/api/v3/cache/reindex", post(cache_reindex))
        .route("/api/v3/cache/tasks", get(cache_tasks))
        .route(
            "/api/v3/cache/tasks/{task_id}",
            axum::routing::delete(cancel_cache_task),
        )
        .route("/api/v3/harness/tools", get(list_harness_tools))
        .route(
            "/api/v3/providers/settings",
            get(get_provider_settings).post(save_provider_settings),
        )
        .route("/api/v3/providers/status", get(get_provider_status))
        .route("/api/v3/providers/test", post(test_provider_status))
        .route("/api/v3/runs", get(list_runs).post(run_workflow))
        .route("/api/v3/runs/preview", post(preview_run))
        .route(
            "/api/v3/tools/command/preview",
            post(preview_command_endpoint),
        )
        .route("/api/v3/tools/command/run", post(run_command_endpoint))
        .route(
            "/api/v3/tools/repo/find-files",
            post(repo_find_files_endpoint),
        )
        .route(
            "/api/v3/tools/repo/search-text",
            post(repo_search_text_endpoint),
        )
        .route(
            "/api/v3/tools/repo/read-file",
            post(repo_read_file_endpoint),
        )
        .route(
            "/api/v3/tools/repo/read-file-range",
            post(repo_read_file_range_endpoint),
        )
        .route("/api/v3/tools/git/status", post(git_status_endpoint))
        .route("/api/v3/tools/git/diff", post(git_diff_endpoint))
        .route("/api/v3/tools/patch/preview", post(preview_patch_endpoint))
        .route("/api/v3/tools/patch/apply", post(apply_patch_endpoint))
        .route("/api/v3/runs/mock", post(run_mock_workflow))
        .route("/api/v3/runs/{run_id}", get(get_run_detail))
        .route("/api/v3/runs/{run_id}/events", get(list_run_events))
        .route("/api/v3/runs/{run_id}/timeline", get(list_run_timeline))
        .route("/api/v3/runs/{run_id}/changes", get(list_run_changes))
        .route(
            "/api/v3/runs/{run_id}/changes/{change_set_id}/diff",
            get(get_change_diff),
        )
        .route(
            "/api/v3/runs/{run_id}/changes/{change_set_id}/accept",
            post(accept_change_set),
        )
        .route(
            "/api/v3/runs/{run_id}/changes/{change_set_id}/undo",
            post(undo_change_set),
        )
        .route("/api/v3/runs/{run_id}/pause", post(pause_run))
        .route("/api/v3/runs/{run_id}/resume", post(resume_run))
        .route("/api/v3/runs/{run_id}/cancel", post(cancel_run))
        .route("/api/v3/runs/{run_id}/heartbeat", get(run_heartbeat))
        .route(
            "/api/v3/runs/{run_id}/report/preview",
            get(preview_run_report),
        )
        .route("/api/v3/runs/{run_id}/report", post(write_run_report))
        .route(
            "/api/v3/runs/{run_id}/repo-evidence",
            get(list_run_repo_evidence),
        )
        .route(
            "/api/v3/runs/{run_id}/artifacts/{artifact_name}",
            get(get_run_artifact),
        )
        .route(
            "/api/v3/runs/{run_id}/checkpoints",
            get(list_run_checkpoints),
        )
        .route(
            "/api/v3/runs/{run_id}/checkpoints/{checkpoint_name}",
            get(get_run_checkpoint).post(write_run_checkpoint),
        )
        .route("/api/v3/blobs/sha256/{digest}", get(get_blob_sha256))
        .route("/api/v3/repo-evidence/{ref_id}", get(get_repo_evidence))
        .with_state(state)
}

pub async fn serve(addr: SocketAddr, state: ApiState) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(state)).await
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "coder-server",
        api_version: "v3",
    })
}

async fn capabilities() -> Json<CapabilitiesResponse> {
    Json(CapabilitiesResponse {
        api_version: "v3",
        workflow: vec![
            "validate",
            "preview",
            "run_mock",
            "library_in_memory",
            "graph_semantics",
        ],
        runs: vec![
            "list",
            "detail",
            "events",
            "pause",
            "resume",
            "cancel",
            "heartbeat",
            "report_preview",
            "report_write",
            "artifacts",
            "blobs",
            "repo_evidence",
        ],
        tools: vec![
            "repo_find_files",
            "repo_search_text",
            "repo_read_file",
            "repo_read_file_range",
            "git_status",
            "git_diff",
            "command_preview",
            "command_run",
            "patch_preview",
            "patch_apply",
        ],
        planner_chat: vec!["sessions", "turns", "discuss_no_execute", "work_preview"],
        settings: vec![
            "provider_settings",
            "provider_status",
            "provider_test_offline",
            "openai_compatible_profiles",
            "deepseek_compatible_profile",
            "secret_refs_only",
        ],
        extensions: vec![
            "plugins_list",
            "plugin_validate",
            "extensions_search",
            "installed_extensions_list",
            "skills_list",
            "skill_manifest_validate",
            "skill_install_baseline",
            "skill_update_baseline",
            "skill_enable_disable",
            "skill_pin_unpin",
            "skill_rollback_baseline",
            "mcp_validate",
            "mcp_servers",
            "mcp_tools",
            "mcp_mock_invoke",
            "harness_tools",
        ],
        memory: vec![
            "project_load",
            "project_write_proposal",
            "project_write_confirmation",
            "knowledge_import_text",
            "knowledge_sources_list",
            "knowledge_chunks_list",
            "knowledge_lexical_retrieve",
        ],
    })
}

async fn agent_role_cards() -> Json<AgentRoleCardsResponse> {
    Json(AgentRoleCardsResponse {
        role_cards: vec![
            AgentRoleCard {
                id: "planner",
                label: "Planner",
                archetype: "planner",
                role: "planner",
                engine_id: "planner-engine",
                default_capabilities: vec![
                    "negotiate_contract",
                    "make_plan",
                    "judge_completion",
                    "judge_risk",
                    "make_next_decision",
                    "round_summarize",
                ],
                description: "Plans work, decides readiness, and owns final reports.",
                default_output_contract: "planner_conversation",
            },
            AgentRoleCard {
                id: "executor",
                label: "Executor",
                archetype: "executor",
                role: "executor",
                engine_id: "code-worker-engine",
                default_capabilities: vec![
                    "follow_planner_order",
                    "modify_files",
                    "optional_check_command",
                    "return_execution_result",
                ],
                description: "Executes planner-approved work and returns evidence.",
                default_output_contract: "execution_result",
            },
        ],
    })
}

async fn default_workflow() -> Json<DefaultWorkflowResponse> {
    let config = default_project_config();
    let workflow_id = "planner-led".to_owned();
    let workflow = config.workflows.get(&workflow_id).cloned();
    Json(DefaultWorkflowResponse {
        workflow_id,
        config,
        workflow,
    })
}

fn default_project_config() -> ProjectConfig {
    serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap()
}

fn resolve_planner_runtime(
    config: &ProjectConfig,
    workflow_id: &str,
    planner_agent_id: Option<&str>,
) -> Result<PlannerRuntimeContext, ApiError> {
    let validation = validate_project_config(config);
    if validation
        .issues
        .iter()
        .any(|issue| issue.level == ValidationLevel::Error)
    {
        return Err(ApiError::bad_request(format!(
            "Planner workflow config is invalid: {}",
            validation_issue_summary(&validation)
        )));
    }
    let workflow = config
        .workflows
        .get(workflow_id)
        .ok_or_else(|| ApiError::bad_request(format!("workflow '{workflow_id}' was not found")))?;
    let node = resolve_planner_node(config, workflow, workflow_id, planner_agent_id)?;
    let agent = config.agents.get(&node.agent).ok_or_else(|| {
        ApiError::bad_request(format!(
            "workflow '{workflow_id}' planner node '{}' references missing agent '{}'",
            node.id, node.agent
        ))
    })?;
    if agent.role != "planner" {
        return Err(ApiError::bad_request(format!(
            "workflow '{workflow_id}' planner node '{}' must reference an agent with role 'planner'",
            node.id
        )));
    }
    let harness = config.harnesses.get(&node.harness).ok_or_else(|| {
        ApiError::bad_request(format!(
            "workflow '{workflow_id}' planner node '{}' references missing harness '{}'",
            node.id, node.harness
        ))
    })?;
    ensure_planner_conversation_harness(&node.harness, harness)?;
    let model = config.models.get(&agent.model).ok_or_else(|| {
        ApiError::bad_request(format!(
            "planner agent '{}' references missing model '{}'",
            node.agent, agent.model
        ))
    })?;
    Ok(PlannerRuntimeContext {
        workflow_id: workflow_id.to_owned(),
        workflow_name: workflow.name.clone(),
        node_id: node.id.clone(),
        agent_id: node.agent.clone(),
        harness_id: node.harness.clone(),
        agent: agent.clone(),
        harness: harness.clone(),
        model: model.clone(),
    })
}

fn resolve_planner_node<'a>(
    config: &ProjectConfig,
    workflow: &'a coder_config::WorkflowSpec,
    workflow_id: &str,
    planner_agent_id: Option<&str>,
) -> Result<&'a ConfigWorkflowNodeSpec, ApiError> {
    if let Some(planner_agent_id) = planner_agent_id.filter(|value| !value.trim().is_empty()) {
        return workflow
            .nodes
            .iter()
            .find(|node| node.agent == planner_agent_id || node.id == planner_agent_id)
            .ok_or_else(|| {
                ApiError::bad_request(format!(
                    "workflow '{workflow_id}' has no planner node for '{planner_agent_id}'"
                ))
            });
    }
    workflow
        .nodes
        .iter()
        .find(|node| {
            config
                .agents
                .get(&node.agent)
                .map(|agent| agent.role == "planner")
                .unwrap_or(false)
        })
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "workflow '{workflow_id}' has no planner node. Add a planner AgentSpec and bind it to a planner-conversation HarnessSpec."
            ))
        })
}

fn ensure_planner_conversation_harness(
    harness_id: &str,
    harness: &ConfigHarnessSpec,
) -> Result<(), ApiError> {
    if harness.backend != "planner-model" {
        return Err(ApiError::bad_request(format!(
            "Planner Chat requires planner harness '{harness_id}' to use backend 'planner-model'"
        )));
    }
    ensure_permission(
        harness_id,
        "read_files",
        harness.permissions.read_files,
        ConfigPermissionDecision::Allow,
    )?;
    for (permission, decision) in [
        ("write_files", harness.permissions.write_files),
        ("run_commands", harness.permissions.run_commands),
        ("network", harness.permissions.network),
        ("secrets", harness.permissions.secrets),
        ("publish_external", harness.permissions.publish_external),
        ("git_commit", harness.permissions.git_commit),
        ("git_push", harness.permissions.git_push),
        ("deploy", harness.permissions.deploy),
    ] {
        ensure_permission(
            harness_id,
            permission,
            decision,
            ConfigPermissionDecision::Deny,
        )?;
    }
    if harness
        .memory
        .write
        .iter()
        .any(|scope| *scope != ConfigMemoryScope::Run)
    {
        return Err(ApiError::bad_request(format!(
            "Planner Conversation Harness '{harness_id}' may only write run memory"
        )));
    }
    Ok(())
}

fn ensure_permission(
    harness_id: &str,
    permission: &str,
    actual: ConfigPermissionDecision,
    expected: ConfigPermissionDecision,
) -> Result<(), ApiError> {
    if actual == expected {
        return Ok(());
    }
    Err(ApiError::bad_request(format!(
        "Planner Conversation Harness '{harness_id}' must set {permission} to {:?}",
        expected
    )))
}

fn validation_issue_summary(report: &ValidationReport) -> String {
    report
        .issues
        .iter()
        .filter(|issue| issue.level == ValidationLevel::Error)
        .take(3)
        .map(|issue| format!("{} at {}", issue.code, issue.target))
        .collect::<Vec<_>>()
        .join("; ")
}

fn planner_turn_events(
    session: &PlannerChatSession,
    response: &PlannerConversationResponse,
) -> Vec<Value> {
    let mut events = vec![json!({
        "type": "planner.message.completed",
        "session_id": session.session_id,
        "workflow_id": session.workflow_id,
        "readiness": response.readiness
    })];
    if let Some(plan) = &response.plan_draft {
        events.push(json!({
            "type": "planner.plan.updated",
            "session_id": session.session_id,
            "selected_workflow_id": plan.selected_workflow_id,
            "open_questions": plan.open_questions,
            "acceptance_criteria": plan.acceptance_criteria,
            "risks": plan.risks
        }));
        for proposal in &plan.memory_proposals {
            events.push(json!({
                "type": "planner.memory.proposed",
                "session_id": session.session_id,
                "scope": proposal.scope,
                "key": proposal.key,
                "requires_confirmation": proposal.requires_confirmation
            }));
        }
    }
    events.push(json!({
        "type": "planner.readiness.changed",
        "session_id": session.session_id,
        "readiness": response.readiness
    }));
    events
}

fn start_work_clarification(session: &PlannerChatSession) -> String {
    if session.plan_draft.is_none() {
        return "I need to turn this into a concrete plan before starting work.".to_owned();
    }
    if !session.open_questions.is_empty() {
        return format!(
            "I need clarification before starting work:\n{}",
            numbered_lines(&session.open_questions)
        );
    }
    "I am not ready to start work yet. Please confirm the goal, scope, and acceptance criteria."
        .to_owned()
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
        if model_provider_config_error(settings, model).is_some() {
            return Some(planner_model_config_error());
        }
    }
    None
}

fn planner_run_context_from_session(session: &PlannerChatSession, plan: &PlanDraft) -> Value {
    let conversation_summary = session
        .turns
        .iter()
        .rev()
        .find(|turn| turn.role == "assistant")
        .map(|turn| turn.content.clone())
        .unwrap_or_else(|| "Planner session was ready to start work.".to_owned());
    let original_user_request = session
        .turns
        .iter()
        .find(|turn| turn.role == "user")
        .map(|turn| turn.content.clone())
        .unwrap_or_else(|| plan.goal.clone());
    json!({
        "original_user_request": original_user_request,
        "planner_conversation_summary": conversation_summary,
        "plan_draft": plan,
        "acceptance_criteria": plan.acceptance_criteria,
        "risks": plan.risks,
        "affected_paths": if plan.affected_paths.is_empty() { plan.scope.clone() } else { plan.affected_paths.clone() },
        "selected_workflow_id": plan.selected_workflow_id
    })
}

fn task_from_plan(plan: &PlanDraft) -> String {
    let mut lines = vec![plan.goal.clone()];
    if !plan.affected_paths.is_empty() {
        lines.push(format!(
            "Affected paths: {}",
            plan.affected_paths.join(", ")
        ));
    } else if !plan.scope.is_empty() {
        lines.push(format!("Scope: {}", plan.scope.join(", ")));
    }
    if !plan.acceptance_criteria.is_empty() {
        lines.push(format!(
            "Acceptance: {}",
            plan.acceptance_criteria.join("; ")
        ));
    }
    lines.join("\n")
}

async fn get_library(State(state): State<ApiState>) -> Json<LibraryResponse> {
    let workflows = state
        .library_workflows
        .lock()
        .unwrap()
        .iter()
        .map(|(id, workflow)| LibraryWorkflowSummary {
            id: id.clone(),
            workflow: workflow.clone(),
        })
        .collect();
    Json(LibraryResponse { workflows })
}

async fn save_library_workflow(
    State(state): State<ApiState>,
    Json(request): Json<LibraryWorkflowSaveRequest>,
) -> Result<Json<LibraryWorkflowSaveResponse>, ApiError> {
    if request.workflow_id.trim().is_empty() {
        return Err(ApiError::bad_request("workflow_id must not be empty"));
    }
    state
        .library_workflows
        .lock()
        .unwrap()
        .insert(request.workflow_id.clone(), request.workflow.clone());
    Ok(Json(LibraryWorkflowSaveResponse {
        workflow_id: request.workflow_id,
        workflow: request.workflow,
        saved: true,
    }))
}

async fn get_library_workflow(
    State(state): State<ApiState>,
    Path(workflow_id): Path<String>,
) -> Result<Json<LibraryWorkflowGetResponse>, ApiError> {
    let workflow = state
        .library_workflows
        .lock()
        .unwrap()
        .get(&workflow_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("workflow '{workflow_id}' was not found")))?;
    Ok(Json(LibraryWorkflowGetResponse {
        workflow_id,
        workflow,
    }))
}

async fn create_planner_chat_session(
    State(state): State<ApiState>,
    Json(request): Json<PlannerChatSessionCreateRequest>,
) -> Result<Json<PlannerChatSessionResponse>, ApiError> {
    let session_id = format!("pcs_{}", RunId::new());
    let workflow_id = request
        .workflow_id
        .unwrap_or_else(|| "planner-led".to_owned());
    let config = request.config.unwrap_or_else(default_project_config);
    let runtime =
        resolve_planner_runtime(&config, &workflow_id, request.planner_agent_id.as_deref())?;
    let session = PlannerChatSession {
        session_id: session_id.clone(),
        workflow_id: workflow_id.clone(),
        mode: normalize_planner_mode(request.mode.as_deref()),
        runtime: Some(runtime),
        ready: false,
        readiness: PlannerReadiness::NeedsClarification,
        plan_draft: None,
        open_questions: vec!["What exact outcome should this plan target?".to_owned()],
        acceptance_criteria: Vec::new(),
        risks: Vec::new(),
        turns: Vec::new(),
    };
    state
        .planner_sessions
        .lock()
        .unwrap()
        .insert(session_id.clone(), session.clone());
    Ok(Json(PlannerChatSessionResponse { session }))
}

async fn get_planner_chat_session(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<PlannerChatSessionResponse>, ApiError> {
    let session = state
        .planner_sessions
        .lock()
        .unwrap()
        .get(&session_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("session '{session_id}' was not found")))?;
    Ok(Json(PlannerChatSessionResponse { session }))
}

async fn planner_chat_turn(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Json(request): Json<PlannerChatTurnRequest>,
) -> Result<Json<PlannerChatTurnResponse>, ApiError> {
    let requested_mode = request.mode.clone();
    let confirmed = request.confirmed.unwrap_or(false);
    let provider_settings = state.provider_settings.lock().unwrap().clone();
    let conversation_request = {
        let mut sessions = state.planner_sessions.lock().unwrap();
        let session = sessions
            .get_mut(&session_id)
            .ok_or_else(|| ApiError::not_found(format!("session '{session_id}' was not found")))?;
        let mode = requested_mode
            .as_deref()
            .map(|mode| normalize_planner_mode(Some(mode)))
            .unwrap_or_else(|| normalize_planner_mode(Some(&session.mode)));
        session.mode = mode.clone();
        if request.config.is_some()
            || request.planner_agent_id.is_some()
            || session.runtime.is_none()
        {
            let config = request
                .config
                .clone()
                .unwrap_or_else(default_project_config);
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
            runtime,
            mode: mode.clone(),
            message: request.message.clone(),
            confirmed,
            history: session.turns.clone(),
            current_plan: session.plan_draft.clone(),
            provider_settings,
        }
    };

    let engine = ModelPlannerConversationEngine::new();
    let planner_response = engine
        .respond(conversation_request)
        .await
        .map_err(ApiError::internal)?;

    let mut sessions = state.planner_sessions.lock().unwrap();
    let session = sessions
        .get_mut(&session_id)
        .ok_or_else(|| ApiError::not_found(format!("session '{session_id}' was not found")))?;
    let mode = requested_mode
        .as_deref()
        .map(|mode| normalize_planner_mode(Some(mode)))
        .unwrap_or_else(|| normalize_planner_mode(Some(&session.mode)));
    session.mode = mode;
    session.turns.push(PlannerChatTurn {
        role: "user".to_owned(),
        content: request.message,
    });
    session.turns.push(PlannerChatTurn {
        role: "assistant".to_owned(),
        content: planner_response.assistant_message.clone(),
    });
    session.plan_draft = planner_response.plan_draft.clone();
    session.readiness = planner_response.readiness;
    session.ready = planner_response.readiness == PlannerReadiness::Ready;
    session.open_questions = planner_response.open_questions.clone();
    session.acceptance_criteria = planner_response.acceptance_criteria.clone();
    session.risks = planner_response.risks.clone();
    let events = planner_turn_events(session, &planner_response);
    Ok(Json(PlannerChatTurnResponse {
        session: session.clone(),
        assistant_message: planner_response.assistant_message,
        plan_draft: planner_response.plan_draft,
        readiness: planner_response.readiness,
        open_questions: planner_response.open_questions,
        acceptance_criteria: planner_response.acceptance_criteria,
        risks: planner_response.risks,
        suggested_mode: planner_response.suggested_mode,
        should_start_workflow: false,
        ready: session.ready,
        execution_allowed: false,
        run_preview: None,
        events,
    }))
}

async fn start_planner_chat_work(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Json(request): Json<PlannerStartWorkRequest>,
) -> Result<Json<PlannerStartWorkResponse>, ApiError> {
    let mut session = state
        .planner_sessions
        .lock()
        .unwrap()
        .get(&session_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("session '{session_id}' was not found")))?;
    let workflow_id = request
        .workflow_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| session.workflow_id.clone());
    let config = request
        .config
        .clone()
        .unwrap_or_else(default_project_config);
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
        session.turns.push(PlannerChatTurn {
            role: "assistant".to_owned(),
            content: assistant_message.clone(),
        });
        session.ready = false;
        session.readiness = PlannerReadiness::NeedsClarification;
        state
            .planner_sessions
            .lock()
            .unwrap()
            .insert(session_id.clone(), session.clone());
        return Ok(Json(PlannerStartWorkResponse {
            session,
            assistant_message: Some(assistant_message),
            run_id: None,
            status: "needs_clarification".to_owned(),
            events_url: None,
            timeline_url: None,
        }));
    }

    let provider_settings = state.provider_settings.lock().unwrap().clone();
    if let Some(message) =
        start_work_provider_config_error(&config, &workflow_id, &provider_settings)
    {
        let planner_response = planner_provider_setup_required_response(message);
        session.turns.push(PlannerChatTurn {
            role: "assistant".to_owned(),
            content: planner_response.assistant_message.clone(),
        });
        session.ready = false;
        session.readiness = planner_response.readiness;
        session.open_questions = planner_response.open_questions;
        session.acceptance_criteria = planner_response.acceptance_criteria;
        session.risks = planner_response.risks;
        state
            .planner_sessions
            .lock()
            .unwrap()
            .insert(session_id.clone(), session.clone());
        return Ok(Json(PlannerStartWorkResponse {
            session,
            assistant_message: Some(planner_response.assistant_message),
            run_id: None,
            status: "blocked".to_owned(),
            events_url: None,
            timeline_url: None,
        }));
    }

    let plan = session
        .plan_draft
        .clone()
        .ok_or_else(|| ApiError::bad_request("planner session has no plan draft"))?;
    let repo_root = request.repo.clone().unwrap_or_else(|| ".".to_owned());
    let plan_context = planner_run_context_from_session(&session, &plan);
    let task = task_from_plan(&plan);
    let mut options = WorkflowRunOptions::new(&workflow_id, &task);
    options.repo_root = PathBuf::from(&repo_root);
    options.plan_context = Some(plan_context);
    let runner = WorkflowRunner::new(config, state.store.clone());
    let output = runner.run(options).await?;
    let run_id = output.run_id.to_string();
    session.ready = false;
    session.turns.push(PlannerChatTurn {
        role: "assistant".to_owned(),
        content: format!("Work started for workflow '{workflow_id}'."),
    });
    state
        .planner_sessions
        .lock()
        .unwrap()
        .insert(session_id, session.clone());
    Ok(Json(PlannerStartWorkResponse {
        session,
        assistant_message: None,
        run_id: Some(run_id.clone()),
        status: format!("{:?}", output.report.status).to_lowercase(),
        events_url: Some(format!("/api/v3/runs/{run_id}/events")),
        timeline_url: Some(format!("/api/v3/runs/{run_id}/timeline")),
    }))
}

async fn load_project_memory(
    State(state): State<ApiState>,
    Json(request): Json<ProjectMemoryLoadRequest>,
) -> Result<Json<ProjectMemoryLoadResponse>, ApiError> {
    if request.requested_by_role != AgentMemoryRole::PlanningChat {
        return Err(ApiError::forbidden(
            "only planning_chat can read project long-term memory",
        ));
    }
    let memory_path = resolve_repo_relative_path(&request.repo_root, &request.memory_path)?;
    let memory = load_project_memory_file(&memory_path)?;
    let mut event_recorded = false;
    if let Some(run_id) = request.run_id {
        let run_id = RunId::from_string(run_id);
        if !stored_run_exists(&state.store, &run_id)? {
            return Err(ApiError::not_found(format!(
                "run '{}' was not found",
                run_id.as_str()
            )));
        }
        let sequence = state.store.read_events(&run_id)?.len() as u64 + 1;
        state.store.append_event(
            &run_id,
            &memory_read_event(run_id.clone(), sequence, &memory.records),
        )?;
        event_recorded = true;
    }
    Ok(Json(ProjectMemoryLoadResponse {
        record_count: memory.records.len(),
        event_recorded,
        memory,
    }))
}

async fn propose_project_memory_write(
    State(state): State<ApiState>,
    Json(request): Json<ProjectMemoryWriteProposalRequest>,
) -> Result<Json<ProjectMemoryWriteProposalResponse>, ApiError> {
    if request.record.scope != MemoryScope::Project {
        return Err(ApiError::bad_request(
            "project memory write proposals require scope 'project'",
        ));
    }
    if request.proposed_by_role != AgentMemoryRole::PlanningChat {
        return Err(ApiError::forbidden(
            "only planning_chat can propose project memory writes",
        ));
    }
    let run_id = RunId::from_string(request.run_id);
    if !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    let sequence = state.store.read_events(&run_id)?.len() as u64 + 1;
    let event = memory_write_proposed_event(run_id.clone(), sequence, &request.record);
    state.store.append_event(&run_id, &event)?;
    Ok(Json(ProjectMemoryWriteProposalResponse {
        run_id: run_id.to_string(),
        event_count: sequence as usize,
        event,
    }))
}

async fn confirm_project_memory_write(
    State(state): State<ApiState>,
    Json(request): Json<ProjectMemoryWriteConfirmRequest>,
) -> Result<Json<ProjectMemoryWriteConfirmResponse>, ApiError> {
    if request.record.scope != MemoryScope::Project {
        return Err(ApiError::bad_request(
            "project memory write confirmation requires scope 'project'",
        ));
    }
    ensure_memory_write_allowed(request.confirmed_by_role, &request.record)?;
    let memory_path = resolve_repo_relative_write_path(&request.repo_root, &request.memory_path)?;
    let run_id = request.run_id.map(RunId::from_string);
    if let Some(run_id) = &run_id {
        if !stored_run_exists(&state.store, run_id)? {
            return Err(ApiError::not_found(format!(
                "run '{}' was not found",
                run_id.as_str()
            )));
        }
    }
    let memory = append_project_memory_record(&memory_path, request.record.clone())?;
    let mut event = None;
    let mut event_count = 0usize;
    if let Some(run_id) = run_id {
        let sequence = state.store.read_events(&run_id)?.len() as u64 + 1;
        let confirmed_event = memory_write_confirmed_event(
            run_id.clone(),
            sequence,
            &request.record,
            request.confirmed_by_role,
        );
        state.store.append_event(&run_id, &confirmed_event)?;
        event_count = sequence as usize;
        event = Some(confirmed_event);
    }
    Ok(Json(ProjectMemoryWriteConfirmResponse {
        record_count: memory.records.len(),
        event_recorded: event.is_some(),
        event_count,
        event,
        memory,
    }))
}

async fn import_knowledge_text(
    Json(request): Json<KnowledgeTextImportApiRequest>,
) -> Result<Json<KnowledgeTextImportResponse>, ApiError> {
    let store = knowledge_store_for_repo(&request.repo_root)?;
    let result = import_text_knowledge_source(
        &store,
        KnowledgeTextImportRequest {
            title: request.title,
            text: request.text,
            owner_scope: request.owner_scope.unwrap_or_else(|| "project".to_owned()),
            tags: request.tags.unwrap_or_default(),
            allowed_agents: request.allowed_agents,
            purpose: request.purpose,
            allowed_contexts: request.allowed_contexts.unwrap_or_default(),
            sensitivity: request.sensitivity.unwrap_or(MemorySensitivity::Project),
        },
    )?;
    Ok(Json(KnowledgeTextImportResponse {
        source: result.source,
        chunks: result.chunks,
        index_dirty: true,
    }))
}

async fn list_knowledge_sources(
    Query(query): Query<RepoRootQuery>,
) -> Result<Json<KnowledgeSourceListResponse>, ApiError> {
    let store = knowledge_store_for_repo(&query.repo_root)?;
    Ok(Json(KnowledgeSourceListResponse {
        sources: store.list_sources()?,
    }))
}

async fn list_knowledge_source_chunks(
    Query(query): Query<RepoRootQuery>,
    Path(source_id): Path<String>,
) -> Result<Json<KnowledgeSourceChunksResponse>, ApiError> {
    let store = knowledge_store_for_repo(&query.repo_root)?;
    let chunks = store.list_chunks(Some(&source_id))?;
    if chunks.is_empty()
        && !store
            .list_sources()?
            .iter()
            .any(|source| source.source_id == source_id)
    {
        return Err(ApiError::not_found(format!(
            "knowledge source '{source_id}' was not found"
        )));
    }
    Ok(Json(KnowledgeSourceChunksResponse { source_id, chunks }))
}

async fn retrieve_knowledge(
    Json(request): Json<KnowledgeRetrieveApiRequest>,
) -> Result<Json<KnowledgeRetrieveResponse>, ApiError> {
    let store = knowledge_store_for_repo(&request.repo_root)?;
    let chunks = store.list_chunks(None)?;
    let results = retrieve_knowledge_hints(
        &chunks,
        &KnowledgeRetrievalRequest {
            role: request.role,
            query: request.query,
            requested_context: request.requested_context,
            backend: request.backend.unwrap_or_default(),
            scope: request.scope,
            tags: request.tags.unwrap_or_default(),
            token_budget: request.token_budget,
            max_results: request.max_results.or(request.top_k),
            include_content: request.include_content.unwrap_or(false),
        },
    )?;
    let hits = results
        .iter()
        .map(KnowledgeRetrievalHit::from_hint)
        .collect();
    Ok(Json(KnowledgeRetrieveResponse { results, hits }))
}

async fn validate_config(Json(request): Json<ConfigValidationRequest>) -> Json<ValidationReport> {
    Json(validate_project_config(&request.config))
}

async fn validate_workflow(
    Json(request): Json<WorkflowValidationRequest>,
) -> Result<Json<ValidationReport>, ApiError> {
    let report = validate_project_config(&request.config);
    if !request.config.workflows.contains_key(&request.workflow_id) {
        return Err(ApiError::not_found(format!(
            "workflow '{}' was not found",
            request.workflow_id
        )));
    }
    Ok(Json(report))
}

async fn validate_mcp(
    Json(request): Json<McpManifestValidationRequest>,
) -> Json<McpManifestValidation> {
    Json(validate_mcp_manifest(&request.manifest))
}

async fn list_mcp_servers() -> Json<McpServerListResponse> {
    Json(McpServerListResponse {
        servers: mock_mcp_servers(),
    })
}

async fn list_mcp_tools() -> Json<McpToolListResponse> {
    Json(McpToolListResponse {
        tools: mock_mcp_tools(),
    })
}

async fn invoke_mcp_tool(
    State(state): State<ApiState>,
    Json(request): Json<McpToolCallRequest>,
) -> Result<Json<McpToolCallResult>, ApiError> {
    if let Some(run_id) = &request.run_id {
        if !stored_run_exists(&state.store, run_id)? {
            return Err(ApiError::not_found(format!(
                "run '{}' was not found",
                run_id.as_str()
            )));
        }
        append_mcp_event(
            &state.store,
            run_id,
            "mcp.server.registered",
            json!({
                "server_id": request.server_id.as_str(),
                "enabled": false,
                "requires_approval": true
            }),
            None,
        )?;
        let discovered = find_mock_mcp_tool(&request.server_id, &request.tool_name);
        append_mcp_event(
            &state.store,
            run_id,
            "mcp.tool.discovered",
            json!({
                "server_id": request.server_id.as_str(),
                "tool_name": request.tool_name.as_str(),
                "discovered": discovered.is_some(),
                "enabled": false,
                "requires_approval": true,
                "risk": discovered.as_ref().map(|tool| tool.risk),
                "side_effect": discovered.as_ref().map(|tool| tool.side_effect)
            }),
            None,
        )?;
        append_mcp_event(
            &state.store,
            run_id,
            "mcp.approval.requested",
            json!({
                "server_id": request.server_id.as_str(),
                "tool_name": request.tool_name.as_str(),
                "approved": request.approved,
                "args_keys": json_object_keys(&request.args)
            }),
            None,
        )?;
    }

    let approved = request.approved;
    let mut result = if approved {
        if let Some(run_id) = &request.run_id {
            append_mcp_event(
                &state.store,
                run_id,
                "mcp.tool.started",
                json!({
                    "server_id": request.server_id.as_str(),
                    "tool_name": request.tool_name.as_str()
                }),
                None,
            )?;
        }
        invoke_mock_mcp_tool(&request)
    } else {
        invoke_mock_mcp_tool(&request)
    };

    if result.status == "failed" {
        attach_mcp_evidence(&state.store, &mut result)?;
    }
    externalize_large_mcp_output(&state.store, &mut result)?;

    if let Some(run_id) = &request.run_id {
        let event_kind = match result.status.as_str() {
            "completed" => "mcp.tool.completed",
            "failed" => "mcp.tool.failed",
            "blocked" => "mcp.tool.blocked",
            _ => "mcp.tool.failed",
        };
        append_mcp_event(
            &state.store,
            run_id,
            event_kind,
            json!({
                "server_id": request.server_id.as_str(),
                "tool_name": request.tool_name.as_str(),
                "status": result.status.as_str(),
                "requires_approval": result.requires_approval,
                "approval_key": result.approval_key.as_str(),
                "evidence_ref": result.evidence_ref.as_deref(),
                "output": &result.output
            }),
            result.evidence_ref.as_deref(),
        )?;
    }

    Ok(Json(result))
}

async fn list_extension_plugins() -> Json<ExtensionPluginListResponse> {
    Json(ExtensionPluginListResponse {
        plugins: builtin_plugin_manifests(),
    })
}

async fn validate_extension_plugin(
    Json(request): Json<ExtensionPluginValidationRequest>,
) -> Json<PluginManifestValidation> {
    Json(validate_plugin_manifest(&request.manifest))
}

async fn validate_extension_skill(
    Json(request): Json<SkillManifestValidationRequest>,
) -> Json<SkillManifestValidation> {
    Json(validate_skill_manifest(&request.manifest))
}

async fn list_extension_skills(State(state): State<ApiState>) -> Json<ExtensionSkillListResponse> {
    let skills = installed_skill_summaries(&state);
    let extensions = extension_search("", &[], &skills);
    Json(ExtensionSkillListResponse {
        skills: extensions
            .into_iter()
            .filter(|extension| extension.extension_type == "skill")
            .collect(),
    })
}

async fn list_extensions_installed(
    State(state): State<ApiState>,
) -> Json<ExtensionInstalledResponse> {
    let skills = installed_skill_summaries(&state);
    Json(ExtensionInstalledResponse {
        extensions: extension_search("", &builtin_plugin_manifests(), &skills),
    })
}

async fn search_extensions_endpoint(
    State(state): State<ApiState>,
    Query(query): Query<ExtensionSearchQuery>,
) -> Json<ExtensionInstalledResponse> {
    let skills = installed_skill_summaries(&state);
    Json(ExtensionInstalledResponse {
        extensions: extension_search(
            query.q.as_deref().unwrap_or_default(),
            &builtin_plugin_manifests(),
            &skills,
        ),
    })
}

async fn list_installed_skills(State(state): State<ApiState>) -> Json<InstalledSkillsPayload> {
    Json(installed_skills_payload(installed_skill_summaries(&state)))
}

async fn discover_skills_endpoint(
    State(state): State<ApiState>,
    Query(query): Query<SkillRegistryQuery>,
) -> Json<DiscoverSkillsPayload> {
    let installed_ids = state
        .installed_skills
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    Json(discover_skills_payload(
        query.registry_url.as_deref().unwrap_or_default(),
        &installed_ids,
    ))
}

async fn list_skill_updates(
    State(state): State<ApiState>,
    Query(_query): Query<SkillRegistryQuery>,
) -> Json<SkillUpdatesResponse> {
    let installed = state.installed_skills.lock().unwrap();
    let updates = installed
        .values()
        .map(skill_update_info)
        .collect::<Vec<_>>();
    Json(SkillUpdatesResponse { updates })
}

async fn install_skill(
    State(state): State<ApiState>,
    Json(request): Json<SkillInstallRequest>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let entry = available_skill(&request.skill_id).ok_or_else(|| {
        ApiError::not_found(format!("skill '{}' was not found", request.skill_id))
    })?;
    let mut installed = state.installed_skills.lock().unwrap();
    let previous = installed.get(&entry.id).cloned();
    let mut record = InstalledSkillRecord::from_remote(&entry, true, request.registry_url);
    if let Some(previous) = previous {
        record.history = previous.history;
        record.history.push(previous.summary);
        record.pinned_version = previous.pinned_version;
        record.update_policy = previous.update_policy;
    }
    let summary = record.summary.clone();
    installed.insert(summary.id.clone(), record);
    Ok(Json(SkillActionResponse {
        skill_id: summary.id.clone(),
        status: "installed".to_owned(),
        skill: Some(summary),
        deleted: false,
        updated: Vec::new(),
    }))
}

async fn update_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
    Json(request): Json<SkillUpdateRequest>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let entry = available_skill(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' was not found")))?;
    let mut installed = state.installed_skills.lock().unwrap();
    let current = installed
        .get(&skill_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    if current.pinned_version.is_some() && current.pinned_version.as_deref() != Some(&entry.version)
    {
        return Ok(Json(SkillActionResponse {
            skill_id,
            status: "pinned".to_owned(),
            skill: Some(current.summary),
            deleted: false,
            updated: Vec::new(),
        }));
    }
    let mut next =
        InstalledSkillRecord::from_remote(&entry, current.summary.enabled, request.registry_url);
    next.history = current.history;
    if current.summary.version != next.summary.version {
        next.history.push(current.summary);
    }
    next.pinned_version = current.pinned_version;
    next.update_policy = current.update_policy;
    let summary = next.summary.clone();
    installed.insert(skill_id.clone(), next);
    Ok(Json(SkillActionResponse {
        skill_id,
        status: "updated".to_owned(),
        skill: Some(summary),
        deleted: false,
        updated: Vec::new(),
    }))
}

async fn auto_update_skills(
    State(state): State<ApiState>,
    Json(_request): Json<SkillRegistryQuery>,
) -> Json<SkillActionResponse> {
    let mut installed = state.installed_skills.lock().unwrap();
    let mut updated = Vec::new();
    let ids = installed.keys().cloned().collect::<Vec<_>>();
    for skill_id in ids {
        let Some(current) = installed.get(&skill_id).cloned() else {
            continue;
        };
        if !auto_update_allowed(&current) {
            continue;
        }
        let Some(entry) = available_skill(&skill_id) else {
            continue;
        };
        if entry.version == current.summary.version {
            continue;
        }
        let mut next =
            InstalledSkillRecord::from_remote(&entry, current.summary.enabled, current.source_url);
        next.history = current.history;
        next.history.push(current.summary);
        next.update_policy = current.update_policy;
        updated.push(next.summary.clone());
        installed.insert(skill_id, next);
    }
    Json(SkillActionResponse {
        skill_id: "all".to_owned(),
        status: "auto_update_completed".to_owned(),
        skill: None,
        deleted: false,
        updated,
    })
}

async fn enable_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    set_skill_enabled(state, skill_id, true)
}

async fn disable_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    set_skill_enabled(state, skill_id, false)
}

async fn remove_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let removed = state
        .installed_skills
        .lock()
        .unwrap()
        .remove(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    Ok(Json(SkillActionResponse {
        skill_id: removed.summary.id,
        status: "removed".to_owned(),
        skill: None,
        deleted: true,
        updated: Vec::new(),
    }))
}

async fn pin_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
    Json(request): Json<SkillPinRequest>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let mut installed = state.installed_skills.lock().unwrap();
    let record = installed
        .get_mut(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    let version = request
        .version
        .filter(|version| !version.trim().is_empty())
        .unwrap_or_else(|| record.summary.version.clone());
    let available_versions = record
        .history
        .iter()
        .map(|skill| skill.version.clone())
        .chain(std::iter::once(record.summary.version.clone()))
        .collect::<BTreeSet<_>>();
    if !available_versions.contains(&version) {
        return Err(ApiError::bad_request(format!(
            "version '{version}' is not available for skill '{skill_id}'"
        )));
    }
    record.pinned_version = Some(version);
    record.update_policy = "manual".to_owned();
    Ok(Json(SkillActionResponse {
        skill_id,
        status: "pinned".to_owned(),
        skill: Some(record.summary.clone()),
        deleted: false,
        updated: Vec::new(),
    }))
}

async fn unpin_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let mut installed = state.installed_skills.lock().unwrap();
    let record = installed
        .get_mut(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    record.pinned_version = None;
    Ok(Json(SkillActionResponse {
        skill_id,
        status: "unpinned".to_owned(),
        skill: Some(record.summary.clone()),
        deleted: false,
        updated: Vec::new(),
    }))
}

async fn rollback_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
    Json(_request): Json<SkillPinRequest>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let mut installed = state.installed_skills.lock().unwrap();
    let record = installed
        .get_mut(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    let status = if let Some(previous) = record.history.pop() {
        record.summary = previous;
        "rolled_back"
    } else {
        "no_history"
    };
    Ok(Json(SkillActionResponse {
        skill_id,
        status: status.to_owned(),
        skill: Some(record.summary.clone()),
        deleted: false,
        updated: Vec::new(),
    }))
}

async fn set_skill_update_policy(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
    Json(request): Json<SkillUpdatePolicyRequest>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let mut installed = state.installed_skills.lock().unwrap();
    let record = installed
        .get_mut(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    match request.update_policy.as_str() {
        "manual" => record.update_policy = "manual".to_owned(),
        "auto_official_low_risk" if auto_update_allowed(record) => {
            record.update_policy = "auto_official_low_risk".to_owned();
        }
        "auto_official_low_risk" => {
            return Err(ApiError::bad_request(
                "auto-update is only allowed for official low-risk skills without external effects",
            ));
        }
        other => {
            return Err(ApiError::bad_request(format!(
                "unsupported update_policy '{other}'"
            )));
        }
    }
    Ok(Json(SkillActionResponse {
        skill_id,
        status: "update_policy_set".to_owned(),
        skill: Some(record.summary.clone()),
        deleted: false,
        updated: Vec::new(),
    }))
}

async fn developer_import_skill() -> Result<Json<SkillActionResponse>, ApiError> {
    Err(ApiError::forbidden(
        "developer skill import is disabled in Rust v3 baseline; use explicit user-controlled install flow",
    ))
}

async fn list_plugin_marketplaces(
    State(state): State<ApiState>,
) -> Json<PluginMarketplaceListResponse> {
    Json(PluginMarketplaceListResponse {
        marketplaces: state
            .plugin_marketplaces
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect(),
    })
}

async fn add_plugin_marketplace(
    State(state): State<ApiState>,
    Json(request): Json<PluginMarketplaceRequest>,
) -> Result<Json<PluginMarketplaceActionResponse>, ApiError> {
    if request.name.trim().is_empty() || request.url.trim().is_empty() {
        return Err(ApiError::bad_request(
            "marketplace name and url must not be empty",
        ));
    }
    let marketplace = PluginMarketplace {
        name: request.name,
        url: request.url,
        enabled: request.enabled.unwrap_or(true),
    };
    state
        .plugin_marketplaces
        .lock()
        .unwrap()
        .insert(marketplace.name.clone(), marketplace.clone());
    Ok(Json(PluginMarketplaceActionResponse {
        status: "added".to_owned(),
        marketplace,
    }))
}

async fn remove_plugin_marketplace(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<Json<PluginMarketplaceRemoveResponse>, ApiError> {
    if name == "builtin" {
        return Err(ApiError::bad_request(
            "the builtin marketplace cannot be removed",
        ));
    }
    let removed = state
        .plugin_marketplaces
        .lock()
        .unwrap()
        .remove(&name)
        .is_some();
    Ok(Json(PluginMarketplaceRemoveResponse { name, removed }))
}

async fn upgrade_plugin_marketplace(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<Json<PluginMarketplaceUpgradeResponse>, ApiError> {
    if !state
        .plugin_marketplaces
        .lock()
        .unwrap()
        .contains_key(&name)
    {
        return Err(ApiError::not_found(format!(
            "plugin marketplace '{name}' was not found"
        )));
    }
    Ok(Json(PluginMarketplaceUpgradeResponse {
        name,
        status: "up_to_date".to_owned(),
        updated_plugins: Vec::new(),
        updated_skills: Vec::new(),
    }))
}

async fn list_plugins() -> Json<PluginListResponse> {
    Json(PluginListResponse {
        plugins: builtin_plugin_manifests(),
    })
}

async fn list_installed_plugins() -> Json<PluginListResponse> {
    Json(PluginListResponse {
        plugins: builtin_plugin_manifests()
            .into_iter()
            .filter(|plugin| plugin.installed)
            .collect(),
    })
}

async fn read_plugin(Path(plugin_id): Path<String>) -> Result<Json<PluginReadResponse>, ApiError> {
    let plugin = builtin_plugin_manifests()
        .into_iter()
        .find(|plugin| plugin.id == plugin_id)
        .ok_or_else(|| ApiError::not_found(format!("plugin '{plugin_id}' was not found")))?;
    Ok(Json(PluginReadResponse {
        plugin,
        skills: builtin_remote_skill_entries(),
        mcp_dependencies: Vec::new(),
        hooks: builtin_hooks(),
    }))
}

async fn read_plugin_skill(
    Path((plugin_id, skill_name)): Path<(String, String)>,
) -> Result<Json<PluginSkillReadResponse>, ApiError> {
    if !builtin_plugin_manifests()
        .into_iter()
        .any(|plugin| plugin.id == plugin_id)
    {
        return Err(ApiError::not_found(format!(
            "plugin '{plugin_id}' was not found"
        )));
    }
    let skill = builtin_remote_skill_entries()
        .into_iter()
        .find(|skill| skill.id == skill_name || skill.name == skill_name)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_name}' was not found")))?;
    Ok(Json(PluginSkillReadResponse { plugin_id, skill }))
}

async fn list_skill_extra_roots(State(state): State<ApiState>) -> Json<SkillExtraRootsResponse> {
    Json(SkillExtraRootsResponse {
        roots: state.skill_extra_roots.lock().unwrap().clone(),
    })
}

async fn add_skill_extra_root(
    State(state): State<ApiState>,
    Json(request): Json<SkillExtraRootRequest>,
) -> Result<Json<SkillExtraRootsResponse>, ApiError> {
    if request.path.trim().is_empty() {
        return Err(ApiError::bad_request("skill root path must not be empty"));
    }
    let root = SkillExtraRoot {
        path: request.path,
        scope: request.scope.unwrap_or_else(|| "user".to_owned()),
        enabled: request.enabled.unwrap_or(true),
    };
    let mut roots = state.skill_extra_roots.lock().unwrap();
    if !roots.iter().any(|item| item.path == root.path) {
        roots.push(root);
    }
    Ok(Json(SkillExtraRootsResponse {
        roots: roots.clone(),
    }))
}

async fn list_hooks() -> Json<HooksResponse> {
    Json(HooksResponse {
        hooks: builtin_hooks(),
    })
}

async fn cache_status(
    State(state): State<ApiState>,
) -> Result<Json<CacheStatusResponse>, ApiError> {
    let probe_run_dir = state.store.run_dir(&RunId::from_string("__cache_probe__"));
    let runs_dir = probe_run_dir
        .parent()
        .map(FsPath::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("runs"));
    let store_root = runs_dir
        .parent()
        .map(FsPath::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let blob_entries = fs::read_dir(store_root.join("blobs"))
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0);
    Ok(Json(CacheStatusResponse {
        repo_index: CacheBucketStatus {
            entries: 0,
            bytes: 0,
            stale: false,
        },
        plugin_cache: CacheBucketStatus {
            entries: builtin_plugin_manifests().len(),
            bytes: 0,
            stale: false,
        },
        skill_cache: CacheBucketStatus {
            entries: installed_skill_summaries(&state).len(),
            bytes: 0,
            stale: false,
        },
        blob_store: CacheBucketStatus {
            entries: blob_entries,
            bytes: store_dir_size(&store_root.join("blobs")),
            stale: false,
        },
    }))
}

async fn cache_clear() -> Json<CacheActionResponse> {
    Json(CacheActionResponse {
        status: "noop".to_owned(),
        message: "Disposable cache clearing is not required for the current in-memory baseline."
            .to_owned(),
    })
}

async fn cache_reindex() -> Json<CacheTaskResponse> {
    Json(CacheTaskResponse {
        task_id: "repo-index-noop".to_owned(),
        status: "completed".to_owned(),
    })
}

async fn cache_tasks() -> Json<CacheTasksResponse> {
    Json(CacheTasksResponse { tasks: Vec::new() })
}

async fn cancel_cache_task(Path(task_id): Path<String>) -> Json<CacheTaskCancelResponse> {
    Json(CacheTaskCancelResponse {
        task_id,
        cancelled: false,
        status: "not_found".to_owned(),
    })
}

async fn list_harness_tools(Query(query): Query<ToolRegistryQuery>) -> Json<ToolRegistryResponse> {
    let registry = ToolRegistry::default();
    Json(ToolRegistryResponse {
        tools: registry.list_tools(query.harness_id.as_deref()),
        harness_id: query.harness_id,
    })
}

async fn get_provider_settings(State(state): State<ApiState>) -> Json<ProviderSettingsResponse> {
    Json(ProviderSettingsResponse {
        settings: state.provider_settings.lock().unwrap().clone(),
    })
}

async fn save_provider_settings(
    State(state): State<ApiState>,
    Json(request): Json<ProviderSettingsPatch>,
) -> Json<ProviderSettingsSaveResponse> {
    let mut settings = state.provider_settings.lock().unwrap();
    apply_provider_settings_patch(&mut settings, request);
    let status = provider_status(&settings, None);
    Json(ProviderSettingsSaveResponse {
        settings: settings.clone(),
        status,
    })
}

async fn get_provider_status(State(state): State<ApiState>) -> Json<ProviderStatus> {
    Json(provider_status(
        &state.provider_settings.lock().unwrap(),
        None,
    ))
}

async fn test_provider_status(
    State(state): State<ApiState>,
    Json(request): Json<ProviderTestRequest>,
) -> Json<ProviderTestResponse> {
    let settings = state.provider_settings.lock().unwrap().clone();
    let provider = request
        .provider
        .as_deref()
        .map(normalize_provider)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| settings.default_provider.clone());
    let test = test_provider_chat_completion(&settings, &provider, request.mock.unwrap_or(false))
        .await
        .unwrap_or_else(|message| ProviderTestResult {
            provider: provider.clone(),
            ok: false,
            mode: "live".to_owned(),
            model: settings.default_model.clone(),
            endpoint: provider_base_url(&settings, &provider)
                .map(|base_url| provider_chat_completions_endpoint_for_display(&base_url)),
            message,
        });
    Json(ProviderTestResponse {
        status: provider_status(&settings, Some(vec![provider])),
        test,
    })
}

async fn run_mock_workflow(
    State(state): State<ApiState>,
    Json(request): Json<MockRunRequest>,
) -> Result<Json<MockRunResponse>, ApiError> {
    let runner = MockWorkflowRunner::new(&request.config, state.store);
    let output = runner.run(&request.workflow_id, &request.task)?;
    Ok(Json(MockRunResponse {
        run_id: output.run_id.to_string(),
        report_ref: output.report_ref,
        report: output.report,
        events_url: format!("/api/v3/runs/{}/events", output.run_id.as_str()),
    }))
}

async fn run_workflow(
    State(state): State<ApiState>,
    Json(request): Json<MockRunRequest>,
) -> Result<Json<MockRunResponse>, ApiError> {
    let mut options = WorkflowRunOptions::new(&request.workflow_id, &request.task);
    if let Some(repo_root) = &request.repo_root {
        options.repo_root = PathBuf::from(repo_root);
    }
    options.plan_context = request.plan_context.clone();
    let runner = WorkflowRunner::new(request.config, state.store);
    let output = runner.run(options).await?;
    Ok(Json(MockRunResponse {
        run_id: output.run_id.to_string(),
        report_ref: output.report_ref,
        report: output.report,
        events_url: format!("/api/v3/runs/{}/events", output.run_id.as_str()),
    }))
}

async fn preview_run(Json(request): Json<RunPreviewRequest>) -> Json<RunPreviewResponse> {
    let mut issues = validate_project_config(&request.config).issues;
    let workflow = request.config.workflows.get(&request.workflow_id);
    if workflow.is_none() {
        issues.push(validation_issue(
            ValidationLevel::Error,
            "workflow_not_found",
            format!("workflow '{}' was not found", request.workflow_id),
            "workflow_id",
        ));
    }
    if request.task.trim().is_empty() {
        issues.push(validation_issue(
            ValidationLevel::Error,
            "task_empty",
            "task must not be empty",
            "task",
        ));
    }

    let status = if issues
        .iter()
        .any(|issue| issue.level == ValidationLevel::Error)
    {
        "blocked"
    } else {
        "ready"
    };
    let backends = workflow
        .map(|workflow| {
            workflow
                .nodes
                .iter()
                .filter_map(|node| request.config.harnesses.get(&node.harness))
                .map(|harness| harness.backend.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Json(RunPreviewResponse {
        status,
        requires_confirmation: status == "ready",
        workflow_id: request.workflow_id,
        task: request.task,
        backends,
        issues,
    })
}

async fn preview_command_endpoint(
    Json(request): Json<CommandPreviewRequest>,
) -> Result<Json<CommandPreview>, ApiError> {
    let preview = preview_command(
        &request.repo_root,
        request.cwd.unwrap_or_else(|| ".".to_owned()),
        request.argv,
        request.source.as_deref().unwrap_or("model"),
        request.sandbox.unwrap_or(false),
    )?;
    Ok(Json(preview))
}

async fn run_command_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<CommandRunToolRequest>,
) -> Result<Json<CommandRunResponse>, ApiError> {
    let CommandRunToolRequest {
        repo_root,
        cwd,
        argv,
        timeout_seconds,
        max_output_bytes,
        source,
        sandbox,
        approved,
        run_id,
    } = request;
    let output = run_command(
        &repo_root,
        CommandRunRequest {
            cwd: cwd.unwrap_or_else(|| ".".into()).into(),
            argv,
            timeout_seconds: timeout_seconds
                .unwrap_or(coder_tools::DEFAULT_COMMAND_TIMEOUT_SECONDS),
            max_output_bytes: max_output_bytes
                .unwrap_or(coder_tools::DEFAULT_MAX_COMMAND_OUTPUT_BYTES),
            source: source.unwrap_or_else(|| "model".to_owned()),
            sandbox: sandbox.unwrap_or(false),
            approved: approved.unwrap_or(false),
        },
    )?;
    let evidence_ref = write_tool_evidence(
        &state.store,
        run_id.as_deref(),
        RepoEvidenceKind::RepoTest,
        &repo_root,
        "Ran command through Rust tool endpoint.",
        json!({
            "evidence_kind": "command_evidence",
            "operation": "command_run",
            "result": serde_json::to_value(&output).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    if let (Some(run_id), Some(reference)) = (&run_id, &evidence_ref) {
        record_command_events(
            &state.store,
            &RunId::from_string(run_id.clone()),
            &output,
            reference,
        )?;
    }
    Ok(Json(CommandRunResponse {
        evidence_ref,
        result: output,
    }))
}

async fn repo_find_files_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<RepoFindFilesRequest>,
) -> Result<Json<RepoFindFilesResponse>, ApiError> {
    let files = find_files(
        &request.repo_root,
        request.query.as_deref(),
        &request.extensions.unwrap_or_default(),
        request
            .max_results
            .unwrap_or(coder_tools::DEFAULT_MAX_FILE_RESULTS),
    )?;
    let evidence_ref = write_tool_evidence(
        &state.store,
        request.run_id.as_deref(),
        RepoEvidenceKind::RepoFileList,
        &request.repo_root,
        format!("Found {} file(s).", files.len()),
        json!({
            "evidence_kind": "repo_evidence",
            "operation": "find_files",
            "files": serde_json::to_value(&files).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    Ok(Json(RepoFindFilesResponse {
        evidence_ref,
        files,
    }))
}

async fn repo_search_text_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<RepoSearchTextRequest>,
) -> Result<Json<RepoSearchTextResponse>, ApiError> {
    let matches = search_text(
        &request.repo_root,
        &request.query,
        &RepoToolConfig {
            max_file_bytes: request
                .max_file_bytes
                .unwrap_or(coder_tools::DEFAULT_MAX_FILE_BYTES),
            max_search_matches: request
                .max_matches
                .unwrap_or(coder_tools::DEFAULT_MAX_SEARCH_MATCHES),
        },
    )?;
    let evidence_ref = write_tool_evidence(
        &state.store,
        request.run_id.as_deref(),
        RepoEvidenceKind::RepoTextSearch,
        &request.repo_root,
        format!("Found {} text match(es).", matches.len()),
        json!({
            "evidence_kind": "repo_evidence",
            "operation": "search_text",
            "query": request.query,
            "matches": serde_json::to_value(&matches).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    Ok(Json(RepoSearchTextResponse {
        evidence_ref,
        matches,
    }))
}

async fn repo_read_file_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<RepoReadFileRequest>,
) -> Result<Json<RepoReadFileResponse>, ApiError> {
    let file = read_file(
        &request.repo_root,
        PathBuf::from(&request.path),
        &RepoToolConfig {
            max_file_bytes: request
                .max_file_bytes
                .unwrap_or(coder_tools::DEFAULT_MAX_FILE_BYTES),
            max_search_matches: coder_tools::DEFAULT_MAX_SEARCH_MATCHES,
        },
    )?;
    let evidence_ref = write_tool_evidence(
        &state.store,
        request.run_id.as_deref(),
        RepoEvidenceKind::RepoRead,
        &request.repo_root,
        format!("Read file '{}'.", file.path),
        json!({
            "evidence_kind": "repo_evidence",
            "operation": "read_file",
            "file": serde_json::to_value(&file).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    Ok(Json(RepoReadFileResponse { evidence_ref, file }))
}

async fn repo_read_file_range_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<RepoReadFileRangeRequest>,
) -> Result<Json<RepoReadFileRangeResponse>, ApiError> {
    let snippet = read_file_range(
        &request.repo_root,
        PathBuf::from(&request.path),
        request.start_line.unwrap_or(1),
        request.max_lines.unwrap_or(120),
        request.max_chars.unwrap_or(16_000),
    )?;
    let evidence_ref = write_tool_evidence(
        &state.store,
        request.run_id.as_deref(),
        RepoEvidenceKind::RepoRead,
        &request.repo_root,
        format!("Read file range '{}'.", snippet.path),
        json!({
            "evidence_kind": "repo_evidence",
            "operation": "read_file_range",
            "snippet": serde_json::to_value(&snippet).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    Ok(Json(RepoReadFileRangeResponse {
        evidence_ref,
        snippet,
    }))
}

async fn git_status_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<GitStatusRequest>,
) -> Result<Json<GitStatusResponse>, ApiError> {
    let status = git_status(&request.repo_root)?;
    let evidence_ref = write_tool_evidence(
        &state.store,
        request.run_id.as_deref(),
        RepoEvidenceKind::RepoDiff,
        &request.repo_root,
        "Captured git status.",
        json!({
            "evidence_kind": "repo_evidence",
            "operation": "git_status",
            "status": serde_json::to_value(&status).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    Ok(Json(GitStatusResponse {
        evidence_ref,
        status,
    }))
}

async fn git_diff_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<GitDiffRequest>,
) -> Result<Json<GitDiffResponse>, ApiError> {
    let diff = git_diff(
        &request.repo_root,
        request
            .max_output_bytes
            .unwrap_or(coder_tools::DEFAULT_MAX_GIT_OUTPUT_BYTES),
    )?;
    let evidence_ref = write_tool_evidence(
        &state.store,
        request.run_id.as_deref(),
        RepoEvidenceKind::RepoDiff,
        &request.repo_root,
        "Captured git diff.",
        json!({
            "evidence_kind": "repo_evidence",
            "operation": "git_diff",
            "diff": serde_json::to_value(&diff).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    Ok(Json(GitDiffResponse { evidence_ref, diff }))
}

async fn preview_patch_endpoint(
    Json(request): Json<PatchPreviewRequest>,
) -> Result<Json<PatchPreviewEvidence>, ApiError> {
    let preview = preview_patch_file(
        &request.repo_root,
        PathBuf::from(&request.patch_file),
        request
            .max_patch_bytes
            .unwrap_or(coder_tools::DEFAULT_MAX_PATCH_BYTES),
    )?;
    Ok(Json(preview))
}

async fn apply_patch_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<PatchApplyToolRequest>,
) -> Result<Json<PatchApplyResponse>, ApiError> {
    let run_id = request
        .run_id
        .as_deref()
        .map(RunId::from_string)
        .ok_or_else(|| ApiError::bad_request("run_id is required for patch apply"))?;
    let result = apply_patch_file(
        &request.repo_root,
        ToolPatchApplyRequest {
            patch_file: PathBuf::from(&request.patch_file),
            max_patch_bytes: request
                .max_patch_bytes
                .unwrap_or(coder_tools::DEFAULT_MAX_PATCH_BYTES),
            source: request.source.unwrap_or_else(|| "model".to_owned()),
            approved: request.approved.unwrap_or(false),
        },
    )?;
    let result_json =
        serde_json::to_value(&result).map_err(|error| ApiError::internal(error.to_string()))?;
    let evidence_ref = state.store.write_repo_evidence(
        &run_id,
        RepoEvidenceKind::RepoDiff,
        result.repo_root.clone(),
        Vec::new(),
        format!(
            "Patch apply {}: {} file(s).",
            result.status, result.preview.file_count
        ),
        json!({
            "evidence_kind": "patch_apply",
            "operation": "patch_apply",
            "result": result_json,
        }),
    )?;
    record_patch_apply_event(&state.store, &run_id, &result, &evidence_ref)?;
    Ok(Json(PatchApplyResponse {
        run_id: run_id.to_string(),
        evidence_ref,
        result,
    }))
}

async fn list_run_events(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunEventsResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let events = state.store.read_events(&run_id)?;
    Ok(Json(RunEventsResponse {
        run_id: run_id.to_string(),
        events,
    }))
}

async fn list_run_timeline(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunTimelineResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let events = state.store.read_events(&run_id)?;
    let report = state.store.read_report(&run_id)?;
    if events.is_empty() && report.is_none() && !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    let items = project_timeline_items(&run_id, &events, report.as_ref());
    Ok(Json(RunTimelineResponse {
        run_id: run_id.to_string(),
        items,
    }))
}

async fn list_run_changes(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunChangeSetListResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    if !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    let change_set = current_change_set(&state.store, &run_id)?;
    Ok(Json(RunChangeSetListResponse {
        run_id: run_id.to_string(),
        changes: change_set.into_iter().collect(),
    }))
}

async fn get_change_diff(
    State(state): State<ApiState>,
    Path((run_id, change_set_id)): Path<(String, String)>,
) -> Result<Json<ChangeSetDiffResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let change_set = read_change_set(&state.store, &run_id, &change_set_id)?;
    Ok(Json(ChangeSetDiffResponse {
        run_id: run_id.to_string(),
        change_set_id,
        diff: change_set.after_diff,
        truncated: change_set.diff_truncated,
    }))
}

async fn accept_change_set(
    State(state): State<ApiState>,
    Path((run_id, change_set_id)): Path<(String, String)>,
) -> Result<Json<ChangeSetActionResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let mut change_set = read_change_set(&state.store, &run_id, &change_set_id)?;
    change_set.status = ChangeSetStatus::Accepted;
    write_change_set(&state.store, &run_id, &change_set)?;
    append_change_set_event(
        &state.store,
        &run_id,
        "changeset.accepted",
        &change_set.change_set_id,
        json!({"changed_files": &change_set.changed_files}),
    )?;
    Ok(Json(ChangeSetActionResponse {
        run_id: run_id.to_string(),
        change_set,
        status: "accepted".to_owned(),
        message: "Change set accepted.".to_owned(),
    }))
}

async fn undo_change_set(
    State(state): State<ApiState>,
    Path((run_id, change_set_id)): Path<(String, String)>,
) -> Result<Json<ChangeSetActionResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let mut change_set = read_change_set(&state.store, &run_id, &change_set_id)?;
    append_change_set_event(
        &state.store,
        &run_id,
        "changeset.undo.started",
        &change_set.change_set_id,
        json!({}),
    )?;
    let current_diff = git_diff(&change_set.repo_root, usize::MAX)?.preview;
    if current_diff != change_set.after_diff {
        change_set.status = ChangeSetStatus::FailedToUndo;
        write_change_set(&state.store, &run_id, &change_set)?;
        append_change_set_event(
            &state.store,
            &run_id,
            "changeset.undo.failed",
            &change_set.change_set_id,
            json!({"reason": "current diff differs from recorded review diff"}),
        )?;
        return Err(ApiError::conflict(
            "current working tree diff differs from the recorded change set; refusing to undo",
        ));
    }
    apply_reverse_diff(&change_set.repo_root, &change_set.after_diff)?;
    change_set.status = ChangeSetStatus::Undone;
    write_change_set(&state.store, &run_id, &change_set)?;
    append_change_set_event(
        &state.store,
        &run_id,
        "changeset.undo.completed",
        &change_set.change_set_id,
        json!({"changed_files": &change_set.changed_files}),
    )?;
    Ok(Json(ChangeSetActionResponse {
        run_id: run_id.to_string(),
        change_set,
        status: "undone".to_owned(),
        message: "Change set undone with reverse patch.".to_owned(),
    }))
}

async fn list_runs(State(state): State<ApiState>) -> Result<Json<RunListResponse>, ApiError> {
    Ok(Json(RunListResponse {
        runs: state.store.list_run_summaries()?,
    }))
}

async fn get_run_detail(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunDetailResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let metadata = state.store.read_metadata(&run_id)?;
    let events = state.store.read_events(&run_id)?;
    let report = state.store.read_report(&run_id)?;
    let repo_evidence_count = state.store.repo_evidence_count(&run_id)?;
    if metadata.is_none() && events.is_empty() && report.is_none() && repo_evidence_count == 0 {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    Ok(Json(RunDetailResponse {
        run_id: run_id.to_string(),
        metadata,
        events,
        report,
        repo_evidence_count,
    }))
}

async fn pause_run(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunControlResponse>, ApiError> {
    control_run(state, RunId::from_string(run_id), RunControlAction::Pause)
}

async fn resume_run(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunControlResponse>, ApiError> {
    control_run(state, RunId::from_string(run_id), RunControlAction::Resume)
}

async fn cancel_run(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunControlResponse>, ApiError> {
    control_run(state, RunId::from_string(run_id), RunControlAction::Cancel)
}

async fn run_heartbeat(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunHeartbeatResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let metadata = state.store.read_metadata(&run_id)?;
    let events = state.store.read_events(&run_id)?;
    let repo_evidence_count = state.store.repo_evidence_count(&run_id)?;
    let has_report = state.store.read_report(&run_id)?.is_some();
    if metadata.is_none() && events.is_empty() && repo_evidence_count == 0 && !has_report {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    Ok(Json(RunHeartbeatResponse {
        run_id: run_id.to_string(),
        status: metadata.as_ref().map(|state| state.status),
        event_count: events.len(),
        has_report,
        repo_evidence_count,
    }))
}

async fn preview_run_report(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunReportResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let report = state.store.build_evidence_report(&run_id)?;
    Ok(Json(RunReportResponse {
        run_id: run_id.to_string(),
        report_ref: None,
        report,
    }))
}

async fn write_run_report(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunReportResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let report = state.store.build_evidence_report(&run_id)?;
    let report_ref = state.store.write_report(&run_id, &report)?;
    Ok(Json(RunReportResponse {
        run_id: run_id.to_string(),
        report_ref: Some(report_ref),
        report,
    }))
}

async fn get_repo_evidence(
    State(state): State<ApiState>,
    Path(ref_id): Path<String>,
) -> Result<Json<RepoEvidenceResponse>, ApiError> {
    let payload = state.store.read_repo_evidence(&ref_id)?;
    Ok(Json(RepoEvidenceResponse { ref_id, payload }))
}

async fn list_run_repo_evidence(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunRepoEvidenceResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let evidence = state.store.list_repo_evidence(&run_id)?;
    Ok(Json(RunRepoEvidenceResponse {
        run_id: run_id.to_string(),
        evidence,
    }))
}

async fn get_run_artifact(
    State(state): State<ApiState>,
    Path((run_id, artifact_name)): Path<(String, String)>,
) -> Result<Json<RunArtifactResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let payload = state.store.read_artifact_json(&run_id, &artifact_name)?;
    Ok(Json(RunArtifactResponse {
        run_id: run_id.to_string(),
        artifact_name,
        payload,
    }))
}

async fn list_run_checkpoints(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunCheckpointListResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let checkpoints = state.store.list_checkpoints(&run_id)?;
    if checkpoints.is_empty() && !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    Ok(Json(RunCheckpointListResponse {
        run_id: run_id.to_string(),
        checkpoints,
    }))
}

async fn get_run_checkpoint(
    State(state): State<ApiState>,
    Path((run_id, checkpoint_name)): Path<(String, String)>,
) -> Result<Json<RunCheckpointResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let payload = state
        .store
        .read_checkpoint_json(&run_id, &checkpoint_name)?;
    Ok(Json(RunCheckpointResponse {
        run_id: run_id.to_string(),
        checkpoint_name,
        payload,
    }))
}

async fn write_run_checkpoint(
    State(state): State<ApiState>,
    Path((run_id, checkpoint_name)): Path<(String, String)>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<RunCheckpointWriteResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    if !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    let checkpoint_ref = state
        .store
        .write_checkpoint(&run_id, &checkpoint_name, &payload)?;
    Ok(Json(RunCheckpointWriteResponse {
        run_id: run_id.to_string(),
        checkpoint_name,
        checkpoint_ref,
    }))
}

async fn get_blob_sha256(
    State(state): State<ApiState>,
    Path(digest): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let content = state.store.read_blob_sha256(&digest)?;
    Ok((
        StatusCode::OK,
        [("content-type", "application/octet-stream")],
        content,
    ))
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    api_version: &'static str,
}

#[derive(Debug, Serialize)]
pub struct CapabilitiesResponse {
    pub api_version: &'static str,
    pub workflow: Vec<&'static str>,
    pub runs: Vec<&'static str>,
    pub tools: Vec<&'static str>,
    pub planner_chat: Vec<&'static str>,
    pub settings: Vec<&'static str>,
    pub extensions: Vec<&'static str>,
    pub memory: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct AgentRoleCardsResponse {
    pub role_cards: Vec<AgentRoleCard>,
}

#[derive(Debug, Serialize)]
pub struct AgentRoleCard {
    pub id: &'static str,
    pub label: &'static str,
    pub archetype: &'static str,
    pub role: &'static str,
    pub engine_id: &'static str,
    pub default_capabilities: Vec<&'static str>,
    pub description: &'static str,
    pub default_output_contract: &'static str,
}

#[derive(Debug, Serialize)]
pub struct DefaultWorkflowResponse {
    pub workflow_id: String,
    pub config: ProjectConfig,
    pub workflow: Option<coder_config::WorkflowSpec>,
}

#[derive(Debug, Serialize)]
pub struct LibraryResponse {
    pub workflows: Vec<LibraryWorkflowSummary>,
}

#[derive(Debug, Serialize)]
pub struct LibraryWorkflowSummary {
    pub id: String,
    pub workflow: Value,
}

#[derive(Debug, Deserialize)]
pub struct LibraryWorkflowSaveRequest {
    pub workflow_id: String,
    pub workflow: Value,
}

#[derive(Debug, Serialize)]
pub struct LibraryWorkflowSaveResponse {
    pub workflow_id: String,
    pub workflow: Value,
    pub saved: bool,
}

#[derive(Debug, Serialize)]
pub struct LibraryWorkflowGetResponse {
    pub workflow_id: String,
    pub workflow: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerChatSession {
    pub session_id: String,
    pub workflow_id: String,
    pub mode: String,
    #[serde(skip)]
    pub runtime: Option<PlannerRuntimeContext>,
    pub ready: bool,
    pub readiness: PlannerReadiness,
    pub plan_draft: Option<PlanDraft>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    pub turns: Vec<PlannerChatTurn>,
}

#[derive(Debug, Clone)]
pub struct PlannerRuntimeContext {
    pub workflow_id: String,
    pub workflow_name: String,
    pub node_id: String,
    pub agent_id: String,
    pub harness_id: String,
    pub agent: ConfigAgentSpec,
    pub harness: ConfigHarnessSpec,
    pub model: ConfigModelSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct PlannerChatSessionCreateRequest {
    pub workflow_id: Option<String>,
    pub planner_agent_id: Option<String>,
    pub config: Option<ProjectConfig>,
    pub mode: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PlannerChatSessionResponse {
    pub session: PlannerChatSession,
}

#[derive(Debug, Deserialize)]
pub struct PlannerChatTurnRequest {
    pub message: String,
    pub confirmed: Option<bool>,
    pub mode: Option<String>,
    pub planner_agent_id: Option<String>,
    pub config: Option<ProjectConfig>,
}

#[derive(Debug, Serialize)]
pub struct PlannerChatTurnResponse {
    pub session: PlannerChatSession,
    pub assistant_message: String,
    pub plan_draft: Option<PlanDraft>,
    pub readiness: PlannerReadiness,
    pub open_questions: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub risks: Vec<String>,
    pub suggested_mode: String,
    pub should_start_workflow: bool,
    pub ready: bool,
    pub execution_allowed: bool,
    pub run_preview: Option<Value>,
    #[serde(default)]
    pub events: Vec<Value>,
}

#[derive(Debug, Deserialize)]
pub struct PlannerStartWorkRequest {
    pub repo: Option<String>,
    pub workflow_id: Option<String>,
    pub planner_agent_id: Option<String>,
    pub config: Option<ProjectConfig>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub skill_pack_ids: Vec<String>,
    #[serde(default)]
    pub knowledge_pack_ids: Vec<String>,
    #[serde(default)]
    pub memory_pack_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PlannerStartWorkResponse {
    pub session: PlannerChatSession,
    pub assistant_message: Option<String>,
    pub run_id: Option<String>,
    pub status: String,
    pub events_url: Option<String>,
    pub timeline_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannerReadiness {
    Ready,
    NeedsClarification,
    Blocked,
    Casual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanDraft {
    pub goal: String,
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub non_goals: Vec<String>,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub affected_paths: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    pub selected_workflow_id: String,
    #[serde(default)]
    pub memory_proposals: Vec<MemoryProposalDraft>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryProposalDraft {
    pub scope: String,
    pub key: String,
    pub content: String,
    pub rationale: String,
    pub requires_confirmation: bool,
}

#[derive(Debug, Clone)]
pub struct PlannerConversationRequest {
    pub session_id: String,
    pub workflow_id: String,
    pub runtime: PlannerRuntimeContext,
    pub mode: String,
    pub message: String,
    pub confirmed: bool,
    pub history: Vec<PlannerChatTurn>,
    pub current_plan: Option<PlanDraft>,
    pub provider_settings: ProviderSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerConversationResponse {
    pub assistant_message: String,
    pub plan_draft: Option<PlanDraft>,
    pub readiness: PlannerReadiness,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    pub suggested_mode: String,
    pub should_start_workflow: bool,
}

#[async_trait]
pub trait PlannerConversationEngine {
    async fn respond(
        &self,
        request: PlannerConversationRequest,
    ) -> Result<PlannerConversationResponse, String>;
}

#[derive(Debug, Deserialize)]
pub struct ProjectMemoryLoadRequest {
    pub repo_root: String,
    pub memory_path: String,
    pub requested_by_role: AgentMemoryRole,
    pub run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectMemoryLoadResponse {
    pub record_count: usize,
    pub event_recorded: bool,
    pub memory: ProjectMemoryFile,
}

#[derive(Debug, Deserialize)]
pub struct ProjectMemoryWriteProposalRequest {
    pub run_id: String,
    pub proposed_by_role: AgentMemoryRole,
    pub record: MemoryRecord,
}

#[derive(Debug, Serialize)]
pub struct ProjectMemoryWriteProposalResponse {
    pub run_id: String,
    pub event_count: usize,
    pub event: coder_events::CoderEvent,
}

#[derive(Debug, Deserialize)]
pub struct ProjectMemoryWriteConfirmRequest {
    pub repo_root: String,
    pub memory_path: String,
    pub run_id: Option<String>,
    pub record: MemoryRecord,
    pub confirmed_by_role: AgentMemoryRole,
}

#[derive(Debug, Serialize)]
pub struct ProjectMemoryWriteConfirmResponse {
    pub record_count: usize,
    pub event_recorded: bool,
    pub event_count: usize,
    pub event: Option<coder_events::CoderEvent>,
    pub memory: ProjectMemoryFile,
}

#[derive(Debug, Deserialize)]
pub struct KnowledgeTextImportApiRequest {
    pub repo_root: String,
    pub title: String,
    pub text: String,
    pub owner_scope: Option<String>,
    pub tags: Option<Vec<String>>,
    pub allowed_agents: Vec<AgentMemoryRole>,
    pub purpose: Vec<MemoryPurpose>,
    pub allowed_contexts: Option<Vec<MemoryAllowedContext>>,
    pub sensitivity: Option<MemorySensitivity>,
}

#[derive(Debug, Serialize)]
pub struct KnowledgeTextImportResponse {
    pub source: KnowledgeSource,
    pub chunks: Vec<KnowledgeChunk>,
    pub index_dirty: bool,
}

#[derive(Debug, Deserialize)]
pub struct RepoRootQuery {
    pub repo_root: String,
}

#[derive(Debug, Serialize)]
pub struct KnowledgeSourceListResponse {
    pub sources: Vec<KnowledgeSource>,
}

#[derive(Debug, Serialize)]
pub struct KnowledgeSourceChunksResponse {
    pub source_id: String,
    pub chunks: Vec<KnowledgeChunk>,
}

#[derive(Debug, Deserialize)]
pub struct KnowledgeRetrieveApiRequest {
    pub repo_root: String,
    pub role: AgentMemoryRole,
    pub query: String,
    pub requested_context: MemoryAllowedContext,
    pub backend: Option<RetrievalBackendKind>,
    pub scope: Option<String>,
    pub tags: Option<Vec<String>>,
    pub token_budget: Option<usize>,
    pub top_k: Option<usize>,
    pub max_results: Option<usize>,
    pub include_content: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct KnowledgeRetrieveResponse {
    pub results: Vec<coder_memory::KnowledgeHint>,
    pub hits: Vec<KnowledgeRetrievalHit>,
}

#[derive(Debug, Deserialize)]
pub struct ConfigValidationRequest {
    pub config: ProjectConfig,
}

#[derive(Debug, Deserialize)]
pub struct WorkflowValidationRequest {
    pub config: ProjectConfig,
    pub workflow_id: String,
}

#[derive(Debug, Deserialize)]
pub struct McpManifestValidationRequest {
    pub manifest: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct McpServerListResponse {
    pub servers: Vec<McpServerSummary>,
}

#[derive(Debug, Serialize)]
pub struct McpToolListResponse {
    pub tools: Vec<McpToolSummary>,
}

#[derive(Debug, Deserialize)]
pub struct ExtensionPluginValidationRequest {
    pub manifest: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ExtensionPluginListResponse {
    pub plugins: Vec<PluginManifest>,
}

#[derive(Debug, Deserialize)]
pub struct SkillManifestValidationRequest {
    pub manifest: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ExtensionSkillListResponse {
    pub skills: Vec<ExtensionManifestSummary>,
}

#[derive(Debug, Serialize)]
pub struct ExtensionInstalledResponse {
    pub extensions: Vec<ExtensionManifestSummary>,
}

#[derive(Debug, Deserialize)]
pub struct ExtensionSearchQuery {
    pub q: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SkillRegistryQuery {
    pub registry_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SkillInstallRequest {
    pub skill_id: String,
    pub registry_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SkillUpdateRequest {
    pub registry_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SkillPinRequest {
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SkillUpdatePolicyRequest {
    pub update_policy: String,
}

#[derive(Debug, Serialize)]
pub struct SkillUpdatesResponse {
    pub updates: Vec<SkillUpdateInfo>,
}

#[derive(Debug, Serialize)]
pub struct SkillActionResponse {
    pub skill_id: String,
    pub status: String,
    pub skill: Option<SkillSummary>,
    pub deleted: bool,
    pub updated: Vec<SkillSummary>,
}

#[derive(Debug, Clone)]
struct InstalledSkillRecord {
    summary: SkillSummary,
    source_url: Option<String>,
    pinned_version: Option<String>,
    update_policy: String,
    history: Vec<SkillSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMarketplace {
    pub name: String,
    pub url: String,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct PluginMarketplaceRequest {
    pub name: String,
    pub url: String,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct PluginMarketplaceListResponse {
    pub marketplaces: Vec<PluginMarketplace>,
}

#[derive(Debug, Serialize)]
pub struct PluginMarketplaceActionResponse {
    pub status: String,
    pub marketplace: PluginMarketplace,
}

#[derive(Debug, Serialize)]
pub struct PluginMarketplaceRemoveResponse {
    pub name: String,
    pub removed: bool,
}

#[derive(Debug, Serialize)]
pub struct PluginMarketplaceUpgradeResponse {
    pub name: String,
    pub status: String,
    pub updated_plugins: Vec<PluginManifest>,
    pub updated_skills: Vec<RemoteSkillEntry>,
}

#[derive(Debug, Serialize)]
pub struct PluginListResponse {
    pub plugins: Vec<PluginManifest>,
}

#[derive(Debug, Serialize)]
pub struct PluginReadResponse {
    pub plugin: PluginManifest,
    pub skills: Vec<RemoteSkillEntry>,
    pub mcp_dependencies: Vec<Value>,
    pub hooks: Vec<HookSummary>,
}

#[derive(Debug, Serialize)]
pub struct PluginSkillReadResponse {
    pub plugin_id: String,
    pub skill: RemoteSkillEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillExtraRoot {
    pub path: String,
    pub scope: String,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct SkillExtraRootRequest {
    pub path: String,
    pub scope: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct SkillExtraRootsResponse {
    pub roots: Vec<SkillExtraRoot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookSummary {
    pub id: String,
    pub trigger: String,
    pub enabled: bool,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct HooksResponse {
    pub hooks: Vec<HookSummary>,
}

#[derive(Debug, Serialize)]
pub struct CacheBucketStatus {
    pub entries: usize,
    pub bytes: u64,
    pub stale: bool,
}

#[derive(Debug, Serialize)]
pub struct CacheStatusResponse {
    pub repo_index: CacheBucketStatus,
    pub plugin_cache: CacheBucketStatus,
    pub skill_cache: CacheBucketStatus,
    pub blob_store: CacheBucketStatus,
}

#[derive(Debug, Serialize)]
pub struct CacheActionResponse {
    pub status: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CacheTaskSummary {
    pub task_id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct CacheTaskResponse {
    pub task_id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct CacheTasksResponse {
    pub tasks: Vec<CacheTaskSummary>,
}

#[derive(Debug, Serialize)]
pub struct CacheTaskCancelResponse {
    pub task_id: String,
    pub cancelled: bool,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderKeyState {
    pub configured: bool,
    pub source: String,
    #[serde(default, skip_serializing, skip_deserializing)]
    pub secret: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSettings {
    pub default_provider: String,
    pub default_model: String,
    pub base_urls: BTreeMap<String, String>,
    pub api_keys: BTreeMap<String, ProviderKeyState>,
    pub mock_mode: bool,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            default_provider: "deepseek".to_owned(),
            default_model: "deepseek-v4-flash".to_owned(),
            base_urls: BTreeMap::from([(
                "deepseek".to_owned(),
                "https://api.deepseek.com".to_owned(),
            )]),
            api_keys: BTreeMap::new(),
            mock_mode: false,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ProviderSettingsPatch {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub base_urls: Option<BTreeMap<String, String>>,
    pub api_keys: Option<BTreeMap<String, Value>>,
    pub mock_mode: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ProviderSettingsResponse {
    pub settings: ProviderSettings,
}

#[derive(Debug, Serialize)]
pub struct ProviderSettingsSaveResponse {
    pub settings: ProviderSettings,
    pub status: ProviderStatus,
}

#[derive(Debug, Deserialize)]
pub struct ProviderTestRequest {
    pub provider: Option<String>,
    pub mock: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ProviderTestResponse {
    pub status: ProviderStatus,
    pub test: ProviderTestResult,
}

#[derive(Debug, Serialize)]
pub struct ProviderTestResult {
    pub provider: String,
    pub ok: bool,
    pub mode: String,
    pub model: String,
    pub endpoint: Option<String>,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderStatusItem {
    pub provider: String,
    pub configured: bool,
    pub credential_configured: bool,
    pub credential_source: String,
    pub base_url: Option<String>,
    pub mode: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderStatus {
    pub default_provider: String,
    pub default_model: String,
    pub mock_mode: bool,
    pub default_status: ProviderStatusItem,
    pub providers: Vec<ProviderStatusItem>,
}

#[derive(Debug, Deserialize)]
pub struct ToolRegistryQuery {
    pub harness_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ToolRegistryResponse {
    pub harness_id: Option<String>,
    pub tools: Vec<ToolRegistryEntry>,
}

#[derive(Debug, Deserialize)]
pub struct MockRunRequest {
    pub config: ProjectConfig,
    pub workflow_id: String,
    pub task: String,
    pub repo_root: Option<String>,
    pub plan_context: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct RunPreviewRequest {
    pub config: ProjectConfig,
    pub workflow_id: String,
    pub task: String,
}

#[derive(Debug, Deserialize)]
pub struct CommandPreviewRequest {
    pub repo_root: String,
    pub cwd: Option<String>,
    pub argv: Vec<String>,
    pub source: Option<String>,
    pub sandbox: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CommandRunToolRequest {
    pub repo_root: String,
    pub cwd: Option<String>,
    pub argv: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub max_output_bytes: Option<usize>,
    pub source: Option<String>,
    pub sandbox: Option<bool>,
    pub approved: Option<bool>,
    pub run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommandRunResponse {
    pub evidence_ref: Option<RepoEvidenceRef>,
    pub result: CommandRunEvidence,
}

#[derive(Debug, Deserialize)]
pub struct RepoFindFilesRequest {
    pub repo_root: String,
    pub query: Option<String>,
    pub extensions: Option<Vec<String>>,
    pub max_results: Option<usize>,
    pub run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RepoFindFilesResponse {
    pub evidence_ref: Option<RepoEvidenceRef>,
    pub files: Vec<RepoFileRef>,
}

#[derive(Debug, Deserialize)]
pub struct RepoSearchTextRequest {
    pub repo_root: String,
    pub query: String,
    pub max_file_bytes: Option<u64>,
    pub max_matches: Option<usize>,
    pub run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RepoSearchTextResponse {
    pub evidence_ref: Option<RepoEvidenceRef>,
    pub matches: Vec<RepoSearchMatch>,
}

#[derive(Debug, Deserialize)]
pub struct RepoReadFileRequest {
    pub repo_root: String,
    pub path: String,
    pub max_file_bytes: Option<u64>,
    pub run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RepoReadFileResponse {
    pub evidence_ref: Option<RepoEvidenceRef>,
    pub file: RepoFileEvidence,
}

#[derive(Debug, Deserialize)]
pub struct RepoReadFileRangeRequest {
    pub repo_root: String,
    pub path: String,
    pub start_line: Option<usize>,
    pub max_lines: Option<usize>,
    pub max_chars: Option<usize>,
    pub run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RepoReadFileRangeResponse {
    pub evidence_ref: Option<RepoEvidenceRef>,
    pub snippet: RepoReadSnippet,
}

#[derive(Debug, Deserialize)]
pub struct GitStatusRequest {
    pub repo_root: String,
    pub run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GitStatusResponse {
    pub evidence_ref: Option<RepoEvidenceRef>,
    pub status: GitStatusEvidence,
}

#[derive(Debug, Deserialize)]
pub struct GitDiffRequest {
    pub repo_root: String,
    pub max_output_bytes: Option<usize>,
    pub run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GitDiffResponse {
    pub evidence_ref: Option<RepoEvidenceRef>,
    pub diff: GitDiffEvidence,
}

#[derive(Debug, Deserialize)]
pub struct PatchPreviewRequest {
    pub repo_root: String,
    pub patch_file: String,
    pub max_patch_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct PatchApplyToolRequest {
    pub repo_root: String,
    pub patch_file: String,
    pub max_patch_bytes: Option<usize>,
    pub source: Option<String>,
    pub approved: Option<bool>,
    pub run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PatchApplyResponse {
    pub run_id: String,
    pub evidence_ref: RepoEvidenceRef,
    pub result: PatchApplyEvidence,
}

#[derive(Debug, Serialize)]
pub struct MockRunResponse {
    pub run_id: String,
    pub report_ref: String,
    pub report: coder_core::FinalReport,
    pub events_url: String,
}

#[derive(Debug, Serialize)]
pub struct RunEventsResponse {
    pub run_id: String,
    pub events: Vec<coder_events::CoderEvent>,
}

#[derive(Debug, Serialize)]
pub struct RunTimelineResponse {
    pub run_id: String,
    pub items: Vec<TimelineItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TimelineItem {
    UserMessage(MessageTimelineItem),
    PlannerMessage(MessageTimelineItem),
    ReasoningSummary(ReasoningSummaryItem),
    PlanUpdate(PlanUpdateItem),
    ExecutorStep(ExecutorStepItem),
    ToolCall(ToolCallItem),
    CommandExecution(CommandExecutionItem),
    FileChange(FileChangeItem),
    Approval(ApprovalItem),
    Verification(VerificationItem),
    FinalSummary(FinalSummaryItem),
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageTimelineItem {
    pub id: String,
    pub agent_id: String,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReasoningSummaryItem {
    pub id: String,
    pub agent_id: String,
    pub summary_text: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanUpdateItem {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub summary: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutorStepItem {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub status: String,
    pub summary: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallItem {
    pub id: String,
    pub agent_id: String,
    pub tool_name: String,
    pub status: String,
    pub summary: Option<String>,
    pub evidence_ref: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandExecutionItem {
    pub id: String,
    pub agent_id: String,
    pub command: Vec<String>,
    pub cwd: String,
    pub status: String,
    pub stdout_preview: Option<String>,
    pub stderr_preview: Option<String>,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<u64>,
    pub evidence_ref: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileChangeItem {
    pub id: String,
    pub agent_id: String,
    pub path: String,
    pub change_type: String,
    pub diff_ref: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalItem {
    pub id: String,
    pub agent_id: String,
    pub risk_level: String,
    pub action_type: String,
    pub summary: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerificationItem {
    pub id: String,
    pub agent_id: String,
    pub status: String,
    pub summary: String,
    pub evidence_ref: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FinalSummaryItem {
    pub id: String,
    pub agent_id: String,
    pub summary: String,
    pub changed_files: Vec<String>,
    pub checks: Vec<String>,
    pub evidence_refs: Vec<coder_core::EvidenceRef>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct RunChangeSetListResponse {
    pub run_id: String,
    pub changes: Vec<ChangeSet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeSet {
    pub change_set_id: String,
    pub run_id: String,
    pub repo_root: String,
    pub status: ChangeSetStatus,
    pub created_at: String,
    pub base_git_head: Option<String>,
    pub before_checkpoint_ref: Option<String>,
    pub after_diff_ref: Option<String>,
    pub reverse_patch_ref: Option<String>,
    pub changed_files: Vec<ChangedFileSummary>,
    pub command_checks: Vec<CommandCheckSummary>,
    pub evidence_refs: Vec<coder_core::EvidenceRef>,
    pub after_diff: String,
    pub diff_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSetStatus {
    PendingReview,
    Accepted,
    Undone,
    FailedToUndo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedFileSummary {
    pub path: String,
    pub change_type: String,
    pub additions: Option<usize>,
    pub deletions: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandCheckSummary {
    pub command: String,
    pub status: String,
    pub exit_code: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ChangeSetDiffResponse {
    pub run_id: String,
    pub change_set_id: String,
    pub diff: String,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct ChangeSetActionResponse {
    pub run_id: String,
    pub change_set: ChangeSet,
    pub status: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct RunListResponse {
    pub runs: Vec<StoredRunSummary>,
}

#[derive(Debug, Serialize)]
pub struct RunDetailResponse {
    pub run_id: String,
    pub metadata: Option<RunState>,
    pub events: Vec<coder_events::CoderEvent>,
    pub report: Option<FinalReport>,
    pub repo_evidence_count: usize,
}

#[derive(Debug, Serialize)]
pub struct RunReportResponse {
    pub run_id: String,
    pub report_ref: Option<String>,
    pub report: FinalReport,
}

#[derive(Debug, Serialize)]
pub struct RunControlResponse {
    pub run_id: String,
    pub status: RunStatus,
    pub control_state: String,
    pub event_count: usize,
    pub report_ref: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunHeartbeatResponse {
    pub run_id: String,
    pub status: Option<RunStatus>,
    pub event_count: usize,
    pub has_report: bool,
    pub repo_evidence_count: usize,
}

#[derive(Debug, Serialize)]
pub struct RepoEvidenceResponse {
    pub ref_id: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RunRepoEvidenceResponse {
    pub run_id: String,
    pub evidence: Vec<RepoEvidenceRef>,
}

#[derive(Debug, Serialize)]
pub struct RunArtifactResponse {
    pub run_id: String,
    pub artifact_name: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RunCheckpointListResponse {
    pub run_id: String,
    pub checkpoints: Vec<RunCheckpointRef>,
}

#[derive(Debug, Serialize)]
pub struct RunCheckpointResponse {
    pub run_id: String,
    pub checkpoint_name: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RunCheckpointWriteResponse {
    pub run_id: String,
    pub checkpoint_name: String,
    pub checkpoint_ref: String,
}

#[derive(Debug, Serialize)]
pub struct RunPreviewResponse {
    pub status: &'static str,
    pub requires_confirmation: bool,
    pub workflow_id: String,
    pub task: String,
    pub backends: Vec<String>,
    pub issues: Vec<ValidationIssue>,
}

fn validation_issue(
    level: ValidationLevel,
    code: impl Into<String>,
    message: impl Into<String>,
    target: impl Into<String>,
) -> ValidationIssue {
    ValidationIssue {
        level,
        code: code.into(),
        message: message.into(),
        target: target.into(),
    }
}

#[derive(Debug, Clone, Copy)]
enum RunControlAction {
    Pause,
    Resume,
    Cancel,
}

fn stored_run_exists(store: &RunStore, run_id: &RunId) -> Result<bool, StoreError> {
    Ok(store.read_metadata(run_id)?.is_some()
        || !store.read_events(run_id)?.is_empty()
        || store.read_report(run_id)?.is_some()
        || store.repo_evidence_count(run_id)? > 0
        || !store.list_checkpoints(run_id)?.is_empty())
}

fn append_mcp_event(
    store: &RunStore,
    run_id: &RunId,
    kind: &str,
    payload: Value,
    evidence_ref: Option<&str>,
) -> Result<(), StoreError> {
    let sequence = store.read_events(run_id)?.len() as u64 + 1;
    let mut event = coder_events::CoderEvent::new(run_id.clone(), sequence, kind, payload);
    if let Some(reference) = evidence_ref {
        event = event.with_ref("mcp_evidence", reference);
    }
    store.append_event(run_id, &event)
}

fn attach_mcp_evidence(store: &RunStore, result: &mut McpToolCallResult) -> Result<(), StoreError> {
    if result.evidence_ref.is_some() {
        return Ok(());
    }
    let output = serde_json::to_string(&result.output).unwrap_or_else(|_| "{}".to_owned());
    let evidence_ref = store.write_blob(output.as_bytes())?;
    result.evidence_ref = Some(evidence_ref);
    Ok(())
}

fn externalize_large_mcp_output(
    store: &RunStore,
    result: &mut McpToolCallResult,
) -> Result<(), StoreError> {
    let output = serde_json::to_string(&result.output).unwrap_or_else(|_| "{}".to_owned());
    if output.len() <= MCP_OUTPUT_INLINE_LIMIT {
        return Ok(());
    }
    let large_ref = store.write_large_text_ref_with_limit(&output, MCP_OUTPUT_INLINE_LIMIT)?;
    result.evidence_ref = Some(large_ref.blob_ref.clone());
    result.output = json!({
        "preview": large_ref.preview,
        "truncated": large_ref.truncated,
        "blob_ref": large_ref.blob_ref
    });
    Ok(())
}

fn json_object_keys(value: &Value) -> Vec<String> {
    match value {
        Value::Object(object) => object.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

impl InstalledSkillRecord {
    fn from_remote(entry: &RemoteSkillEntry, enabled: bool, source_url: Option<String>) -> Self {
        Self {
            summary: remote_skill_summary(entry, enabled),
            source_url,
            pinned_version: None,
            update_policy: "manual".to_owned(),
            history: Vec::new(),
        }
    }
}

fn installed_skill_summaries(state: &ApiState) -> Vec<SkillSummary> {
    state
        .installed_skills
        .lock()
        .unwrap()
        .values()
        .map(|record| record.summary.clone())
        .collect()
}

fn available_skill(skill_id: &str) -> Option<RemoteSkillEntry> {
    builtin_remote_skill_entries()
        .into_iter()
        .find(|entry| entry.id == skill_id)
}

fn skill_update_info(record: &InstalledSkillRecord) -> SkillUpdateInfo {
    let available = available_skill(&record.summary.id);
    let available_version = available.as_ref().map(|entry| entry.version.clone());
    let update_available = available_version
        .as_deref()
        .map(|version| version != record.summary.version)
        .unwrap_or(false);
    SkillUpdateInfo {
        skill_id: record.summary.id.clone(),
        installed_version: record.summary.version.clone(),
        available_version,
        update_available,
        auto_update_eligible: auto_update_allowed(record),
        pinned_version: record.pinned_version.clone(),
        update_policy: record.update_policy.clone(),
        reason: if available.is_some() {
            None
        } else {
            Some("not listed in Rust v3 builtin registry".to_owned())
        },
        risk_level: record.summary.risk_level,
        trust_level: record.summary.trust_level,
        external_effect: record.summary.external_effect,
    }
}

fn auto_update_allowed(record: &InstalledSkillRecord) -> bool {
    record.summary.trust_level == coder_extensions::SkillTrustLevel::Official
        && record.summary.risk_level == coder_extensions::SkillRiskLevel::Low
        && !record.summary.external_effect
        && record.pinned_version.is_none()
}

fn project_timeline_items(
    run_id: &RunId,
    events: &[coder_events::CoderEvent],
    report: Option<&FinalReport>,
) -> Vec<TimelineItem> {
    let mut items = Vec::new();
    for event in events {
        let created_at = event.timestamp.to_string();
        match event.kind.as_str() {
            "run.started" => {
                let task = payload_string(&event.payload, "task")
                    .unwrap_or_else(|| "Workflow started.".to_owned());
                items.push(TimelineItem::PlanUpdate(PlanUpdateItem {
                    id: timeline_id(event, "plan"),
                    agent_id: "planner".to_owned(),
                    title: "Work started".to_owned(),
                    summary: public_preview(&task, 800),
                    created_at,
                }));
            }
            "planner.message.completed" => {
                let summary = payload_string(&event.payload, "summary")
                    .or_else(|| payload_string(&event.payload, "message"))
                    .unwrap_or_else(|| "Planner updated the run.".to_owned());
                items.push(TimelineItem::PlannerMessage(MessageTimelineItem {
                    id: timeline_id(event, "planner-message"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .unwrap_or_else(|| "planner".to_owned()),
                    content: public_preview(&summary, 800),
                    created_at,
                }));
            }
            "planner.plan.updated" => {
                items.push(TimelineItem::PlanUpdate(PlanUpdateItem {
                    id: timeline_id(event, "plan-update"),
                    agent_id: "planner".to_owned(),
                    title: "Plan updated".to_owned(),
                    summary: event
                        .payload
                        .get("acceptance_criteria")
                        .and_then(|value| value.as_array())
                        .map(|items| format!("{} acceptance criteria", items.len()))
                        .unwrap_or_else(|| "Planner updated execution context.".to_owned()),
                    created_at,
                }));
            }
            "planner.readiness.changed" | "reasoning.summary" => {
                let summary = payload_string(&event.payload, "summary")
                    .or_else(|| payload_string(&event.payload, "readiness"))
                    .unwrap_or_else(|| "Planner checked readiness.".to_owned());
                items.push(TimelineItem::ReasoningSummary(ReasoningSummaryItem {
                    id: timeline_id(event, "reasoning"),
                    agent_id: "planner".to_owned(),
                    summary_text: vec![public_preview(&summary, 500)],
                    created_at,
                }));
            }
            "executor.reasoning_summary" => {
                let summary = payload_string(&event.payload, "summary")
                    .unwrap_or_else(|| "Executor summarized its next step.".to_owned());
                items.push(TimelineItem::ReasoningSummary(ReasoningSummaryItem {
                    id: timeline_id(event, "reasoning"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .unwrap_or_else(|| "executor".to_owned()),
                    summary_text: vec![public_preview(&summary, 500)],
                    created_at,
                }));
            }
            "executor.action_selected"
            | "executor.next_step"
            | "executor.completed"
            | "executor.blocked"
            | "executor.failed" => {
                let title = match event.kind.as_str() {
                    "executor.action_selected" => "Action selected",
                    "executor.next_step" => "Next step",
                    "executor.completed" => "Executor completed",
                    "executor.blocked" => "Executor blocked",
                    "executor.failed" => "Executor failed",
                    _ => "Executor update",
                };
                items.push(TimelineItem::ExecutorStep(ExecutorStepItem {
                    id: timeline_id(event, "executor"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .unwrap_or_else(|| "executor".to_owned()),
                    title: title.to_owned(),
                    status: timeline_status(event),
                    summary: executor_event_summary(&event.payload)
                        .map(|value| public_preview(&value, 500)),
                    created_at,
                }));
            }
            "observation.recorded" => {
                let summary = payload_string(&event.payload, "summary")
                    .unwrap_or_else(|| "Observation recorded.".to_owned());
                items.push(TimelineItem::ExecutorStep(ExecutorStepItem {
                    id: timeline_id(event, "observation"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .unwrap_or_else(|| "executor".to_owned()),
                    title: "Observation recorded".to_owned(),
                    status: timeline_status(event),
                    summary: Some(public_preview(&summary, 500)),
                    created_at,
                }));
            }
            "node.started" | "node.completed" | "agent.called" | "agent.completed"
            | "workflow.started" | "round.started" | "run.completed" | "run.failed"
            | "run.blocked" | "run.cancelled" => {
                items.push(TimelineItem::ExecutorStep(ExecutorStepItem {
                    id: timeline_id(event, "step"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .or_else(|| payload_string(&event.payload, "node_id"))
                        .unwrap_or_else(|| "executor".to_owned()),
                    title: event.kind.replace('.', " "),
                    status: status_from_event_kind(&event.kind),
                    summary: payload_string(&event.payload, "summary")
                        .or_else(|| payload_string(&event.payload, "message"))
                        .map(|value| public_preview(&value, 500)),
                    created_at,
                }));
            }
            "tool.started" | "tool.completed" | "tool.failed" | "tool.called" | "tool.result"
            | "mcp.tool.called" | "mcp.tool.completed" => {
                items.push(TimelineItem::ToolCall(ToolCallItem {
                    id: timeline_id(event, "tool"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .unwrap_or_else(|| "executor".to_owned()),
                    tool_name: payload_string(&event.payload, "tool_name")
                        .or_else(|| payload_string(&event.payload, "tool"))
                        .or_else(|| payload_string(&event.payload, "node_id"))
                        .unwrap_or_else(|| "tool".to_owned()),
                    status: timeline_status(event),
                    summary: payload_string(&event.payload, "result_summary")
                        .or_else(|| payload_string(&event.payload, "summary"))
                        .map(|value| public_preview(&value, 500)),
                    evidence_ref: first_event_ref(event),
                    created_at,
                }));
            }
            "command.previewed" | "command.completed" | "command.failed" => {
                items.push(TimelineItem::CommandExecution(CommandExecutionItem {
                    id: timeline_id(event, "command"),
                    agent_id: "executor".to_owned(),
                    command: command_from_payload(&event.payload),
                    cwd: payload_string(&event.payload, "cwd").unwrap_or_else(|| ".".to_owned()),
                    status: status_from_event_kind(&event.kind),
                    stdout_preview: payload_string(&event.payload, "stdout_preview")
                        .or_else(|| payload_string(&event.payload, "output"))
                        .map(|value| public_preview(&value, 1000)),
                    stderr_preview: payload_string(&event.payload, "stderr_preview")
                        .map(|value| public_preview(&value, 1000)),
                    exit_code: payload_i64(&event.payload, "returncode")
                        .or_else(|| payload_i64(&event.payload, "exit_code")),
                    duration_ms: payload_u64(&event.payload, "duration_ms"),
                    evidence_ref: first_event_ref(event),
                    created_at,
                }));
            }
            "patch.previewed" | "patch.applied" | "patch.failed" => {
                let files = changed_files_from_payload(&event.payload);
                if files.is_empty() {
                    items.push(TimelineItem::FileChange(FileChangeItem {
                        id: timeline_id(event, "file"),
                        agent_id: "executor".to_owned(),
                        path: payload_string(&event.payload, "patch_file")
                            .unwrap_or_else(|| "patch".to_owned()),
                        change_type: status_from_event_kind(&event.kind),
                        diff_ref: first_event_ref(event),
                        created_at,
                    }));
                } else {
                    for (index, file) in files.into_iter().enumerate() {
                        items.push(TimelineItem::FileChange(FileChangeItem {
                            id: format!("{}-{index}", timeline_id(event, "file")),
                            agent_id: "executor".to_owned(),
                            path: file.path,
                            change_type: file.change_type,
                            diff_ref: first_event_ref(event),
                            created_at: created_at.clone(),
                        }));
                    }
                }
            }
            "approval.requested" | "approval.required" | "approval.recorded" => {
                items.push(TimelineItem::Approval(ApprovalItem {
                    id: timeline_id(event, "approval"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .unwrap_or_else(|| "executor".to_owned()),
                    risk_level: payload_string(&event.payload, "risk_level")
                        .or_else(|| payload_string(&event.payload, "risk"))
                        .unwrap_or_else(|| "medium".to_owned()),
                    action_type: payload_string(&event.payload, "action_type")
                        .or_else(|| payload_string(&event.payload, "approval_type"))
                        .unwrap_or_else(|| "approval".to_owned()),
                    summary: payload_string(&event.payload, "summary")
                        .or_else(|| payload_string(&event.payload, "reason"))
                        .unwrap_or_else(|| "Approval requested.".to_owned()),
                    status: status_from_event_kind(&event.kind),
                    created_at,
                }));
            }
            "verification.started" | "verification.completed" | "verification.failed" => {
                items.push(TimelineItem::Verification(VerificationItem {
                    id: timeline_id(event, "verification"),
                    agent_id: "executor".to_owned(),
                    status: status_from_event_kind(&event.kind),
                    summary: payload_string(&event.payload, "summary")
                        .or_else(|| payload_string(&event.payload, "command"))
                        .unwrap_or_else(|| "Verification step.".to_owned()),
                    evidence_ref: first_event_ref(event),
                    created_at,
                }));
            }
            _ => {}
        }
    }
    if let Some(report) = report {
        items.push(TimelineItem::FinalSummary(FinalSummaryItem {
            id: format!("timeline-final-{}", run_id.as_str()),
            agent_id: "planner".to_owned(),
            summary: public_preview(&report.summary, 1200),
            changed_files: report.changed_files.clone(),
            checks: report.checks.clone(),
            evidence_refs: report.evidence_refs.clone(),
            created_at: events
                .last()
                .map(|event| event.timestamp.to_string())
                .unwrap_or_default(),
        }));
    }
    items
}

fn build_current_change_set(
    store: &RunStore,
    run_id: &RunId,
) -> Result<Option<ChangeSet>, ApiError> {
    let events = store.read_events(run_id)?;
    let report = store.read_report(run_id)?.unwrap_or_else(|| {
        store
            .build_evidence_report(run_id)
            .unwrap_or_else(|_| FinalReport::completed("No report available."))
    });
    let Some(repo_root) = repo_root_from_events(&events) else {
        return Ok(None);
    };
    let diff = git_diff(&repo_root, 1024 * 1024)?;
    if diff.preview.trim().is_empty() {
        return Ok(None);
    }
    let change_set_id = "changeset-current".to_owned();
    let changed_files = if !report.changed_files.is_empty() {
        report
            .changed_files
            .iter()
            .map(|path| ChangedFileSummary {
                path: path.clone(),
                change_type: "modified".to_owned(),
                additions: None,
                deletions: None,
            })
            .collect()
    } else {
        changed_files_from_diff(&diff.preview)
    };
    let command_checks = report
        .checks
        .iter()
        .filter(|check| !check.starts_with("plan_context:") && !check.starts_with("acceptance:"))
        .map(|check| CommandCheckSummary {
            command: check.clone(),
            status: if check.contains("failed") {
                "failed".to_owned()
            } else {
                "completed".to_owned()
            },
            exit_code: None,
        })
        .collect();
    let before_checkpoint_ref = store
        .list_checkpoints(run_id)?
        .into_iter()
        .find(|checkpoint| checkpoint.name == "before-run.json")
        .map(|checkpoint| checkpoint.checkpoint_ref);
    let after_diff_ref = format!(
        "artifact://runs/{}/artifacts/{}.json",
        run_id.as_str(),
        change_set_id
    );
    let change_set = ChangeSet {
        change_set_id,
        run_id: run_id.to_string(),
        repo_root,
        status: ChangeSetStatus::PendingReview,
        created_at: now_timestamp_string(),
        base_git_head: run_started_payload_string(&events, "git_head"),
        before_checkpoint_ref,
        after_diff_ref: Some(after_diff_ref.clone()),
        reverse_patch_ref: Some(format!("{after_diff_ref}#reverse-git-apply")),
        changed_files,
        command_checks,
        evidence_refs: report.evidence_refs,
        after_diff: diff.preview,
        diff_truncated: diff.truncated,
    };
    write_change_set(store, run_id, &change_set)?;
    Ok(Some(change_set))
}

fn current_change_set(store: &RunStore, run_id: &RunId) -> Result<Option<ChangeSet>, ApiError> {
    let Some(stored) = read_stored_change_set(store, run_id, "changeset-current")? else {
        return build_current_change_set(store, run_id);
    };
    let current_diff = git_diff(&stored.repo_root, 1024 * 1024)?.preview;
    if current_diff.trim().is_empty() {
        return Ok(None);
    }
    if current_diff == stored.after_diff || stored.status != ChangeSetStatus::PendingReview {
        return Ok(Some(stored));
    }
    build_current_change_set(store, run_id)
}

fn read_stored_change_set(
    store: &RunStore,
    run_id: &RunId,
    change_set_id: &str,
) -> Result<Option<ChangeSet>, ApiError> {
    match store.read_artifact_json(run_id, &change_set_artifact_name(change_set_id)) {
        Ok(value) => Ok(Some(serde_json::from_value(value).map_err(|error| {
            ApiError::internal(format!("stored change set is invalid: {error}"))
        })?)),
        Err(StoreError::ArtifactNotFound { .. }) => Ok(None),
        Err(error) => Err(ApiError::from(error)),
    }
}

fn read_change_set(
    store: &RunStore,
    run_id: &RunId,
    change_set_id: &str,
) -> Result<ChangeSet, ApiError> {
    read_stored_change_set(store, run_id, change_set_id)?.map_or_else(
        || {
            build_current_change_set(store, run_id)?
                .filter(|change_set| change_set.change_set_id == change_set_id)
                .ok_or_else(|| {
                    ApiError::not_found(format!("change set '{change_set_id}' was not found"))
                })
        },
        Ok,
    )
}

fn write_change_set(
    store: &RunStore,
    run_id: &RunId,
    change_set: &ChangeSet,
) -> Result<String, ApiError> {
    Ok(store.write_artifact(
        run_id,
        &change_set_artifact_name(&change_set.change_set_id),
        change_set,
    )?)
}

fn append_change_set_event(
    store: &RunStore,
    run_id: &RunId,
    kind: &str,
    change_set_id: &str,
    mut payload: Value,
) -> Result<(), ApiError> {
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "change_set_id".to_owned(),
            Value::String(change_set_id.to_owned()),
        );
    }
    let sequence = store.read_events(run_id)?.len() as u64 + 1;
    store.append_event(
        run_id,
        &coder_events::CoderEvent::new(run_id.clone(), sequence, kind, payload),
    )?;
    Ok(())
}

fn apply_reverse_diff(repo_root: &str, diff: &str) -> Result<(), ApiError> {
    if diff.trim().is_empty() {
        return Ok(());
    }
    let root = fs::canonicalize(repo_root).map_err(|error| {
        ApiError::bad_request(format!("repo root '{repo_root}' is invalid: {error}"))
    })?;
    if !root.is_dir() {
        return Err(ApiError::bad_request(format!(
            "repo root '{}' is not a directory",
            root.display()
        )));
    }
    run_git_apply_reverse(&root, diff, true)?;
    run_git_apply_reverse(&root, diff, false)
}

fn run_git_apply_reverse(root: &FsPath, diff: &str, check: bool) -> Result<(), ApiError> {
    let mut command = Command::new("git");
    command.arg("apply").arg("-R");
    if check {
        command.arg("--check");
    }
    let mut child = command
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| ApiError::internal(format!("failed to run git apply: {error}")))?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| ApiError::internal("failed to open git apply stdin"))?
        .write_all(diff.as_bytes())
        .map_err(|error| ApiError::internal(format!("failed to write reverse patch: {error}")))?;
    let output = child
        .wait_with_output()
        .map_err(|error| ApiError::internal(format!("failed to wait for git apply: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(ApiError::conflict(format!(
        "reverse patch did not apply cleanly: {}",
        public_preview(&stderr, 1000)
    )))
}

fn change_set_artifact_name(change_set_id: &str) -> String {
    format!("{change_set_id}.json")
}

fn repo_root_from_events(events: &[coder_events::CoderEvent]) -> Option<String> {
    events
        .iter()
        .find(|event| event.kind == "run.started")
        .and_then(|event| payload_string(&event.payload, "repo_root"))
}

fn run_started_payload_string(events: &[coder_events::CoderEvent], key: &str) -> Option<String> {
    events
        .iter()
        .find(|event| event.kind == "run.started")
        .and_then(|event| payload_string(&event.payload, key))
}

fn changed_files_from_payload(payload: &Value) -> Vec<ChangedFileSummary> {
    payload
        .get("files")
        .and_then(|value| value.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|file| {
                    let path = payload_string(file, "new_path")
                        .or_else(|| payload_string(file, "path"))
                        .or_else(|| payload_string(file, "old_path"))?;
                    Some(ChangedFileSummary {
                        path,
                        change_type: payload_string(file, "status")
                            .or_else(|| payload_string(file, "action"))
                            .unwrap_or_else(|| "modified".to_owned()),
                        additions: payload_u64(file, "additions").map(|value| value as usize),
                        deletions: payload_u64(file, "deletions").map(|value| value as usize),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn changed_files_from_diff(diff: &str) -> Vec<ChangedFileSummary> {
    let mut files = Vec::new();
    for line in diff.lines() {
        let Some(path) = line.strip_prefix("+++ b/") else {
            continue;
        };
        if path == "/dev/null" {
            continue;
        }
        files.push(ChangedFileSummary {
            path: path.to_owned(),
            change_type: "modified".to_owned(),
            additions: None,
            deletions: None,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files.dedup_by(|left, right| left.path == right.path);
    files
}

fn command_from_payload(payload: &Value) -> Vec<String> {
    payload
        .get("argv")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| {
            payload_string(payload, "command")
                .map(|command| vec![command])
                .unwrap_or_else(|| vec!["command".to_owned()])
        })
}

fn payload_string(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(|value| match value {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    })
}

fn payload_i64(payload: &Value, key: &str) -> Option<i64> {
    payload.get(key).and_then(Value::as_i64)
}

fn payload_u64(payload: &Value, key: &str) -> Option<u64> {
    payload.get(key).and_then(Value::as_u64)
}

fn timeline_id(event: &coder_events::CoderEvent, suffix: &str) -> String {
    format!("{}-{suffix}", event.event_id)
}

fn first_event_ref(event: &coder_events::CoderEvent) -> Option<String> {
    event.refs.first().map(|reference| reference.uri.clone())
}

fn timeline_status(event: &coder_events::CoderEvent) -> String {
    payload_string(&event.payload, "status").unwrap_or_else(|| status_from_event_kind(&event.kind))
}

fn executor_event_summary(payload: &Value) -> Option<String> {
    payload_string(payload, "summary")
        .or_else(|| {
            let tool_name = payload_string(payload, "tool_name")?;
            Some(format!("Selected {tool_name}."))
        })
        .or_else(|| payload_string(payload, "based_on_observation"))
}

fn status_from_event_kind(kind: &str) -> String {
    if kind.ends_with(".failed") || kind == "run.failed" {
        "failed".to_owned()
    } else if kind.ends_with(".blocked") || kind == "run.blocked" {
        "blocked".to_owned()
    } else if kind.ends_with(".completed") || kind == "run.completed" {
        "completed".to_owned()
    } else if kind.ends_with(".started") {
        "running".to_owned()
    } else if kind.ends_with(".requested") || kind.ends_with(".required") {
        "pending".to_owned()
    } else if kind.ends_with(".applied") {
        "applied".to_owned()
    } else if kind.ends_with(".previewed") {
        "previewed".to_owned()
    } else {
        "noted".to_owned()
    }
}

fn public_preview(text: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for ch in text.chars().take(max_chars) {
        output.push(ch);
    }
    if text.chars().count() > max_chars {
        output.push_str("...");
    }
    redact_secret_markers(&output)
}

fn redact_secret_markers(text: &str) -> String {
    let mut redacted = text.to_owned();
    for marker in [
        "api_key",
        "apikey",
        "authorization",
        "cookie",
        "password",
        "private_key",
        "secret",
        "token",
    ] {
        if redacted.to_ascii_lowercase().contains(marker) {
            redacted = "[redacted sensitive output]".to_owned();
            break;
        }
    }
    redacted
}

fn builtin_hooks() -> Vec<HookSummary> {
    vec![
        HookSummary {
            id: "approval.guardian".to_owned(),
            trigger: "approval.requested".to_owned(),
            enabled: true,
            description: "Routes risky executor actions through human approval.".to_owned(),
        },
        HookSummary {
            id: "final-summary".to_owned(),
            trigger: "run.finalizing".to_owned(),
            enabled: true,
            description: "Builds the evidence-backed final summary.".to_owned(),
        },
    ]
}

fn now_timestamp_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| format!("unix:{}", duration.as_secs()))
        .unwrap_or_else(|_| "unix:0".to_owned())
}

fn store_dir_size(path: &FsPath) -> u64 {
    let Ok(metadata) = fs::metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }
    fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| store_dir_size(&entry.path()))
                .sum()
        })
        .unwrap_or(0)
}

fn set_skill_enabled(
    state: ApiState,
    skill_id: String,
    enabled: bool,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let mut installed = state.installed_skills.lock().unwrap();
    let record = installed
        .get_mut(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    record.summary.enabled = enabled;
    Ok(Json(SkillActionResponse {
        skill_id,
        status: if enabled { "enabled" } else { "disabled" }.to_owned(),
        skill: Some(record.summary.clone()),
        deleted: false,
        updated: Vec::new(),
    }))
}

fn apply_provider_settings_patch(settings: &mut ProviderSettings, patch: ProviderSettingsPatch) {
    if let Some(provider) = patch.default_provider {
        let provider = normalize_provider(&provider);
        if !provider.is_empty() {
            settings.default_provider = provider;
        }
    }
    if let Some(model) = patch.default_model {
        let model = model.trim();
        if !model.is_empty() {
            settings.default_model = model.to_owned();
        }
    }
    if let Some(mock_mode) = patch.mock_mode {
        settings.mock_mode = mock_mode;
    }
    if let Some(base_urls) = patch.base_urls {
        settings.base_urls = clean_provider_string_map(base_urls);
    }
    if let Some(api_keys) = patch.api_keys {
        for (provider, value) in api_keys {
            let provider = normalize_provider(&provider);
            if provider.is_empty() {
                continue;
            }
            if value.is_null() {
                settings.api_keys.remove(&provider);
                continue;
            }
            let text = value.as_str().map(str::trim).unwrap_or_default();
            if text.is_empty() || text.chars().all(|ch| ch == '*') {
                continue;
            }
            settings.api_keys.insert(
                provider,
                ProviderKeyState {
                    configured: true,
                    source: "settings".to_owned(),
                    secret: Some(text.to_owned()),
                },
            );
        }
    }
}

fn provider_status(settings: &ProviderSettings, providers: Option<Vec<String>>) -> ProviderStatus {
    let selected = providers.unwrap_or_else(|| {
        let mut names = provider_env_keys().keys().cloned().collect::<BTreeSet<_>>();
        names.insert(settings.default_provider.clone());
        names.extend(settings.api_keys.keys().cloned());
        names.into_iter().collect()
    });
    let providers = selected
        .into_iter()
        .map(|provider| provider_status_item(settings, &normalize_provider(&provider)))
        .collect::<Vec<_>>();
    ProviderStatus {
        default_provider: settings.default_provider.clone(),
        default_model: settings.default_model.clone(),
        mock_mode: settings.mock_mode,
        default_status: provider_status_item(settings, &settings.default_provider),
        providers,
    }
}

fn provider_status_item(settings: &ProviderSettings, provider: &str) -> ProviderStatusItem {
    let provider = if provider.trim().is_empty() {
        "openai"
    } else {
        provider.trim()
    };
    let (credential_configured, credential_source) = provider_credential_state(settings, provider);
    let configured = provider == "ollama" || credential_configured || settings.mock_mode;
    ProviderStatusItem {
        provider: provider.to_owned(),
        configured,
        credential_configured: provider == "ollama" || credential_configured,
        credential_source: if provider == "ollama" {
            "ollama".to_owned()
        } else {
            credential_source
        },
        base_url: provider_base_url(settings, provider),
        mode: if settings.mock_mode && !credential_configured && provider != "ollama" {
            "mock"
        } else {
            "live"
        }
        .to_owned(),
    }
}

fn provider_credential_state(settings: &ProviderSettings, provider: &str) -> (bool, String) {
    if settings
        .api_keys
        .get(provider)
        .map(|state| state.configured && !state.secret.as_deref().unwrap_or("").trim().is_empty())
        .unwrap_or(false)
    {
        return (true, "settings".to_owned());
    }
    if provider_api_key_from_env(provider, None).is_some() {
        return (true, "environment".to_owned());
    }
    (false, "missing".to_owned())
}

fn provider_base_url(settings: &ProviderSettings, provider: &str) -> Option<String> {
    if let Some(value) = settings_provider_base_url(settings, provider) {
        return Some(value);
    }
    provider_base_url_from_env(None)
        .or_else(|| default_provider_base_url(provider).map(str::to_owned))
}

fn settings_provider_base_url(settings: &ProviderSettings, provider: &str) -> Option<String> {
    settings.base_urls.get(provider).cloned()
}

fn provider_base_url_from_env(model_base_url_env: Option<&str>) -> Option<String> {
    let candidates = [
        model_base_url_env,
        Some("CODER_BASE_URL"),
        Some("LLM_BASE_URL"),
    ];
    for env_name in candidates.into_iter().flatten() {
        if let Some(value) = env::var_os(env_name).and_then(|value| value.into_string().ok()) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn provider_api_key(
    settings: &ProviderSettings,
    provider: &str,
    model_api_key_env: Option<&str>,
) -> Option<(String, String)> {
    settings
        .api_keys
        .get(provider)
        .and_then(|state| state.secret.as_deref())
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
        .map(|secret| (secret.to_owned(), "settings".to_owned()))
        .or_else(|| {
            provider_api_key_from_env(provider, model_api_key_env)
                .map(|secret| (secret, "environment".to_owned()))
        })
}

fn provider_api_key_from_env(provider: &str, model_api_key_env: Option<&str>) -> Option<String> {
    let env_keys = provider_env_keys();
    let provider_env_name = env_keys
        .get(provider)
        .map(String::as_str)
        .unwrap_or("CODER_API_KEY");
    let candidates = [
        model_api_key_env,
        Some(provider_env_name),
        Some("CODER_API_KEY"),
        Some("LLM_API_KEY"),
    ];
    let mut seen = BTreeSet::new();
    for env_name in candidates.into_iter().flatten() {
        if !seen.insert(env_name.to_owned()) {
            continue;
        }
        if let Some(value) = env::var_os(env_name).and_then(|value| value.into_string().ok()) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn provider_env_keys() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("openai".to_owned(), "OPENAI_API_KEY".to_owned()),
        ("openai-compatible".to_owned(), "CODER_API_KEY".to_owned()),
        ("deepseek".to_owned(), "DEEPSEEK_API_KEY".to_owned()),
        ("moonshot".to_owned(), "MOONSHOT_API_KEY".to_owned()),
        ("kimi".to_owned(), "MOONSHOT_API_KEY".to_owned()),
        ("qwen".to_owned(), "DASHSCOPE_API_KEY".to_owned()),
        ("dashscope".to_owned(), "DASHSCOPE_API_KEY".to_owned()),
        ("groq".to_owned(), "GROQ_API_KEY".to_owned()),
        ("openrouter".to_owned(), "OPENROUTER_API_KEY".to_owned()),
        ("together".to_owned(), "TOGETHER_API_KEY".to_owned()),
        ("mistral".to_owned(), "MISTRAL_API_KEY".to_owned()),
        ("perplexity".to_owned(), "PERPLEXITY_API_KEY".to_owned()),
        ("xai".to_owned(), "XAI_API_KEY".to_owned()),
        ("gemini".to_owned(), "GEMINI_API_KEY".to_owned()),
        ("ollama".to_owned(), "OLLAMA_API_KEY".to_owned()),
    ])
}

fn default_provider_base_url(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some("https://api.openai.com/v1"),
        "deepseek" => Some("https://api.deepseek.com"),
        "moonshot" | "kimi" => Some("https://api.moonshot.cn/v1"),
        "qwen" | "dashscope" => Some("https://dashscope.aliyuncs.com/compatible-mode/v1"),
        "groq" => Some("https://api.groq.com/openai/v1"),
        "openrouter" => Some("https://openrouter.ai/api/v1"),
        "together" => Some("https://api.together.xyz/v1"),
        "mistral" => Some("https://api.mistral.ai/v1"),
        "perplexity" => Some("https://api.perplexity.ai"),
        "xai" => Some("https://api.x.ai/v1"),
        "gemini" => Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        "ollama" => Some("http://localhost:11434/v1"),
        _ => None,
    }
}

fn normalize_provider(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn provider_http_client_builder(url: &str) -> reqwest::ClientBuilder {
    let builder = Client::builder();
    if url.contains("://127.0.0.1") || url.contains("://localhost") || url.contains("://[::1]") {
        builder.no_proxy()
    } else {
        builder
    }
}

async fn test_provider_chat_completion(
    settings: &ProviderSettings,
    provider: &str,
    mock: bool,
) -> Result<ProviderTestResult, String> {
    let provider = normalize_provider(provider);
    let model = settings.default_model.clone();
    if mock {
        return Ok(ProviderTestResult {
            provider,
            ok: true,
            mode: "mock".to_owned(),
            model,
            endpoint: None,
            message: "Mock provider test passed without a live request.".to_owned(),
        });
    }
    let status = provider_status_item(settings, &provider);
    if settings.mock_mode && !status.credential_configured {
        return Ok(ProviderTestResult {
            provider,
            ok: true,
            mode: "mock".to_owned(),
            model,
            endpoint: None,
            message: "Mock mode is enabled; no live provider request was sent.".to_owned(),
        });
    }
    let (api_key, source) = provider_api_key(settings, &provider, None).ok_or_else(|| {
        "Provider test requires an API key from Provider Settings or developer/headless environment fallback."
            .to_owned()
    })?;
    let base_url = provider_base_url(settings, &provider)
        .ok_or_else(|| "Provider test requires a base URL.".to_owned())?;
    let url = provider_chat_completions_endpoint(&base_url);
    let endpoint = provider_chat_completions_endpoint_for_display(&base_url);
    let client = provider_http_client_builder(&url)
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .post(&url)
        .bearer_auth(&api_key)
        .json(&json!({
            "model": &settings.default_model,
            "messages": [
                {"role": "user", "content": "Reply with OK."}
            ],
            "temperature": 0,
            "max_tokens": 8
        }))
        .send()
        .await
        .map_err(|error| format!("Provider test request failed: {}", error))?;
    if !response.status().is_success() {
        return Ok(ProviderTestResult {
            provider,
            ok: false,
            mode: "live".to_owned(),
            model,
            endpoint: Some(endpoint),
            message: format!("Provider returned HTTP {}.", response.status()),
        });
    }
    let payload: Value = response.json().await.map_err(|error| error.to_string())?;
    let content = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if content.is_empty() {
        return Ok(ProviderTestResult {
            provider,
            ok: false,
            mode: "live".to_owned(),
            model,
            endpoint: Some(endpoint),
            message: "Provider response did not include assistant content.".to_owned(),
        });
    }
    Ok(ProviderTestResult {
        provider,
        ok: true,
        mode: "live".to_owned(),
        model,
        endpoint: Some(endpoint),
        message: format!("Live provider test succeeded using {source} credentials."),
    })
}

fn provider_chat_completions_endpoint(base_url: &str) -> String {
    let base_url = base_url.trim();
    if let Ok(mut url) = reqwest::Url::parse(base_url) {
        url.set_query(None);
        url.set_fragment(None);
        let path = format!("{}/chat/completions", url.path().trim_end_matches('/'));
        url.set_path(&path);
        return url.to_string();
    }
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

fn provider_chat_completions_endpoint_for_display(base_url: &str) -> String {
    sanitize_provider_endpoint(&provider_chat_completions_endpoint(base_url))
}

fn sanitize_provider_endpoint(endpoint: &str) -> String {
    if let Ok(mut url) = reqwest::Url::parse(endpoint) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string();
    }
    endpoint
        .split('?')
        .next()
        .unwrap_or(endpoint)
        .split('#')
        .next()
        .unwrap_or(endpoint)
        .to_owned()
}

fn normalize_planner_mode(value: Option<&str>) -> String {
    if value
        .map(|item| item.trim().eq_ignore_ascii_case("work"))
        .unwrap_or(false)
    {
        "work".to_owned()
    } else {
        "discuss".to_owned()
    }
}

#[derive(Debug, Clone, Default)]
struct DeterministicPlannerConversationEngine;

#[async_trait]
impl PlannerConversationEngine for DeterministicPlannerConversationEngine {
    async fn respond(
        &self,
        request: PlannerConversationRequest,
    ) -> Result<PlannerConversationResponse, String> {
        Ok(deterministic_planner_response(&request, None))
    }
}

#[derive(Debug, Clone, Default)]
struct ModelPlannerConversationEngine {
    fallback: DeterministicPlannerConversationEngine,
}

impl ModelPlannerConversationEngine {
    fn new() -> Self {
        Self::default()
    }

    async fn live_assistant_message(
        &self,
        request: &PlannerConversationRequest,
    ) -> Result<Option<String>, String> {
        if request.provider_settings.mock_mode {
            return Ok(None);
        }
        let model = planner_model_profile(request);
        let provider = planner_model_provider(request, model);
        let (api_key, _) = provider_api_key(
            &request.provider_settings,
            &provider,
            model.api_key_env.as_deref(),
        )
        .ok_or_else(planner_model_config_error)?;
        let base_url = planner_model_base_url(request, &provider, model)
            .ok_or_else(planner_model_config_error)?;
        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
        let model_name = planner_model_name(request, model);
        let mut messages = vec![json!({
            "role": "system",
            "content": planner_system_prompt(&request.runtime)
        })];
        for turn in request
            .history
            .iter()
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            let role = if turn.role == "assistant" {
                "assistant"
            } else {
                "user"
            };
            messages.push(json!({
                "role": role,
                "content": &turn.content
            }));
        }
        messages.push(json!({
            "role": "user",
            "content": &request.message
        }));
        let client = provider_http_client_builder(&url)
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|error| error.to_string())?;
        let response = client
            .post(url)
            .bearer_auth(api_key)
            .json(&json!({
                "model": model_name,
                "messages": messages,
                "temperature": 0.2
            }))
            .send()
            .await
            .map_err(|error| format!("planner model request failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("planner model returned HTTP {}", response.status()));
        }
        let payload: Value = response.json().await.map_err(|error| error.to_string())?;
        Ok(payload
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .map(str::to_owned))
    }
}

fn planner_model_profile(request: &PlannerConversationRequest) -> &ConfigModelSpec {
    &request.runtime.model
}

fn planner_model_name(request: &PlannerConversationRequest, model: &ConfigModelSpec) -> String {
    if matches!(model.model.as_str(), "best" | "standard" | "economy") {
        request.provider_settings.default_model.clone()
    } else {
        model.model.clone()
    }
}

fn planner_model_provider(request: &PlannerConversationRequest, model: &ConfigModelSpec) -> String {
    model_provider_for_settings(&request.provider_settings, model)
}

fn planner_model_base_url(
    request: &PlannerConversationRequest,
    provider: &str,
    model: &ConfigModelSpec,
) -> Option<String> {
    model_provider_base_url(&request.provider_settings, provider, model)
}

fn model_provider_for_settings(settings: &ProviderSettings, model: &ConfigModelSpec) -> String {
    if matches!(model.model.as_str(), "best" | "standard" | "economy")
        && !settings.default_provider.trim().is_empty()
    {
        normalize_provider(&settings.default_provider)
    } else {
        normalize_provider(&model.provider)
    }
}

fn model_provider_base_url(
    settings: &ProviderSettings,
    provider: &str,
    model: &ConfigModelSpec,
) -> Option<String> {
    settings_provider_base_url(settings, provider)
        .or_else(|| provider_base_url_from_env(model.base_url_env.as_deref()))
        .or_else(|| default_provider_base_url(provider).map(str::to_owned))
}

fn model_provider_config_error(
    settings: &ProviderSettings,
    model: &ConfigModelSpec,
) -> Option<String> {
    if settings.mock_mode {
        return None;
    }
    let provider = model_provider_for_settings(settings, model);
    if model_provider_base_url(settings, &provider, model).is_none() {
        return Some(planner_model_config_error());
    }
    if provider != "ollama"
        && provider_api_key(settings, &provider, model.api_key_env.as_deref()).is_none()
    {
        return Some(planner_model_config_error());
    }
    None
}

const PLANNER_MODEL_CONFIG_ERROR: &str = "Planner model provider is not configured. Configure Provider Settings, or set LLM_BASE_URL and LLM_API_KEY for developer/headless fallback.";

fn planner_model_config_error() -> String {
    PLANNER_MODEL_CONFIG_ERROR.to_owned()
}

fn is_planner_model_config_error(error: &str) -> bool {
    error == PLANNER_MODEL_CONFIG_ERROR
}

fn planner_system_prompt(runtime: &PlannerRuntimeContext) -> String {
    format!(
        "{}\n\nRuntime boundary:\n- workflow_id: {}\n- workflow_name: {}\n- node_id: {}\n- agent_id: {}\n- harness_id: {}\n- tools: {}\n- side effects: denied\n\nDiscuss, clarify, remember only through explicit proposals, draft concise plans, and never claim execution happened in Discuss mode.",
        runtime.agent.system,
        runtime.workflow_id,
        runtime.workflow_name,
        runtime.node_id,
        runtime.agent_id,
        runtime.harness_id,
        runtime.harness.tools.join(", ")
    )
}

#[async_trait]
impl PlannerConversationEngine for ModelPlannerConversationEngine {
    async fn respond(
        &self,
        request: PlannerConversationRequest,
    ) -> Result<PlannerConversationResponse, String> {
        let model_message = match self.live_assistant_message(&request).await {
            Ok(message) => message,
            Err(error) if is_planner_model_config_error(&error) => {
                return Ok(planner_provider_setup_required_response(error));
            }
            Err(error) => return Err(error),
        };
        if model_message.is_some() {
            return Ok(deterministic_planner_response(&request, model_message));
        }
        self.fallback.respond(request).await
    }
}

fn planner_provider_setup_required_response(message: String) -> PlannerConversationResponse {
    PlannerConversationResponse {
        assistant_message: format!("{message} Open Settings and save a provider API key, then send the Planner message again."),
        plan_draft: None,
        readiness: PlannerReadiness::Blocked,
        open_questions: vec!["Configure a provider API key in Settings.".to_owned()],
        acceptance_criteria: Vec::new(),
        risks: Vec::new(),
        suggested_mode: "discuss".to_owned(),
        should_start_workflow: false,
    }
}

fn deterministic_planner_response(
    request: &PlannerConversationRequest,
    model_message: Option<String>,
) -> PlannerConversationResponse {
    let mode = normalize_planner_mode(Some(&request.mode));
    let work_like = message_looks_like_work(&request.message) || request.current_plan.is_some();
    if mode == "discuss" && !work_like {
        let assistant_message = model_message.unwrap_or_else(|| {
            "I can discuss that. If you want repository work later, I will first turn it into a scoped plan and keep execution behind Start Work.".to_owned()
        });
        return PlannerConversationResponse {
            assistant_message,
            plan_draft: request.current_plan.clone(),
            readiness: PlannerReadiness::Casual,
            open_questions: Vec::new(),
            acceptance_criteria: Vec::new(),
            risks: Vec::new(),
            suggested_mode: "discuss".to_owned(),
            should_start_workflow: false,
        };
    }

    let plan = planner_plan_draft(request);
    let readiness = if plan.open_questions.is_empty() {
        PlannerReadiness::Ready
    } else {
        PlannerReadiness::NeedsClarification
    };
    let assistant_message = model_message.unwrap_or_else(|| {
        deterministic_planner_message(&mode, &plan, readiness, request.confirmed)
    });

    PlannerConversationResponse {
        assistant_message,
        open_questions: plan.open_questions.clone(),
        acceptance_criteria: plan.acceptance_criteria.clone(),
        risks: plan.risks.clone(),
        suggested_mode: if readiness == PlannerReadiness::Ready {
            "work".to_owned()
        } else {
            "discuss".to_owned()
        },
        should_start_workflow: false,
        readiness,
        plan_draft: Some(plan),
    }
}

fn deterministic_planner_message(
    mode: &str,
    plan: &PlanDraft,
    readiness: PlannerReadiness,
    confirmed: bool,
) -> String {
    if readiness == PlannerReadiness::NeedsClarification {
        return format!(
            "I can plan this, but I need clarification before Start Work can run:\n{}",
            numbered_lines(&plan.open_questions)
        );
    }
    if mode == "work" && confirmed {
        return format!(
            "The plan is ready for workflow '{}'. Use Start Work to run it and get evidence against the acceptance criteria.",
            plan.selected_workflow_id
        );
    }
    if mode == "work" {
        return format!(
            "The plan is ready for workflow '{}'. Use Start Work when you want me to execute it. Acceptance criteria:\n{}",
            plan.selected_workflow_id,
            numbered_lines(&plan.acceptance_criteria)
        );
    }
    format!(
        "I have enough information to plan this. Goal: {}\nAcceptance criteria:\n{}\nUse Start Work when you want me to execute it.",
        plan.goal,
        numbered_lines(&plan.acceptance_criteria)
    )
}

fn planner_plan_draft(request: &PlannerConversationRequest) -> PlanDraft {
    let current = request.current_plan.clone();
    let affected_paths = unique_strings(extract_affected_paths(&request.message));
    let acceptance_criteria = {
        let parsed = unique_strings(extract_acceptance_criteria(&request.message));
        if !parsed.is_empty() {
            parsed
        } else {
            current
                .as_ref()
                .map(|plan| plan.acceptance_criteria.clone())
                .filter(|items| !items.is_empty())
                .unwrap_or_else(|| {
                    vec!["The workflow ends with an evidence-backed final report.".to_owned()]
                })
        }
    };
    let mut open_questions = Vec::new();
    if affected_paths.is_empty()
        && current
            .as_ref()
            .map(|plan| plan.affected_paths.is_empty() && plan.scope.is_empty())
            .unwrap_or(true)
        && !message_has_whole_repo_scope(&request.message)
    {
        open_questions
            .push("Which path, module, or repository scope should I focus on?".to_owned());
    }
    if acceptance_criteria.is_empty() {
        open_questions
            .push("Which checks or acceptance criteria should prove completion?".to_owned());
    }
    if request.message.trim().len() < 12 {
        open_questions
            .push("What exact change or investigation should the workflow perform?".to_owned());
    }
    open_questions = unique_strings(open_questions);
    let goal = extract_goal(&request.message)
        .or_else(|| current.as_ref().map(|plan| plan.goal.clone()))
        .unwrap_or_else(|| "Complete the requested repository work.".to_owned());
    let scope = if affected_paths.is_empty() {
        current
            .as_ref()
            .map(|plan| plan.scope.clone())
            .unwrap_or_default()
    } else {
        affected_paths.clone()
    };
    let risks = {
        let parsed = unique_strings(extract_risks(&request.message));
        if !parsed.is_empty() {
            parsed
        } else {
            current
                .as_ref()
                .map(|plan| plan.risks.clone())
                .filter(|items| !items.is_empty())
                .unwrap_or_else(|| {
                    vec!["Behavior may change if the affected scope is too broad.".to_owned()]
                })
        }
    };
    let memory_proposals = {
        let parsed = memory_proposals_for(&request.message);
        if parsed.is_empty() {
            current
                .as_ref()
                .map(|plan| plan.memory_proposals.clone())
                .unwrap_or_default()
        } else {
            parsed
        }
    };
    PlanDraft {
        goal,
        scope,
        non_goals: current
            .as_ref()
            .map(|plan| plan.non_goals.clone())
            .unwrap_or_else(|| vec!["Do not change unrelated product surfaces.".to_owned()]),
        assumptions: current
            .as_ref()
            .map(|plan| plan.assumptions.clone())
            .unwrap_or_else(|| {
                vec![
                    "Normal validation must stay offline.".to_owned(),
                    "Current repo evidence overrides stale memory.".to_owned(),
                ]
            }),
        steps: plan_steps_for(&request.message),
        affected_paths,
        acceptance_criteria,
        risks,
        open_questions,
        selected_workflow_id: request.workflow_id.clone(),
        memory_proposals,
    }
}

fn memory_proposals_for(message: &str) -> Vec<MemoryProposalDraft> {
    let lower = message.to_ascii_lowercase();
    if !(lower.contains("remember")
        || lower.contains("preference")
        || lower.contains("project convention")
        || lower.contains("记住"))
    {
        return Vec::new();
    }
    let content = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(message.trim())
        .trim_matches(|ch: char| ch == '"' || ch == '\'')
        .to_owned();
    if content.is_empty() {
        return Vec::new();
    }
    vec![MemoryProposalDraft {
        scope: "project".to_owned(),
        key: stable_memory_key(&content),
        content,
        rationale: "The user phrased this as a durable preference or project convention."
            .to_owned(),
        requires_confirmation: true,
    }]
}

fn stable_memory_key(content: &str) -> String {
    let key = content
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() {
                Some(ch.to_ascii_lowercase())
            } else if ch.is_whitespace() || matches!(ch, '-' | '_' | '/' | '.') {
                Some('-')
            } else {
                None
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join("-");
    if key.is_empty() {
        "planner-memory-proposal".to_owned()
    } else {
        key
    }
}

fn numbered_lines(items: &[String]) -> String {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| format!("{}. {}", index + 1, item))
        .collect::<Vec<_>>()
        .join("\n")
}

fn message_looks_like_work(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    let work_markers = [
        "add",
        "build",
        "change",
        "check",
        "code",
        "delete",
        "fix",
        "implement",
        "inspect",
        "plan",
        "patch",
        "refactor",
        "repo",
        "run",
        "test",
        "update",
        "work",
        "workflow",
    ];
    work_markers.iter().any(|marker| lower.contains(marker))
        || !extract_affected_paths(message).is_empty()
}

fn message_has_whole_repo_scope(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("whole repo") || lower.contains("entire repo") || lower.contains("project")
}

fn extract_goal(message: &str) -> Option<String> {
    message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| {
            line.trim_matches(|ch: char| ch == '"' || ch == '\'')
                .to_owned()
        })
        .filter(|line| !line.is_empty())
}

fn extract_affected_paths(message: &str) -> Vec<String> {
    message
        .split_whitespace()
        .filter_map(|token| {
            let cleaned = token
                .trim_matches(|ch: char| {
                    matches!(ch, ',' | ';' | ':' | ')' | '(' | '[' | ']' | '"' | '\'')
                })
                .replace('\\', "/");
            let lower = cleaned.to_ascii_lowercase();
            let path_like = cleaned.contains('/')
                || [
                    ".rs", ".tsx", ".ts", ".js", ".jsx", ".md", ".toml", ".yaml", ".yml", ".json",
                    ".css", ".ps1", ".sh",
                ]
                .iter()
                .any(|suffix| lower.ends_with(suffix));
            if path_like && !cleaned.contains("://") {
                Some(cleaned)
            } else {
                None
            }
        })
        .collect()
}

fn extract_acceptance_criteria(message: &str) -> Vec<String> {
    let mut criteria = Vec::new();
    for line in message.lines().map(str::trim) {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower
            .find("acceptance:")
            .or_else(|| lower.find("success criteria:"))
            .and_then(|index| line.get(index..))
        {
            let value = rest
                .split_once(':')
                .map(|(_, right)| right.trim())
                .unwrap_or_default();
            if !value.is_empty() {
                criteria.extend(split_list_like(value));
            }
        }
    }
    let lower = message.to_ascii_lowercase();
    if lower.contains("test") {
        criteria.push("Relevant tests pass.".to_owned());
    }
    if lower.contains("build") {
        criteria.push("The build passes.".to_owned());
    }
    criteria
}

fn extract_risks(message: &str) -> Vec<String> {
    let mut risks = Vec::new();
    for line in message.lines().map(str::trim) {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower
            .find("risk:")
            .or_else(|| lower.find("risks:"))
            .and_then(|index| line.get(index..))
        {
            let value = rest
                .split_once(':')
                .map(|(_, right)| right.trim())
                .unwrap_or_default();
            if !value.is_empty() {
                risks.extend(split_list_like(value));
            }
        }
    }
    risks
}

fn split_list_like(value: &str) -> Vec<String> {
    value
        .split([';', '|'])
        .map(|item| item.trim().trim_start_matches('-').trim())
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn plan_steps_for(message: &str) -> Vec<String> {
    let mut steps = vec![
        "Confirm the scoped goal and acceptance criteria.".to_owned(),
        "Gather bounded repository evidence for the affected scope.".to_owned(),
        "Execute the selected workflow through role-specific harnesses.".to_owned(),
        "Report checks, evidence, patches, blockers, and next steps.".to_owned(),
    ];
    if message.to_ascii_lowercase().contains("refactor") {
        steps.insert(2, "Preserve behavior while changing structure.".to_owned());
    }
    steps
}

fn unique_strings(items: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    items
        .into_iter()
        .filter_map(|item| {
            let item = item.trim().to_owned();
            if item.is_empty() || !seen.insert(item.clone()) {
                None
            } else {
                Some(item)
            }
        })
        .collect()
}

fn clean_provider_string_map(values: BTreeMap<String, String>) -> BTreeMap<String, String> {
    values
        .into_iter()
        .filter_map(|(provider, value)| {
            let provider = normalize_provider(&provider);
            let value = value.trim().to_owned();
            (!provider.is_empty() && !value.is_empty()).then_some((provider, value))
        })
        .collect()
}

fn resolve_repo_relative_path(repo_root: &str, relative_path: &str) -> Result<PathBuf, ApiError> {
    let root = fs::canonicalize(repo_root)
        .map_err(|error| ApiError::bad_request(format!("invalid repo_root: {error}")))?;
    let requested = PathBuf::from(relative_path);
    if requested.is_absolute() {
        return Err(ApiError::bad_request("memory_path must be relative"));
    }
    let resolved = fs::canonicalize(root.join(&requested))
        .map_err(|error| ApiError::bad_request(format!("invalid memory_path: {error}")))?;
    if !resolved.starts_with(&root) {
        return Err(ApiError::bad_request("memory_path escapes repo_root"));
    }
    Ok(resolved)
}

fn resolve_repo_relative_write_path(
    repo_root: &str,
    relative_path: &str,
) -> Result<PathBuf, ApiError> {
    let root = fs::canonicalize(repo_root)
        .map_err(|error| ApiError::bad_request(format!("invalid repo_root: {error}")))?;
    let requested = PathBuf::from(relative_path);
    if requested.is_absolute() || relative_path.trim().is_empty() {
        return Err(ApiError::bad_request("memory_path must be relative"));
    }
    if requested
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ApiError::bad_request("memory_path escapes repo_root"));
    }
    Ok(root.join(requested))
}

fn knowledge_store_for_repo(repo_root: &str) -> Result<KnowledgeStore, ApiError> {
    let root = fs::canonicalize(repo_root)
        .map_err(|error| ApiError::bad_request(format!("invalid repo_root: {error}")))?;
    Ok(KnowledgeStore::new(root.join(".coder").join("memory")))
}

fn control_run(
    state: ApiState,
    run_id: RunId,
    action: RunControlAction,
) -> Result<Json<RunControlResponse>, ApiError> {
    let mut metadata = state
        .store
        .read_metadata(&run_id)?
        .ok_or_else(|| ApiError::not_found(format!("run '{}' was not found", run_id.as_str())))?;
    let events = state.store.read_events(&run_id)?;
    let (kind, status_text) = match action {
        RunControlAction::Pause => ("run.paused", "paused"),
        RunControlAction::Resume => {
            metadata.status = RunStatus::Running;
            ("run.resumed", "running")
        }
        RunControlAction::Cancel => {
            metadata.status = RunStatus::Cancelled;
            ("run.cancelled", "cancelled")
        }
    };
    let sequence = events.len() as u64 + 1;
    let event_count = events.len() + 1;
    let event = coder_events::CoderEvent::new(
        run_id.clone(),
        sequence,
        kind,
        json!({
            "status": status_text,
        }),
    );
    metadata.updated_at = event.timestamp;
    state.store.write_metadata(&metadata)?;
    state.store.append_event(&run_id, &event)?;
    let report_ref = if matches!(action, RunControlAction::Cancel) {
        let report = state.store.build_evidence_report(&run_id)?;
        Some(state.store.write_report(&run_id, &report)?)
    } else {
        None
    };
    Ok(Json(RunControlResponse {
        run_id: run_id.to_string(),
        status: metadata.status,
        control_state: status_text.to_owned(),
        event_count,
        report_ref,
    }))
}

fn write_tool_evidence(
    store: &RunStore,
    run_id: Option<&str>,
    kind: RepoEvidenceKind,
    repo_root: &str,
    summary: impl Into<String>,
    payload: Value,
) -> Result<Option<RepoEvidenceRef>, ApiError> {
    let Some(run_id) = run_id else {
        return Ok(None);
    };
    let repo_root = fs::canonicalize(repo_root)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| repo_root.to_owned());
    let reference = store.write_repo_evidence(
        &RunId::from_string(run_id.to_owned()),
        kind,
        repo_root,
        Vec::new(),
        summary,
        payload,
    )?;
    Ok(Some(reference))
}

fn record_command_events(
    store: &RunStore,
    run_id: &RunId,
    output: &CommandRunEvidence,
    evidence_ref: &RepoEvidenceRef,
) -> Result<(), StoreError> {
    let mut sequence = store.read_events(run_id)?.len() as u64 + 1;
    let evidence_uri = format!("repo-evidence://{}", evidence_ref.ref_id);
    if output.blocked && output.requires_approval {
        store.append_event(
            run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                sequence,
                "approval.requested",
                json!({
                    "approval_type": "command",
                    "approval_key": &output.approval_key,
                    "command": &output.command,
                    "cwd": &output.cwd,
                    "policy": &output.policy,
                    "evidence_ref": &evidence_ref.ref_id,
                }),
            )
            .with_ref("command_evidence", evidence_uri),
        )?;
        return Ok(());
    }

    store.append_event(
        run_id,
        &coder_events::CoderEvent::new(
            run_id.clone(),
            sequence,
            "command.started",
            json!({
                "command": &output.command,
                "argv": &output.argv,
                "cwd": &output.cwd,
                "approval_key": &output.approval_key,
                "policy": &output.policy,
                "evidence_ref": &evidence_ref.ref_id,
            }),
        )
        .with_ref("command_evidence", evidence_uri.clone()),
    )?;
    sequence += 1;
    let kind = match output.status.as_str() {
        "completed" => "command.completed",
        _ => "command.failed",
    };
    store.append_event(
        run_id,
        &coder_events::CoderEvent::new(
            run_id.clone(),
            sequence,
            kind,
            json!({
                "command": &output.command,
                "cwd": &output.cwd,
                "status": &output.status,
                "passed": output.passed,
                "returncode": output.returncode,
                "timed_out": output.timed_out,
                "output_preview": &output.output,
                "output_truncated": output.output_truncated,
                "evidence_ref": &evidence_ref.ref_id,
            }),
        )
        .with_ref("command_evidence", evidence_uri),
    )?;
    Ok(())
}

fn record_patch_apply_event(
    store: &RunStore,
    run_id: &RunId,
    output: &PatchApplyEvidence,
    evidence_ref: &RepoEvidenceRef,
) -> Result<(), StoreError> {
    let sequence = store.read_events(run_id)?.len() as u64 + 1;
    let evidence_uri = format!("repo-evidence://{}", evidence_ref.ref_id);
    if output.requires_approval {
        store.append_event(
            run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                sequence,
                "approval.requested",
                json!({
                    "approval_type": "patch_apply",
                    "approval_key": &output.approval_key,
                    "patch_file": &output.patch_file,
                    "reason": &output.reason,
                    "files": &output.preview.files,
                    "evidence_ref": &evidence_ref.ref_id,
                }),
            )
            .with_ref("patch_evidence", evidence_uri),
        )?;
        return Ok(());
    }

    let kind = if output.applied {
        "patch.applied"
    } else {
        "patch.failed"
    };
    store.append_event(
        run_id,
        &coder_events::CoderEvent::new(
            run_id.clone(),
            sequence,
            kind,
            json!({
                "status": &output.status,
                "patch_file": &output.patch_file,
                "applied": output.applied,
                "reason": &output.reason,
                "approval_key": &output.approval_key,
                "file_count": output.preview.file_count,
                "files": &output.preview.files,
                "evidence_ref": &evidence_ref.ref_id,
            }),
        )
        .with_ref("patch_evidence", evidence_uri),
    )?;
    Ok(())
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": self.message,
            })),
        )
            .into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::RunNotFound(_)
            | StoreError::RepoEvidenceNotFound(_)
            | StoreError::ArtifactNotFound { .. }
            | StoreError::CheckpointNotFound { .. }
            | StoreError::BlobNotFound(_) => Self::not_found(error.to_string()),
            StoreError::InvalidStoreSegment { .. }
            | StoreError::InvalidFileName(_)
            | StoreError::InvalidBlobDigest(_) => Self {
                status: StatusCode::BAD_REQUEST,
                message: error.to_string(),
            },
            other => Self::internal(other.to_string()),
        }
    }
}

impl From<WorkflowError> for ApiError {
    fn from(error: WorkflowError) -> Self {
        match error {
            WorkflowError::InvalidConfig(_)
            | WorkflowError::WorkflowNotFound(_)
            | WorkflowError::BackendNotFound(_) => Self {
                status: StatusCode::BAD_REQUEST,
                message: error.to_string(),
            },
            WorkflowError::Store(error) => Self::from(error),
        }
    }
}

impl From<RepoToolError> for ApiError {
    fn from(error: RepoToolError) -> Self {
        match error {
            RepoToolError::InvalidRoot { .. }
            | RepoToolError::InvalidRootKind(_)
            | RepoToolError::PathNotFound { .. }
            | RepoToolError::PathOutsideRepo(_)
            | RepoToolError::NotAFile(_)
            | RepoToolError::NotADirectory(_)
            | RepoToolError::SensitivePath(_)
            | RepoToolError::BinaryFile(_)
            | RepoToolError::FileTooLarge { .. }
            | RepoToolError::EmptyQuery
            | RepoToolError::PatchNoFiles(_)
            | RepoToolError::EmptyCommandArgv => Self {
                status: StatusCode::BAD_REQUEST,
                message: error.to_string(),
            },
            other => Self::internal(other.to_string()),
        }
    }
}

impl From<MemoryError> for ApiError {
    fn from(error: MemoryError) -> Self {
        match error {
            MemoryError::PolicyViolation(_) => Self::forbidden(error.to_string()),
            _ => Self::bad_request(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, process::Command};

    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::*;

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
        assert_eq!(
            body["workflow"]["nodes"][0]["harness"],
            "planner-conversation"
        );
        assert_eq!(body["workflow"]["name"], "Planner-led Agent Workflow");
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
            "Discuss mode recorded the turn without starting execution."
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
        assert_eq!(
            confirmed["plan_draft"]["affected_paths"][0],
            "crates/coder-server/src/lib.rs"
        );
    }

    #[tokio::test]
    async fn planner_chat_rejects_non_planner_model_harness() {
        let app = test_router();
        let mut config = default_project_config();
        config
            .workflows
            .get_mut("planner-led")
            .unwrap()
            .nodes
            .first_mut()
            .unwrap()
            .harness = "review-only".to_owned();

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
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("backend 'planner-model'"));
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
            .contains("Planner model provider is not configured"));
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
                    "evidence_refs": [{"kind": "doc", "reference": "docs/rust-migration-map.md"}],
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
                event.kind == "mcp.tool.completed"
                    && event.payload["tool_name"] == "mock.large_output"
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
        assert!(plugin_ids.contains("openhands-task-executor-runtime"));
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
        assert!(!tool_names.contains("inspect_run_state"));
        let patch_tool = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["capability"]["name"] == "apply_patch_sandbox")
            .unwrap();
        assert_eq!(patch_tool["requires_approval"], true);
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

        let save = post_json(
            app.clone(),
            "/api/v3/providers/settings",
            json!({
                "default_provider": "deepseek",
                "default_model": "deepseek-chat",
                "base_urls": {"deepseek": "https://api.deepseek.com"},
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
            provider_chat_completions_endpoint_for_display(
                "https://user:secret@api.deepseek.com/v1?token=secret#fragment",
            ),
            "https://api.deepseek.com/v1/chat/completions"
        );
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
            .any(|backend| backend.as_str() == Some("openhands")));
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
            harness.openhands = None;
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
        assert!(body["report"]["checks"]
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
        let app = router(ApiState::new(store.clone()));

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
        let app = router(ApiState::new(store));

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
        let run_id = start_body["run_id"].as_str().unwrap();
        assert_eq!(
            start_body["events_url"],
            format!("/api/v3/runs/{run_id}/events")
        );
        assert_eq!(
            start_body["timeline_url"],
            format!("/api/v3/runs/{run_id}/timeline")
        );
        let run_id = RunId::from_string(run_id);
        let events = store.read_events(&run_id).unwrap();
        assert_eq!(events[0].kind, "run.started");
        assert!(events[0].payload.get("plan_context").is_some());
        let report = store.read_report(&run_id).unwrap().unwrap();
        assert!(report
            .checks
            .iter()
            .any(|check| check.starts_with("plan_context:")));
        assert!(report
            .checks
            .iter()
            .any(|check| check.starts_with("acceptance:")));
        let _ = fs::remove_dir_all(root);
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
            .contains("Planner model provider is not configured"));
        assert_eq!(body["session"]["readiness"], "blocked");
        assert!(store.list_run_summaries().unwrap().is_empty());
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
                    3,
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
                    4,
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
                    5,
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
                    6,
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
                    7,
                    "observation.recorded",
                    json!({"agent_id": "executor", "tool_name": "repo_find_files", "summary": "Found README.md"}),
                ),
            )
            .unwrap();
        store
            .append_event(
                &run_id,
                &coder_events::CoderEvent::new(
                    run_id.clone(),
                    8,
                    "backend.openhands.ActionEvent",
                    json!({"raw": {"api_key": "raw-secret-value"}, "raw_ref": "blob://sha256/raw"}),
                ),
            )
            .unwrap();
        store
            .write_report(
                &run_id,
                &FinalReport::completed("Done").with_check("cargo test: completed exit 0"),
            )
            .unwrap();
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
        assert!(items
            .iter()
            .any(|item| item["type"] == "executor_step" && item["title"] == "Action selected"));
        assert!(
            items
                .iter()
                .any(|item| item["type"] == "executor_step"
                    && item["title"] == "Observation recorded")
        );
        assert!(items
            .iter()
            .any(|item| item["type"] == "tool_call" && item["tool_name"] == "repo_find_files"));
        assert!(items.iter().any(|item| item["type"] == "command_execution"));
        assert!(items.iter().any(|item| item["type"] == "file_change"));
        assert!(items.iter().any(|item| item["type"] == "final_summary"));
        assert!(items.iter().all(|item| item.get("payload").is_none()));
        assert!(!body.to_string().contains("raw-secret-value"));
        let _ = fs::remove_dir_all(root);
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

    fn test_router() -> Router {
        let state = ApiState::new(RunStore::new(temp_root()));
        state.provider_settings.lock().unwrap().mock_mode = true;
        router(state)
    }

    async fn spawn_openai_compatible_test_server() -> String {
        async fn chat_completion() -> Json<Value> {
            Json(json!({
                "choices": [
                    {
                        "message": {
                            "content": "Live provider response."
                        }
                    }
                ]
            }))
        }

        let app = Router::new().route("/chat/completions", axum::routing::post(chat_completion));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
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

    async fn response_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn example_config() -> Value {
        serde_yaml::from_str::<ProjectConfig>(include_str!("../../../examples/coder.yaml"))
            .map(|config| serde_json::to_value(config).unwrap())
            .unwrap()
    }

    fn temp_root() -> PathBuf {
        static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("coder-server-{}-{}", std::process::id(), id))
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
}
