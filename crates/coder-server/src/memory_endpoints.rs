use axum::{
    extract::{Path as AxumPath, Query, State},
    Json,
};
use coder_core::RunId;
use coder_memory::{
    append_project_memory_record, ensure_memory_write_allowed, import_text_knowledge_source,
    load_project_memory_file, memory_read_event, memory_write_confirmed_event,
    memory_write_proposed_event, retrieve_knowledge_hints, AgentMemoryRole, KnowledgeRetrievalHit,
    KnowledgeRetrievalRequest, KnowledgeStore, KnowledgeTextImportRequest, MemoryScope,
    MemorySensitivity,
};
use std::{fs, path::PathBuf};

use crate::stored_run_exists;
use crate::{
    ApiError, ApiState, KnowledgeRetrieveApiRequest, KnowledgeRetrieveResponse,
    KnowledgeSourceChunksResponse, KnowledgeSourceListResponse, KnowledgeTextImportApiRequest,
    KnowledgeTextImportResponse, ProjectMemoryLoadRequest, ProjectMemoryLoadResponse,
    ProjectMemoryWriteConfirmRequest, ProjectMemoryWriteConfirmResponse,
    ProjectMemoryWriteProposalRequest, ProjectMemoryWriteProposalResponse, RepoRootQuery,
};

pub(crate) async fn load_project_memory(
    State(state): State<ApiState>,
    Json(request): Json<ProjectMemoryLoadRequest>,
) -> Result<Json<ProjectMemoryLoadResponse>, ApiError> {
    if request.requested_by_role != AgentMemoryRole::Conversation {
        return Err(ApiError::forbidden(
            "only conversation can read project long-term memory",
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
        let sequence = state.store.event_count(&run_id)? as u64 + 1;
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

pub(crate) async fn propose_project_memory_write(
    State(state): State<ApiState>,
    Json(request): Json<ProjectMemoryWriteProposalRequest>,
) -> Result<Json<ProjectMemoryWriteProposalResponse>, ApiError> {
    if request.record.scope != MemoryScope::Project {
        return Err(ApiError::bad_request(
            "project memory write proposals require scope 'project'",
        ));
    }
    if request.proposed_by_role != AgentMemoryRole::Conversation {
        return Err(ApiError::forbidden(
            "only conversation can propose project memory writes",
        ));
    }
    let run_id = RunId::from_string(request.run_id);
    if !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    let sequence = state.store.event_count(&run_id)? as u64 + 1;
    let event = memory_write_proposed_event(run_id.clone(), sequence, &request.record);
    state.store.append_event(&run_id, &event)?;
    Ok(Json(ProjectMemoryWriteProposalResponse {
        run_id: run_id.to_string(),
        event_count: sequence as usize,
        event,
    }))
}

pub(crate) async fn confirm_project_memory_write(
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
        let sequence = state.store.event_count(&run_id)? as u64 + 1;
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

pub(crate) async fn import_knowledge_text(
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

pub(crate) async fn list_knowledge_sources(
    Query(query): Query<RepoRootQuery>,
) -> Result<Json<KnowledgeSourceListResponse>, ApiError> {
    let store = knowledge_store_for_repo(&query.repo_root)?;
    Ok(Json(KnowledgeSourceListResponse {
        sources: store.list_sources()?,
    }))
}

pub(crate) async fn list_knowledge_source_chunks(
    Query(query): Query<RepoRootQuery>,
    AxumPath(source_id): AxumPath<String>,
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

pub(crate) async fn retrieve_knowledge(
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
