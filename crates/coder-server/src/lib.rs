use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use coder_config::{
    validate_project_config, ProjectConfig, ValidationIssue, ValidationLevel, ValidationReport,
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
    validate_mcp_manifest, McpManifestValidation, ToolRegistry, ToolRegistryEntry,
};
use coder_memory::{
    append_project_memory_record, ensure_memory_write_allowed, import_text_knowledge_source,
    load_project_memory_file, memory_read_event, memory_write_confirmed_event,
    memory_write_proposed_event, retrieve_knowledge_hints, AgentMemoryRole, KnowledgeChunk,
    KnowledgeRetrievalRequest, KnowledgeSource, KnowledgeStore, KnowledgeTextImportRequest,
    MemoryAllowedContext, MemoryError, MemoryPurpose, MemoryRecord, MemoryScope, MemorySensitivity,
    ProjectMemoryFile,
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
use coder_workflow::{MockWorkflowRunner, WorkflowError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct ApiState {
    pub store: RunStore,
    library_workflows: Arc<Mutex<BTreeMap<String, Value>>>,
    planner_sessions: Arc<Mutex<BTreeMap<String, PlannerChatSession>>>,
    installed_skills: Arc<Mutex<BTreeMap<String, InstalledSkillRecord>>>,
}

impl ApiState {
    pub fn new(store: RunStore) -> Self {
        Self {
            store,
            library_workflows: Arc::new(Mutex::new(BTreeMap::new())),
            planner_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            installed_skills: Arc::new(Mutex::new(BTreeMap::new())),
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
        .route("/api/v3/harness/tools", get(list_harness_tools))
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
        settings: vec!["provider_profiles_deferred"],
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
                default_output_contract: "planner_order",
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
    let config: ProjectConfig =
        serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
    let workflow_id = "planner-led".to_owned();
    let workflow = config.workflows.get(&workflow_id).cloned();
    Json(DefaultWorkflowResponse {
        workflow_id,
        config,
        workflow,
    })
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
) -> Json<PlannerChatSessionResponse> {
    let session_id = format!("pcs_{}", RunId::new());
    let session = PlannerChatSession {
        session_id: session_id.clone(),
        workflow_id: request
            .workflow_id
            .unwrap_or_else(|| "planner-led".to_owned()),
        mode: request.mode.unwrap_or_else(|| "discuss".to_owned()),
        ready: false,
        turns: Vec::new(),
    };
    state
        .planner_sessions
        .lock()
        .unwrap()
        .insert(session_id.clone(), session.clone());
    Json(PlannerChatSessionResponse { session })
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
    let mut sessions = state.planner_sessions.lock().unwrap();
    let session = sessions
        .get_mut(&session_id)
        .ok_or_else(|| ApiError::not_found(format!("session '{session_id}' was not found")))?;
    let user_turn = PlannerChatTurn {
        role: "user".to_owned(),
        content: request.message.clone(),
    };
    session.turns.push(user_turn);
    let ready = request.message.to_ascii_lowercase().contains("ready");
    session.ready = session.ready || ready;
    let execution_allowed =
        session.mode == "work" && session.ready && request.confirmed == Some(true);
    let assistant = if session.mode == "discuss" {
        "Discuss mode recorded the turn without starting execution.".to_owned()
    } else if execution_allowed {
        "Work mode is confirmed and ready for run creation.".to_owned()
    } else {
        "Work mode needs a ready task state and explicit confirmation before execution.".to_owned()
    };
    session.turns.push(PlannerChatTurn {
        role: "assistant".to_owned(),
        content: assistant.clone(),
    });
    Ok(Json(PlannerChatTurnResponse {
        session: session.clone(),
        assistant_message: assistant,
        ready: session.ready,
        execution_allowed,
        run_preview: if session.mode == "work" {
            Some(json!({
                "status": if session.ready { "ready" } else { "blocked" },
                "requires_confirmation": session.ready,
                "workflow_id": session.workflow_id
            }))
        } else {
            None
        },
    }))
}

async fn load_project_memory(
    State(state): State<ApiState>,
    Json(request): Json<ProjectMemoryLoadRequest>,
) -> Result<Json<ProjectMemoryLoadResponse>, ApiError> {
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
            tags: request.tags.unwrap_or_default(),
            token_budget: request.token_budget,
            max_results: request.max_results,
            include_content: request.include_content.unwrap_or(false),
        },
    )?;
    Ok(Json(KnowledgeRetrieveResponse { results }))
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

async fn list_harness_tools(Query(query): Query<ToolRegistryQuery>) -> Json<ToolRegistryResponse> {
    let registry = ToolRegistry::default();
    Json(ToolRegistryResponse {
        tools: registry.list_tools(query.harness_id.as_deref()),
        harness_id: query.harness_id,
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
    run_mock_workflow(State(state), Json(request)).await
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
    pub ready: bool,
    pub turns: Vec<PlannerChatTurn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct PlannerChatSessionCreateRequest {
    pub workflow_id: Option<String>,
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
}

#[derive(Debug, Serialize)]
pub struct PlannerChatTurnResponse {
    pub session: PlannerChatSession,
    pub assistant_message: String,
    pub ready: bool,
    pub execution_allowed: bool,
    pub run_preview: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectMemoryLoadRequest {
    pub repo_root: String,
    pub memory_path: String,
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
    pub tags: Option<Vec<String>>,
    pub token_budget: Option<usize>,
    pub max_results: Option<usize>,
    pub include_content: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct KnowledgeRetrieveResponse {
    pub results: Vec<coder_memory::KnowledgeHint>,
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
    use std::{fs, path::PathBuf};

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
        assert_eq!(turn_body["ready"], true);
        assert_eq!(turn_body["execution_allowed"], false);
        assert_eq!(turn_body["run_preview"], Value::Null);
    }

    #[tokio::test]
    async fn planner_chat_work_mode_requires_ready_and_confirmation() {
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
        assert_eq!(unready["run_preview"]["status"], "blocked");

        let unconfirmed_response = post_json(
            app.clone(),
            &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
            json!({
                "message": "ready to run",
                "confirmed": false
            }),
        )
        .await;
        let unconfirmed = response_json(unconfirmed_response).await;
        assert_eq!(unconfirmed["ready"], true);
        assert_eq!(unconfirmed["execution_allowed"], false);
        assert_eq!(unconfirmed["run_preview"]["requires_confirmation"], true);

        let confirmed_response = post_json(
            app,
            &format!("/api/v3/planner-chat/sessions/{session_id}/turn"),
            json!({
                "message": "ready and confirmed",
                "confirmed": true
            }),
        )
        .await;
        let confirmed = response_json(confirmed_response).await;
        assert_eq!(confirmed["execution_allowed"], true);
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
                "confirmed_by_role": "workflow_supervisor",
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
    async fn task_execution_cannot_confirm_project_memory_write() {
        let repo = temp_root();
        let store_root = temp_root();
        fs::create_dir_all(&repo).unwrap();
        let store = RunStore::new(&store_root);
        let run_id = RunId::from_string("run-1");
        let state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
        store.write_metadata(&state).unwrap();
        let app = router(ApiState::new(store));

        let response = post_json(
            app,
            "/api/v3/memory/project/confirm-write",
            json!({
                "repo_root": repo.display().to_string(),
                "memory_path": "memory.json",
                "run_id": "run-1",
                "confirmed_by_role": "task_execution",
                "record": {
                    "id": "mem_4",
                    "scope": "project",
                    "key": "blocked",
                    "content": "Executor should not directly persist this.",
                    "tags": [],
                    "source_ref": "memory://project/blocked"
                }
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
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
        assert_eq!(retrieve_body["results"][0]["content_preview"], Value::Null);

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

    fn test_router() -> Router {
        router(ApiState::new(RunStore::new(temp_root())))
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
