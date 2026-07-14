use std::{collections::BTreeSet, path::PathBuf, time::Duration};

use axum::{
    extract::{Path, Query, State},
    Json,
};
use coder_config::{
    resolve_agent_runtime_policy, AgentRuntimePolicy, ModelCapabilities, ModelSpec, ProjectConfig,
    ResolvedAgentRuntimePolicy,
};
use coder_core::RunId;
use coder_store::{
    CompactionCircuitState, DurableJsonlPageOptions, RunStore, MAX_DURABLE_JSONL_PAGE_LIMIT,
};
use coder_tools::{read_file_range, RepoReadSnippet};
use coder_workflow::{context_budget_for_runtime, ModelToolResultBlock};
use serde_json::{json, Value};

use crate::provider_runtime::{
    normalize_provider, provider_api_key, provider_base_url, provider_chat_completions_endpoint,
    provider_chat_completions_endpoint_for_display, provider_http_client_builder,
    provider_proxy_url_for_url, provider_request_max_retries, redact_provider_error,
    send_provider_request_with_retry,
};
use crate::run_token_budget::{
    check_existing_run_token_budget, provider_token_usage, record_existing_run_token_usage,
};
use crate::{
    estimate_text_tokens, public_preview, stored_run_exists, truncate_text_to_chars, ApiError,
    ApiState, ModelToolExecuteResponse, ProviderSettings, RunContentReplacementsResponse,
    RunEventsQuery, RunTranscriptCompactionCircuitResponse, RunTranscriptCompactionRequest,
    RunTranscriptCompactionResponse, CONTENT_REPLACEMENT_REPLAY_CONTRACT, INVOKED_SKILL_CONTRACT,
    INVOKED_SKILL_EVENT_KIND, POST_COMPACT_FILE_RESTORE_CONTRACT, POST_COMPACT_MAX_CHARS_PER_FILE,
    POST_COMPACT_MAX_CHARS_PER_SKILL, POST_COMPACT_MAX_FILES_TO_RESTORE,
    POST_COMPACT_MAX_TOKENS_PER_FILE, POST_COMPACT_MAX_TOKENS_PER_SKILL,
    POST_COMPACT_SKILLS_TOKEN_BUDGET, POST_COMPACT_TOKEN_BUDGET,
    RUN_RESUME_CONTENT_REPLACEMENT_RECORD_LIMIT, RUN_TRANSCRIPT_COMPACTION_ATTACHMENT_CONTRACT,
    RUN_TRANSCRIPT_COMPACTION_CONTRACT, RUN_TRANSCRIPT_COMPACTION_EVENT_KIND,
    RUN_TRANSCRIPT_COMPACTION_MAX_EVENTS, RUN_TRANSCRIPT_COMPACTION_MAX_EVENT_CHARS,
    RUN_TRANSCRIPT_COMPACTION_MAX_OUTPUT_TOKENS,
};
pub(crate) async fn compact_run_transcript(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    Json(request): Json<RunTranscriptCompactionRequest>,
) -> Result<Json<RunTranscriptCompactionResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    compact_run_transcript_for_run(state, run_id, request)
        .await
        .map(Json)
}

pub(crate) async fn compact_run_transcript_for_run(
    state: ApiState,
    run_id: RunId,
    request: RunTranscriptCompactionRequest,
) -> Result<RunTranscriptCompactionResponse, ApiError> {
    let events = state.store.read_events(&run_id)?;
    if events.is_empty() && !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }

    let max_events = request
        .max_events
        .unwrap_or(RUN_TRANSCRIPT_COMPACTION_MAX_EVENTS);
    if max_events == 0 || max_events > RUN_TRANSCRIPT_COMPACTION_MAX_EVENTS {
        return Err(ApiError::bad_request(format!(
            "max_events must be between 1 and {RUN_TRANSCRIPT_COMPACTION_MAX_EVENTS}"
        )));
    }

    let scope_id = request
        .scope_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("run-transcript-{}", run_id.as_str()));
    let max_consecutive_failures =
        AgentRuntimePolicy::default().max_consecutive_compaction_failures;
    let existing_circuit = state.store.read_compaction_circuit_state(&scope_id)?;
    if existing_circuit
        .as_ref()
        .map(|state| state.circuit_breaker_open)
        .unwrap_or(false)
    {
        let circuit = transcript_compaction_circuit_response(
            &scope_id,
            max_consecutive_failures,
            existing_circuit.as_ref(),
        );
        let mut response = transcript_compaction_response_base(TranscriptCompactionResponseInput {
            run_id: &run_id,
            status: "circuit_open",
            success: false,
            provider: None,
            model: None,
            endpoint: None,
            summary: None,
            artifact_ref: None,
            events: &events,
            max_events,
            transcript_estimated_tokens: 0,
            event_sequence: None,
            error: Some("Transcript compaction circuit breaker is open.".to_owned()),
            circuit,
        });
        response.event_sequence = Some(append_run_transcript_compaction_event(
            &state.store,
            &run_id,
            transcript_compaction_event_payload(&response),
            None,
        )?);
        return Ok(response);
    }

    let transcript = build_run_transcript_projection_with_content_replacements(
        &state.store,
        &run_id,
        &events,
        max_events,
    );
    let settings = state.provider_settings.lock().unwrap().clone();
    let model_result = request_run_transcript_compaction_summary(
        &state,
        &run_id,
        &settings,
        &transcript.text,
        &request,
    )
    .await;
    let failure_context = FailedTranscriptCompactionContext {
        store: &state.store,
        run_id: &run_id,
        scope_id: &scope_id,
        max_consecutive_failures,
        events: &events,
        max_events,
        transcript_estimated_tokens: transcript.estimated_tokens,
    };

    let response = match model_result {
        Ok(model) => {
            let summary = format_compact_summary(&model.raw_summary);
            if summary.trim().is_empty() {
                record_failed_run_transcript_compaction(
                    &failure_context,
                    Some(model.provider),
                    Some(model.model),
                    Some(model.endpoint),
                    "Provider response did not include a usable compact summary.".to_owned(),
                )?
            } else {
                let circuit_state = state.store.record_compaction_circuit_outcome(
                    &scope_id,
                    max_consecutive_failures,
                    true,
                )?;
                let sequence = state.store.event_count(&run_id)? as u64 + 1;
                let summary_estimated_tokens = estimate_text_tokens(&summary);
                let artifact_payload = json!({
                    "contract": RUN_TRANSCRIPT_COMPACTION_CONTRACT,
                    "source": "coder-server",
                    "run_id": run_id.as_str(),
                    "status": "completed",
                    "summary": &summary,
                    "summary_estimated_tokens": summary_estimated_tokens,
                    "transcript_event_count": transcript.event_count,
                    "transcript_events_included": transcript.included_events,
                    "transcript_events_omitted": transcript.omitted_events,
                    "transcript_truncated": transcript.truncated,
                    "transcript_estimated_tokens": transcript.estimated_tokens,
                    "content_replacement_replay": transcript.content_replacement_replay.clone(),
                    "provider": &model.provider,
                    "model": &model.model,
                    "endpoint": &model.endpoint,
                    "circuit": transcript_compaction_circuit_response(
                        &scope_id,
                        max_consecutive_failures,
                        Some(&circuit_state),
                    )
                });
                let artifact_ref = state.store.write_artifact(
                    &run_id,
                    &format!("transcript-compaction-{sequence}.json"),
                    &artifact_payload,
                )?;
                let mut response = RunTranscriptCompactionResponse {
                    contract: RUN_TRANSCRIPT_COMPACTION_CONTRACT,
                    source: "coder-server",
                    policy: "claude_style_model_summary_with_persistent_circuit",
                    run_id: run_id.to_string(),
                    status: "completed".to_owned(),
                    success: true,
                    provider: Some(model.provider),
                    model: Some(model.model),
                    endpoint: Some(model.endpoint),
                    summary: artifact_payload["summary"].as_str().map(str::to_owned),
                    summary_estimated_tokens,
                    transcript_event_count: transcript.event_count,
                    transcript_events_included: transcript.included_events,
                    transcript_events_omitted: transcript.omitted_events,
                    transcript_truncated: transcript.truncated,
                    transcript_estimated_tokens: transcript.estimated_tokens,
                    artifact_ref: Some(artifact_ref.clone()),
                    event_sequence: None,
                    error: None,
                    circuit: transcript_compaction_circuit_response(
                        &scope_id,
                        max_consecutive_failures,
                        Some(&circuit_state),
                    ),
                };
                response.event_sequence = Some(append_run_transcript_compaction_event(
                    &state.store,
                    &run_id,
                    transcript_compaction_event_payload(&response),
                    Some(artifact_ref),
                )?);
                response
            }
        }
        Err(error) => {
            record_failed_run_transcript_compaction(&failure_context, None, None, None, error)?
        }
    };

    Ok(response)
}

#[derive(Debug, Clone)]
struct RunTranscriptProjection {
    text: String,
    event_count: usize,
    included_events: usize,
    omitted_events: usize,
    truncated: bool,
    estimated_tokens: u32,
    content_replacement_replay: Option<Value>,
}

#[derive(Debug, Clone)]
pub(crate) struct RunTranscriptAutoCompactionDecision {
    pub should_compact: bool,
    pub reason: String,
    pub runtime_source: String,
    pub runtime_agent_id: Option<String>,
    pub runtime_context_window_tokens: u32,
    pub runtime_auto_compact_token_limit: Option<u32>,
    pub effective_estimated_tokens: u32,
    pub projected_tokens: u32,
    pub threshold_tokens: u32,
    pub blocking_limit_tokens: u32,
    pub estimated_max_turn_growth_tokens: u32,
    pub event_count: usize,
    pub boundary_sequence: Option<u64>,
    pub events_after_boundary: usize,
    pub circuit_breaker_open: bool,
}

#[derive(Debug, Clone)]
struct RunTranscriptCompactionModelOutput {
    provider: String,
    model: String,
    endpoint: String,
    raw_summary: String,
}

fn build_run_transcript_projection(
    events: &[coder_events::CoderEvent],
    max_events: usize,
) -> RunTranscriptProjection {
    let event_count = events.len();
    let omitted_events = event_count.saturating_sub(max_events);
    let included = events.iter().skip(omitted_events).collect::<Vec<_>>();
    let mut text = format!(
        "Coder run transcript projection. total_events={event_count}; included_events={}; omitted_older_events={omitted_events}.\n",
        included.len()
    );
    for event in &included {
        let payload = serde_json::to_string(&event.payload)
            .unwrap_or_else(|_| "{\"unserializable\":true}".to_owned());
        text.push_str(&format!(
            "\n[sequence={}; kind={}]\n{}\n",
            event.sequence,
            event.kind,
            public_preview(&payload, RUN_TRANSCRIPT_COMPACTION_MAX_EVENT_CHARS)
        ));
    }
    RunTranscriptProjection {
        estimated_tokens: estimate_text_tokens(&text),
        text,
        event_count,
        included_events: included.len(),
        omitted_events,
        truncated: omitted_events > 0,
        content_replacement_replay: None,
    }
}

fn build_run_transcript_projection_with_content_replacements(
    store: &RunStore,
    run_id: &RunId,
    events: &[coder_events::CoderEvent],
    max_events: usize,
) -> RunTranscriptProjection {
    let mut projection = build_run_transcript_projection(events, max_events);
    let Some(replay) = build_content_replacement_transcript_projection(store, run_id) else {
        return projection;
    };
    projection.text.push_str(&replay.text);
    projection.estimated_tokens = estimate_text_tokens(&projection.text);
    projection.content_replacement_replay = Some(replay.summary);
    projection
}

#[derive(Debug, Clone)]
struct ContentReplacementTranscriptProjection {
    text: String,
    summary: Value,
}

fn build_content_replacement_transcript_projection(
    store: &RunStore,
    run_id: &RunId,
) -> Option<ContentReplacementTranscriptProjection> {
    let options =
        DurableJsonlPageOptions::tail(RUN_RESUME_CONTENT_REPLACEMENT_RECORD_LIMIT).ok()?;
    let page = store
        .read_run_content_replacement_records_page(run_id, options)
        .ok()?;
    if page.returned_records == 0 {
        return None;
    }
    let replacement_count = page
        .records
        .iter()
        .map(|record| record.replacements.len())
        .sum::<usize>();
    let records_ref = format!(
        "content-replacements://runs/{}/content-replacements.jsonl",
        run_id.as_str()
    );
    let mut text = format!(
        "\n[content_replacement_replay]\nrecords_ref={records_ref}; total_records={}; returned_records={}; replacement_count={replacement_count}; truncated={}; next_after_sequence={:?}.\n",
        page.total_records, page.returned_records, page.truncated, page.next_after_sequence
    );
    for record in &page.records {
        text.push_str(&format!(
            "\n[content_replacement sequence={}; replacement_count={}]\n",
            record.sequence,
            record.replacements.len()
        ));
        for replacement in &record.replacements {
            text.push_str(&format!(
                "kind={}; tool_use_id={}\n{}\n",
                replacement.kind,
                replacement.tool_use_id,
                public_preview(
                    &replacement.replacement,
                    RUN_TRANSCRIPT_COMPACTION_MAX_EVENT_CHARS
                )
            ));
        }
    }
    let estimated_tokens = estimate_text_tokens(&text);
    Some(ContentReplacementTranscriptProjection {
        text,
        summary: json!({
            "contract": CONTENT_REPLACEMENT_REPLAY_CONTRACT,
            "source": "coder-server",
            "policy": "compact_transcript_tail_replay",
            "records_ref": records_ref,
            "record_count": page.total_records,
            "returned_count": page.returned_records,
            "replacement_count": replacement_count,
            "truncated": page.truncated,
            "next_after_sequence": page.next_after_sequence,
            "estimated_tokens": estimated_tokens,
            "limit": RUN_RESUME_CONTENT_REPLACEMENT_RECORD_LIMIT
        }),
    })
}

#[cfg(test)]
pub(crate) fn run_transcript_auto_compaction_decision(
    store: &RunStore,
    run_id: &RunId,
) -> Result<RunTranscriptAutoCompactionDecision, ApiError> {
    run_transcript_auto_compaction_decision_for_agent(store, run_id, None)
}

pub(crate) fn run_transcript_auto_compaction_decision_for_agent(
    store: &RunStore,
    run_id: &RunId,
    agent_id: Option<&str>,
) -> Result<RunTranscriptAutoCompactionDecision, ApiError> {
    let events = store.read_events(run_id)?;
    let runtime = resolve_run_transcript_runtime_policy(store, run_id, agent_id);
    let budget = context_budget_for_runtime(&runtime.policy);
    let scope_id = format!("run-transcript-{}", run_id.as_str());
    let circuit = store.read_compaction_circuit_state(&scope_id)?;
    let circuit_breaker_open = circuit
        .as_ref()
        .map(|state| state.circuit_breaker_open)
        .unwrap_or(false);
    let (effective_estimated_tokens, boundary_sequence, events_after_boundary) =
        effective_run_transcript_tokens_after_compaction_boundary(&events);
    let projected_tokens =
        effective_estimated_tokens.saturating_add(budget.estimated_max_turn_growth_tokens);
    let should_compact =
        !circuit_breaker_open && projected_tokens >= budget.autocompact_threshold_tokens;
    let reason = if circuit_breaker_open {
        "persistent_compaction_circuit_open"
    } else if should_compact {
        "projected_context_crosses_autocompact_threshold"
    } else {
        "below_autocompact_threshold"
    };
    Ok(RunTranscriptAutoCompactionDecision {
        should_compact,
        reason: reason.to_owned(),
        runtime_source: runtime.source.to_owned(),
        runtime_agent_id: runtime.agent_id,
        runtime_context_window_tokens: runtime.policy.context_window_tokens,
        runtime_auto_compact_token_limit: Some(runtime.policy.auto_compact_token_limit),
        effective_estimated_tokens,
        projected_tokens,
        threshold_tokens: budget.autocompact_threshold_tokens,
        blocking_limit_tokens: budget.blocking_limit_tokens,
        estimated_max_turn_growth_tokens: budget.estimated_max_turn_growth_tokens,
        event_count: events.len(),
        boundary_sequence,
        events_after_boundary,
        circuit_breaker_open,
    })
}

pub(crate) async fn maybe_auto_compact_run_transcript_attachment(
    state: ApiState,
    run_id: &RunId,
    preserved_results: &[ModelToolResultBlock],
    agent_id: Option<&str>,
) -> Option<Value> {
    let decision =
        run_transcript_auto_compaction_decision_for_agent(&state.store, run_id, agent_id).ok()?;
    if !decision.should_compact {
        return None;
    }
    let response = compact_run_transcript_for_run(
        state.clone(),
        run_id.clone(),
        RunTranscriptCompactionRequest {
            custom_instructions: Some(
                "Automatic model-loop compaction: preserve the current task, latest tool results, errors, fixes, pending work, and next action."
                    .to_owned(),
            ),
            scope_id: None,
            max_events: None,
        },
    )
    .await
    .ok()?;
    if !response.success {
        return None;
    }
    let post_compact_file_restore = post_compact_file_restore_payload(
        &state.store,
        run_id,
        decision.boundary_sequence,
        agent_id,
        preserved_results,
    );
    let post_compact_skill_restore = post_compact_skill_restore_payload(
        &state.store,
        run_id,
        decision.boundary_sequence,
        agent_id,
    );
    Some(run_transcript_compaction_attachment(
        &response,
        &decision,
        post_compact_file_restore,
        post_compact_skill_restore,
    ))
}

pub(crate) fn post_compact_restore_candidate_payload(
    canonical_tool_name: &str,
    tool_use_id: &str,
    tool_name: &str,
    input: &Value,
    response: &ModelToolExecuteResponse,
    agent_id: Option<&str>,
    harness_id: Option<&str>,
) -> Option<Value> {
    if response.is_error || response.status != "completed" {
        return None;
    }
    let path = match canonical_tool_name {
        "repo_read_file" => response
            .payload
            .get("file")
            .and_then(|file| file.get("path"))
            .and_then(Value::as_str),
        "repo_read_file_range" => response
            .payload
            .get("snippet")
            .and_then(|snippet| snippet.get("path"))
            .and_then(Value::as_str),
        _ => None,
    }?
    .trim();
    if path.is_empty() {
        return None;
    }
    let repo_root = response
        .payload
        .get("evidence_ref")
        .and_then(|reference| reference.get("repo_root"))
        .and_then(Value::as_str)
        .or_else(|| input.get("repo_root").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(json!({
        "contract": "coder.post_compact_restore_candidate.v1",
        "source": "coder-server",
        "kind": "repo_read_file",
        "tool_use_id": tool_use_id,
        "tool_name": tool_name,
        "canonical_tool_name": canonical_tool_name,
        "repo_root": repo_root,
        "path": path,
        "agent_id": agent_id,
        "harness_id": harness_id,
        "restore_strategy": "fresh_read_file_range_after_compaction"
    }))
}

fn effective_run_transcript_tokens_after_compaction_boundary(
    events: &[coder_events::CoderEvent],
) -> (u32, Option<u64>, usize) {
    let latest_success = events.iter().rev().find(|event| {
        event.kind == RUN_TRANSCRIPT_COMPACTION_EVENT_KIND
            && event.payload["success"].as_bool().unwrap_or(false)
    });
    let Some(boundary) = latest_success else {
        let projection =
            build_run_transcript_projection(events, RUN_TRANSCRIPT_COMPACTION_MAX_EVENTS);
        return (projection.estimated_tokens, None, events.len());
    };
    let summary_tokens = boundary.payload["summary_estimated_tokens"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_default();
    let events_after_boundary = events
        .iter()
        .filter(|event| event.sequence > boundary.sequence)
        .filter(|event| event.kind != RUN_TRANSCRIPT_COMPACTION_EVENT_KIND)
        .cloned()
        .collect::<Vec<_>>();
    let projection = build_run_transcript_projection(
        &events_after_boundary,
        RUN_TRANSCRIPT_COMPACTION_MAX_EVENTS,
    );
    (
        summary_tokens.saturating_add(projection.estimated_tokens),
        Some(boundary.sequence),
        events_after_boundary.len(),
    )
}

#[derive(Debug, Clone)]
struct RunTranscriptRuntimeResolution {
    policy: ResolvedAgentRuntimePolicy,
    source: &'static str,
    agent_id: Option<String>,
}

fn resolve_run_transcript_runtime_policy(
    store: &RunStore,
    run_id: &RunId,
    requested_agent_id: Option<&str>,
) -> RunTranscriptRuntimeResolution {
    let requested_agent_id = normalize_runtime_agent_id(requested_agent_id);
    let inferred_agent_id =
        requested_agent_id.or_else(|| latest_node_started_agent_id(store, run_id));
    let Some(agent_id) = inferred_agent_id else {
        return RunTranscriptRuntimeResolution {
            policy: default_resolved_agent_runtime(),
            source: "default_runtime_no_agent_id",
            agent_id: None,
        };
    };

    let Some(config) = read_run_project_config_snapshot(store, run_id) else {
        return RunTranscriptRuntimeResolution {
            policy: default_resolved_agent_runtime(),
            source: "default_runtime_missing_config",
            agent_id: Some(agent_id),
        };
    };

    let Some(profile) = config.task_profiles.get(&agent_id) else {
        return RunTranscriptRuntimeResolution {
            policy: default_resolved_agent_runtime(),
            source: "default_runtime_missing_task_profile",
            agent_id: Some(agent_id),
        };
    };

    let model = config
        .models
        .get(&profile.model)
        .cloned()
        .unwrap_or_else(default_compaction_model);
    RunTranscriptRuntimeResolution {
        policy: resolve_agent_runtime_policy(&model, &profile.runtime),
        source: "run_config_task_profile_runtime",
        agent_id: Some(agent_id),
    }
}

fn default_resolved_agent_runtime() -> ResolvedAgentRuntimePolicy {
    resolve_agent_runtime_policy(&default_compaction_model(), &AgentRuntimePolicy::default())
}

fn default_compaction_model() -> ModelSpec {
    ModelSpec {
        provider: "openai-compatible".to_owned(),
        model: "default".to_owned(),
        base_url_env: None,
        api_key_env: None,
        capabilities: ModelCapabilities::default(),
    }
}

fn normalize_runtime_agent_id(agent_id: Option<&str>) -> Option<String> {
    agent_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn read_run_project_config_snapshot(store: &RunStore, run_id: &RunId) -> Option<ProjectConfig> {
    store
        .read_run_config_snapshot_json(run_id)
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn latest_node_started_agent_id(store: &RunStore, run_id: &RunId) -> Option<String> {
    let page = store
        .read_events_page(run_id, DurableJsonlPageOptions::tail(1000).ok()?)
        .ok()?;
    page.records.iter().rev().find_map(|event| {
        if event.kind != "node.started" {
            return None;
        }
        event
            .payload
            .get("agent")
            .or_else(|| event.payload.get("agent_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|agent_id| !agent_id.is_empty())
            .map(str::to_owned)
    })
}

#[derive(Debug, Clone)]
struct PostCompactRestoreCandidate {
    repo_root: String,
    path: String,
    sequence: u64,
    tool_use_id: String,
    tool_name: String,
    canonical_tool_name: String,
}

#[derive(Debug, Clone)]
struct PostCompactSkillCandidate {
    skill_name: String,
    skill_path: String,
    content: String,
    sequence: u64,
    content_truncated: bool,
}

fn post_compact_file_restore_payload(
    store: &RunStore,
    run_id: &RunId,
    boundary_sequence: Option<u64>,
    agent_id: Option<&str>,
    preserved_results: &[ModelToolResultBlock],
) -> Option<Value> {
    let events = store.read_events(run_id).ok()?;
    let preserved_keys = preserved_model_tool_read_keys(preserved_results);
    let mut seen_keys = BTreeSet::new();
    let mut candidates = Vec::new();
    for event in events.iter().rev() {
        if boundary_sequence
            .map(|boundary| event.sequence <= boundary)
            .unwrap_or(false)
        {
            break;
        }
        let Some(candidate) = post_compact_restore_candidate_from_event(event, agent_id) else {
            continue;
        };
        let key = post_compact_file_key(&candidate.repo_root, &candidate.path);
        if preserved_keys.contains(&key) || !seen_keys.insert(key) {
            continue;
        }
        candidates.push(candidate);
        if candidates.len() >= POST_COMPACT_MAX_FILES_TO_RESTORE {
            break;
        }
    }

    let mut used_tokens = 0_u32;
    let mut restored_files = Vec::new();
    let mut model_sections = Vec::new();
    for candidate in candidates {
        let snippet = match read_file_range(
            &candidate.repo_root,
            PathBuf::from(&candidate.path),
            1,
            200,
            POST_COMPACT_MAX_CHARS_PER_FILE,
        ) {
            Ok(snippet) => snippet,
            Err(_) => continue,
        };
        let model_text = post_compact_restored_file_model_text(&candidate, &snippet);
        let estimated_tokens = estimate_text_tokens(&model_text);
        if used_tokens.saturating_add(estimated_tokens) > POST_COMPACT_TOKEN_BUDGET {
            continue;
        }
        used_tokens = used_tokens.saturating_add(estimated_tokens);
        model_sections.push(model_text);
        restored_files.push(json!({
            "path": snippet.path,
            "repo_root": candidate.repo_root,
            "start_line": snippet.start_line,
            "end_line": snippet.end_line,
            "truncated": snippet.truncated,
            "estimated_tokens": estimated_tokens,
            "source_sequence": candidate.sequence,
            "source_tool_use_id": candidate.tool_use_id,
            "source_tool_name": candidate.tool_name,
            "source_canonical_tool_name": candidate.canonical_tool_name
        }));
    }

    if restored_files.is_empty() {
        return None;
    }
    let restored_count = restored_files.len();
    let prompt = format!(
        "<system-reminder>\nRecent files restored after context compaction. These files were freshly re-read using Claude Code style post-compact limits: max_files={POST_COMPACT_MAX_FILES_TO_RESTORE}, total_token_budget={POST_COMPACT_TOKEN_BUDGET}, max_tokens_per_file={POST_COMPACT_MAX_TOKENS_PER_FILE}.\n\n{}\n</system-reminder>",
        model_sections.join("\n\n")
    );
    Some(json!({
        "contract": POST_COMPACT_FILE_RESTORE_CONTRACT,
        "source": "coder-server",
        "type": "post_compact_file_restore",
        "run_id": run_id.as_str(),
        "status": "completed",
        "restored_file_count": restored_count,
        "restored_files": restored_files,
        "used_tokens": used_tokens,
        "max_files": POST_COMPACT_MAX_FILES_TO_RESTORE,
        "token_budget": POST_COMPACT_TOKEN_BUDGET,
        "max_tokens_per_file": POST_COMPACT_MAX_TOKENS_PER_FILE,
        "max_chars_per_file": POST_COMPACT_MAX_CHARS_PER_FILE,
        "boundary_sequence": boundary_sequence,
        "agent_id": agent_id,
        "model_content": [
            {
                "type": "text",
                "text": prompt
            }
        ]
    }))
}

fn post_compact_skill_restore_payload(
    store: &RunStore,
    run_id: &RunId,
    boundary_sequence: Option<u64>,
    agent_id: Option<&str>,
) -> Option<Value> {
    let events = store.read_events(run_id).ok()?;
    let mut seen_skills = BTreeSet::new();
    let mut candidates = Vec::new();
    for event in events.iter().rev() {
        if boundary_sequence
            .map(|boundary| event.sequence <= boundary)
            .unwrap_or(false)
        {
            break;
        }
        let Some(candidate) = post_compact_skill_candidate_from_event(event, agent_id) else {
            continue;
        };
        let key = normalize_post_compact_path_key(&candidate.skill_name);
        if !seen_skills.insert(key) {
            continue;
        }
        candidates.push(candidate);
    }

    let mut used_tokens = 0_u32;
    let mut restored_skills = Vec::new();
    let mut model_sections = Vec::new();
    for candidate in candidates {
        let model_text = post_compact_restored_skill_model_text(&candidate);
        let estimated_tokens = estimate_text_tokens(&model_text);
        if used_tokens.saturating_add(estimated_tokens) > POST_COMPACT_SKILLS_TOKEN_BUDGET {
            continue;
        }
        used_tokens = used_tokens.saturating_add(estimated_tokens);
        model_sections.push(model_text);
        restored_skills.push(json!({
            "name": candidate.skill_name,
            "path": candidate.skill_path,
            "estimated_tokens": estimated_tokens,
            "source_sequence": candidate.sequence,
            "content_truncated": candidate.content_truncated
        }));
    }

    if restored_skills.is_empty() {
        return None;
    }
    let restored_count = restored_skills.len();
    let prompt = format!(
        "<system-reminder>\nInvoked skills restored after context compaction. These skills were restored using Claude Code style post-compact limits: total_skill_budget={POST_COMPACT_SKILLS_TOKEN_BUDGET}, max_tokens_per_skill={POST_COMPACT_MAX_TOKENS_PER_SKILL}.\n\n{}\n</system-reminder>",
        model_sections.join("\n\n")
    );
    Some(json!({
        "contract": "coder.post_compact_skill_restore.v1",
        "source": "coder-server",
        "type": "invoked_skills",
        "run_id": run_id.as_str(),
        "status": "completed",
        "restored_skill_count": restored_count,
        "skills": restored_skills,
        "used_tokens": used_tokens,
        "token_budget": POST_COMPACT_SKILLS_TOKEN_BUDGET,
        "max_tokens_per_skill": POST_COMPACT_MAX_TOKENS_PER_SKILL,
        "max_chars_per_skill": POST_COMPACT_MAX_CHARS_PER_SKILL,
        "boundary_sequence": boundary_sequence,
        "agent_id": agent_id,
        "model_content": [
            {
                "type": "text",
                "text": prompt
            }
        ]
    }))
}

fn post_compact_restore_candidate_from_event(
    event: &coder_events::CoderEvent,
    agent_id: Option<&str>,
) -> Option<PostCompactRestoreCandidate> {
    if event.kind != "model_tool.phase"
        || event.payload["phase"].as_str() != Some("tool_execution")
        || event.payload["status"].as_str() != Some("completed")
    {
        return None;
    }
    let candidate = event.payload.get("post_compact_restore_candidate")?;
    if !post_compact_agent_matches(agent_id, candidate.get("agent_id").and_then(Value::as_str)) {
        return None;
    }
    let repo_root = candidate
        .get("repo_root")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    let path = candidate
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    Some(PostCompactRestoreCandidate {
        repo_root,
        path,
        sequence: event.sequence,
        tool_use_id: candidate
            .get("tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        tool_name: candidate
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        canonical_tool_name: candidate
            .get("canonical_tool_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

fn post_compact_skill_candidate_from_event(
    event: &coder_events::CoderEvent,
    agent_id: Option<&str>,
) -> Option<PostCompactSkillCandidate> {
    if event.kind != INVOKED_SKILL_EVENT_KIND
        || event.payload["contract"].as_str() != Some(INVOKED_SKILL_CONTRACT)
    {
        return None;
    }
    if !post_compact_agent_matches(
        agent_id,
        event.payload.get("agent_id").and_then(Value::as_str),
    ) {
        return None;
    }
    let skill_name = event
        .payload
        .get("skill_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    let skill_path = event
        .payload
        .get("skill_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    let raw_content = event.payload.get("content").and_then(Value::as_str)?;
    let (content, truncated_for_restore) =
        truncate_text_to_chars(raw_content, POST_COMPACT_MAX_CHARS_PER_SKILL);
    Some(PostCompactSkillCandidate {
        skill_name,
        skill_path,
        content,
        sequence: event.sequence,
        content_truncated: event.payload["content_truncated"]
            .as_bool()
            .unwrap_or(false)
            || truncated_for_restore,
    })
}

fn post_compact_agent_matches(expected: Option<&str>, actual: Option<&str>) -> bool {
    match (expected, actual) {
        (Some(expected), Some(actual)) => expected == actual,
        (None, None) => true,
        _ => false,
    }
}

fn preserved_model_tool_read_keys(results: &[ModelToolResultBlock]) -> BTreeSet<String> {
    results
        .iter()
        .filter_map(|result| {
            let repo_root = result
                .payload
                .get("evidence_ref")
                .and_then(|reference| reference.get("repo_root"))
                .and_then(Value::as_str)?;
            let path = result
                .payload
                .get("file")
                .and_then(|file| file.get("path"))
                .and_then(Value::as_str)
                .or_else(|| {
                    result
                        .payload
                        .get("snippet")
                        .and_then(|snippet| snippet.get("path"))
                        .and_then(Value::as_str)
                })?;
            Some(post_compact_file_key(repo_root, path))
        })
        .collect()
}

fn post_compact_file_key(repo_root: &str, path: &str) -> String {
    format!(
        "{}\n{}",
        normalize_post_compact_path_key(repo_root),
        normalize_post_compact_path_key(path)
    )
}

fn normalize_post_compact_path_key(value: &str) -> String {
    let normalized = value.trim().replace('\\', "/");
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn post_compact_restored_file_model_text(
    candidate: &PostCompactRestoreCandidate,
    snippet: &RepoReadSnippet,
) -> String {
    format!(
        "<file path=\"{}\" repo_root=\"{}\" start_line=\"{}\" end_line=\"{}\" truncated=\"{}\" source_tool_use_id=\"{}\">\n{}\n</file>",
        snippet.path,
        candidate.repo_root,
        snippet.start_line,
        snippet.end_line,
        snippet.truncated,
        candidate.tool_use_id,
        snippet.text
    )
}

fn post_compact_restored_skill_model_text(candidate: &PostCompactSkillCandidate) -> String {
    format!(
        "<skill name=\"{}\" path=\"{}\" truncated=\"{}\" source_sequence=\"{}\">\n{}\n</skill>",
        candidate.skill_name,
        candidate.skill_path,
        candidate.content_truncated,
        candidate.sequence,
        candidate.content
    )
}

fn run_transcript_compaction_attachment(
    response: &RunTranscriptCompactionResponse,
    decision: &RunTranscriptAutoCompactionDecision,
    post_compact_file_restore: Option<Value>,
    post_compact_skill_restore: Option<Value>,
) -> Value {
    let summary = response.summary.as_deref().unwrap_or("").trim();
    let artifact_ref = response.artifact_ref.as_deref().unwrap_or("");
    let prompt = format!(
        "<system-reminder>\nCoder automatically compacted the run transcript because projected context crossed the autocompact threshold. Use this compacted context for continuity instead of relying on older raw transcript details.\n\nartifact_ref: {artifact_ref}\nthreshold_tokens: {}\nprojected_tokens: {}\nboundary_sequence: {}\n\n{summary}\n</system-reminder>",
        decision.threshold_tokens,
        decision.projected_tokens,
        decision
            .boundary_sequence
            .map(|sequence| sequence.to_string())
            .unwrap_or_else(|| "none".to_owned())
    );
    let mut model_content = vec![json!({
        "type": "text",
        "text": prompt
    })];
    if let Some(restore) = post_compact_file_restore.as_ref() {
        if let Some(blocks) = restore.get("model_content").and_then(Value::as_array) {
            model_content.extend(blocks.iter().cloned());
        }
    }
    if let Some(restore) = post_compact_skill_restore.as_ref() {
        if let Some(blocks) = restore.get("model_content").and_then(Value::as_array) {
            model_content.extend(blocks.iter().cloned());
        }
    }
    json!({
        "contract": RUN_TRANSCRIPT_COMPACTION_ATTACHMENT_CONTRACT,
        "source": "coder-server",
        "type": "context_compaction_summary",
        "run_id": &response.run_id,
        "status": &response.status,
        "success": response.success,
        "artifact_ref": &response.artifact_ref,
        "event_sequence": response.event_sequence,
        "summary_estimated_tokens": response.summary_estimated_tokens,
        "decision": {
            "reason": decision.reason,
            "runtime_source": decision.runtime_source,
            "runtime_agent_id": decision.runtime_agent_id,
            "runtime_context_window_tokens": decision.runtime_context_window_tokens,
            "runtime_auto_compact_token_limit": decision.runtime_auto_compact_token_limit,
            "effective_estimated_tokens": decision.effective_estimated_tokens,
            "projected_tokens": decision.projected_tokens,
            "threshold_tokens": decision.threshold_tokens,
            "blocking_limit_tokens": decision.blocking_limit_tokens,
            "estimated_max_turn_growth_tokens": decision.estimated_max_turn_growth_tokens,
            "event_count": decision.event_count,
            "boundary_sequence": decision.boundary_sequence,
            "events_after_boundary": decision.events_after_boundary,
            "circuit_breaker_open": decision.circuit_breaker_open
        },
        "post_compact_file_restore": post_compact_file_restore,
        "post_compact_skill_restore": post_compact_skill_restore,
        "model_content": model_content
    })
}

async fn request_run_transcript_compaction_summary(
    state: &ApiState,
    run_id: &RunId,
    settings: &ProviderSettings,
    transcript_text: &str,
    request: &RunTranscriptCompactionRequest,
) -> Result<RunTranscriptCompactionModelOutput, String> {
    if check_existing_run_token_budget(state, run_id).is_some_and(|budget| budget.exhausted()) {
        return Err(
            "Workflow token budget is exhausted; transcript compaction was not sent.".to_owned(),
        );
    }
    if settings.mock_mode {
        return Err(
            "Provider mock mode is enabled; transcript compaction requires a live provider."
                .to_owned(),
        );
    }
    let provider = normalize_provider(&settings.default_provider);
    if provider.is_empty() {
        return Err("Transcript compaction requires a default provider.".to_owned());
    }
    let model = settings.default_model.trim().to_owned();
    if model.is_empty() {
        return Err("Transcript compaction requires a default model.".to_owned());
    }
    let base_url = provider_base_url(settings, &provider)
        .ok_or_else(|| "Transcript compaction requires a provider base URL.".to_owned())?;
    let url = provider_chat_completions_endpoint(&base_url);
    let endpoint = provider_chat_completions_endpoint_for_display(&base_url);
    let proxy_url = provider_proxy_url_for_url(settings, &provider, Some(&url));
    let api_key = provider_api_key(settings, &provider, None);
    if provider != "ollama" && api_key.is_none() {
        return Err(
            "Transcript compaction requires an API key from Provider Settings or environment."
                .to_owned(),
        );
    }
    let api_secret = api_key
        .as_ref()
        .map(|(secret, _)| secret.as_str())
        .unwrap_or("");
    let client = provider_http_client_builder(settings, &provider, &url)
        .map_err(|error| {
            redact_provider_error(
                &error,
                &[api_secret, &base_url, proxy_url.as_deref().unwrap_or("")],
            )
        })?
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|error| {
            redact_provider_error(
                &error.to_string(),
                &[api_secret, &base_url, proxy_url.as_deref().unwrap_or("")],
            )
        })?;
    let request_body = transcript_compaction_chat_completion_body(
        &provider,
        &model,
        transcript_compaction_prompt(transcript_text, request.custom_instructions.as_deref()),
    );
    let response = send_provider_request_with_retry(
        || {
            let builder = client.post(&url).json(&request_body);
            if let Some((secret, _)) = api_key.as_ref() {
                builder.bearer_auth(secret)
            } else {
                builder
            }
        },
        None,
        provider_request_max_retries(settings, &provider),
    )
    .await
    .map_err(|error| {
        redact_provider_error(
            &format!("transcript compaction model request failed: {error}"),
            &[api_secret, &base_url, proxy_url.as_deref().unwrap_or("")],
        )
    })?
    .response;
    if !response.status().is_success() {
        return Err(format!(
            "transcript compaction model returned HTTP {}",
            response.status()
        ));
    }
    let payload: Value = response.json().await.map_err(|error| {
        redact_provider_error(
            &error.to_string(),
            &[api_secret, &base_url, proxy_url.as_deref().unwrap_or("")],
        )
    })?;
    record_existing_run_token_usage(state, run_id, provider_token_usage(&request_body, &payload));
    let raw_summary = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "Provider response did not include assistant summary content.".to_owned())?;
    Ok(RunTranscriptCompactionModelOutput {
        provider,
        model,
        endpoint,
        raw_summary,
    })
}

fn transcript_compaction_prompt(
    transcript_text: &str,
    custom_instructions: Option<&str>,
) -> String {
    let mut prompt = String::from(
        "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.\n\n\
- Do NOT use Read, Bash, Grep, Glob, Edit, Write, or ANY other tool.\n\
- You already have all the context you need in the transcript below.\n\
- Tool calls will be rejected and will waste your only turn; you will fail the task.\n\
- Your entire response must be plain text: an <analysis> block followed by a <summary> block.\n\n\
Your task is to create a detailed summary of the run transcript so development can continue without losing context.\n\
Before the final summary, use <analysis> tags to check chronology, user intent, key decisions, files, code, errors, fixes, current work, and pending tasks.\n\
The stored output must be inside <summary> tags and include:\n\
1. Primary Request and Intent\n\
2. Key Technical Concepts\n\
3. Files and Code Sections\n\
4. Errors and fixes\n\
5. Problem Solving\n\
6. All user messages\n\
7. Pending Tasks\n\
8. Current Work\n\
9. Optional Next Step\n",
    );
    if let Some(custom) = custom_instructions
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        prompt.push_str("\nAdditional Instructions:\n");
        prompt.push_str(custom);
        prompt.push('\n');
    }
    prompt.push_str(
        "\nREMINDER: Do NOT call any tools. Respond with plain text only: an <analysis> block followed by a <summary> block.\n\nTranscript:\n",
    );
    prompt.push_str(transcript_text);
    prompt
}

fn transcript_compaction_chat_completion_body(
    provider: &str,
    model_name: &str,
    prompt: String,
) -> Value {
    let mut body = json!({
        "model": model_name,
        "messages": [
            {
                "role": "system",
                "content": "You are a helpful AI assistant tasked with summarizing conversations."
            },
            {
                "role": "user",
                "content": prompt
            }
        ],
        "temperature": 0,
        "max_tokens": RUN_TRANSCRIPT_COMPACTION_MAX_OUTPUT_TOKENS
    });
    if normalize_provider(provider) == "deepseek" {
        body["thinking"] = json!({"type": "disabled"});
    }
    body
}

fn format_compact_summary(summary: &str) -> String {
    let without_analysis = strip_xml_block(summary, "<analysis>", "</analysis>");
    if let Some(summary_body) = extract_xml_block(&without_analysis, "<summary>", "</summary>") {
        format!("Summary:\n{}", summary_body.trim())
    } else {
        without_analysis.trim().to_owned()
    }
}

fn strip_xml_block(value: &str, start_tag: &str, end_tag: &str) -> String {
    let Some(start) = value.find(start_tag) else {
        return value.to_owned();
    };
    let Some(relative_end) = value[start + start_tag.len()..].find(end_tag) else {
        return value.to_owned();
    };
    let end = start + start_tag.len() + relative_end + end_tag.len();
    let mut output = String::new();
    output.push_str(&value[..start]);
    output.push_str(&value[end..]);
    output
}

fn extract_xml_block(value: &str, start_tag: &str, end_tag: &str) -> Option<String> {
    let start = value.find(start_tag)? + start_tag.len();
    let end = value[start..].find(end_tag)? + start;
    Some(value[start..end].to_owned())
}

struct FailedTranscriptCompactionContext<'a> {
    store: &'a RunStore,
    run_id: &'a RunId,
    scope_id: &'a str,
    max_consecutive_failures: u8,
    events: &'a [coder_events::CoderEvent],
    max_events: usize,
    transcript_estimated_tokens: u32,
}

fn record_failed_run_transcript_compaction(
    context: &FailedTranscriptCompactionContext<'_>,
    provider: Option<String>,
    model: Option<String>,
    endpoint: Option<String>,
    error: String,
) -> Result<RunTranscriptCompactionResponse, ApiError> {
    let circuit_state = context.store.record_compaction_circuit_outcome(
        context.scope_id,
        context.max_consecutive_failures,
        false,
    )?;
    let projection = build_run_transcript_projection(context.events, context.max_events);
    let mut response = transcript_compaction_response_base(TranscriptCompactionResponseInput {
        run_id: context.run_id,
        status: "failed",
        success: false,
        provider,
        model,
        endpoint,
        summary: None,
        artifact_ref: None,
        events: context.events,
        max_events: context.max_events,
        transcript_estimated_tokens: context
            .transcript_estimated_tokens
            .max(projection.estimated_tokens),
        event_sequence: None,
        error: Some(error),
        circuit: transcript_compaction_circuit_response(
            context.scope_id,
            context.max_consecutive_failures,
            Some(&circuit_state),
        ),
    });
    response.event_sequence = Some(append_run_transcript_compaction_event(
        context.store,
        context.run_id,
        transcript_compaction_event_payload(&response),
        None,
    )?);
    Ok(response)
}

struct TranscriptCompactionResponseInput<'a> {
    run_id: &'a RunId,
    status: &'a str,
    success: bool,
    provider: Option<String>,
    model: Option<String>,
    endpoint: Option<String>,
    summary: Option<String>,
    artifact_ref: Option<String>,
    events: &'a [coder_events::CoderEvent],
    max_events: usize,
    transcript_estimated_tokens: u32,
    event_sequence: Option<u64>,
    error: Option<String>,
    circuit: RunTranscriptCompactionCircuitResponse,
}

fn transcript_compaction_response_base(
    input: TranscriptCompactionResponseInput<'_>,
) -> RunTranscriptCompactionResponse {
    let TranscriptCompactionResponseInput {
        run_id,
        status,
        success,
        provider,
        model,
        endpoint,
        summary,
        artifact_ref,
        events,
        max_events,
        transcript_estimated_tokens,
        event_sequence,
        error,
        circuit,
    } = input;
    let event_count = events.len();
    let omitted = event_count.saturating_sub(max_events);
    let included = event_count.saturating_sub(omitted);
    let summary_estimated_tokens = summary
        .as_deref()
        .map(estimate_text_tokens)
        .unwrap_or_default();
    RunTranscriptCompactionResponse {
        contract: RUN_TRANSCRIPT_COMPACTION_CONTRACT,
        source: "coder-server",
        policy: "claude_style_model_summary_with_persistent_circuit",
        run_id: run_id.to_string(),
        status: status.to_owned(),
        success,
        provider,
        model,
        endpoint,
        summary,
        summary_estimated_tokens,
        transcript_event_count: event_count,
        transcript_events_included: included,
        transcript_events_omitted: omitted,
        transcript_truncated: omitted > 0,
        transcript_estimated_tokens,
        artifact_ref,
        event_sequence,
        error,
        circuit,
    }
}

fn transcript_compaction_circuit_response(
    scope_id: &str,
    max_consecutive_failures: u8,
    state: Option<&CompactionCircuitState>,
) -> RunTranscriptCompactionCircuitResponse {
    RunTranscriptCompactionCircuitResponse {
        scope_id: state
            .map(|state| state.scope_id.clone())
            .unwrap_or_else(|| scope_id.to_owned()),
        max_consecutive_failures: state
            .map(|state| state.max_consecutive_failures)
            .unwrap_or(max_consecutive_failures),
        consecutive_failures: state
            .map(|state| state.consecutive_failures)
            .unwrap_or_default(),
        circuit_breaker_open: state
            .map(|state| state.circuit_breaker_open)
            .unwrap_or(false),
        updated_at: state.map(|state| state.updated_at.to_string()),
    }
}

fn transcript_compaction_event_payload(response: &RunTranscriptCompactionResponse) -> Value {
    json!({
        "contract": response.contract,
        "source": response.source,
        "policy": response.policy,
        "status": &response.status,
        "success": response.success,
        "provider": &response.provider,
        "model": &response.model,
        "endpoint": &response.endpoint,
        "summary_estimated_tokens": response.summary_estimated_tokens,
        "transcript_event_count": response.transcript_event_count,
        "transcript_events_included": response.transcript_events_included,
        "transcript_events_omitted": response.transcript_events_omitted,
        "transcript_truncated": response.transcript_truncated,
        "transcript_estimated_tokens": response.transcript_estimated_tokens,
        "artifact_ref": &response.artifact_ref,
        "error": &response.error,
        "circuit": &response.circuit
    })
}

fn append_run_transcript_compaction_event(
    store: &RunStore,
    run_id: &RunId,
    payload: Value,
    artifact_ref: Option<String>,
) -> Result<u64, ApiError> {
    let sequence = store.event_count(run_id)? as u64 + 1;
    let mut event = coder_events::CoderEvent::new(
        run_id.clone(),
        sequence,
        RUN_TRANSCRIPT_COMPACTION_EVENT_KIND,
        payload,
    );
    if let Some(artifact_ref) = artifact_ref {
        event = event.with_ref("transcript_summary", artifact_ref);
    }
    store.append_event(run_id, &event)?;
    Ok(sequence)
}

pub(crate) async fn list_run_content_replacements(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    Query(query): Query<RunEventsQuery>,
) -> Result<Json<RunContentReplacementsResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let limit = validated_jsonl_page_limit(query.limit.unwrap_or(200))?;
    let options = if query.tail {
        DurableJsonlPageOptions::tail(limit)?
    } else {
        DurableJsonlPageOptions::with_after_sequence(query.after_sequence, limit)?
    };
    let response = run_content_replacements_response(
        &state.store,
        &run_id,
        options,
        if query.tail {
            "tail_page"
        } else {
            "incremental_page"
        },
    )?;
    if response.record_count == 0 && !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    Ok(Json(response))
}

pub(crate) fn validated_jsonl_page_limit(limit: usize) -> Result<usize, ApiError> {
    if limit == 0 || limit > MAX_DURABLE_JSONL_PAGE_LIMIT {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {MAX_DURABLE_JSONL_PAGE_LIMIT}"
        )));
    }
    Ok(limit)
}

pub(crate) fn run_content_replacements_response(
    store: &RunStore,
    run_id: &RunId,
    options: DurableJsonlPageOptions,
    policy: &'static str,
) -> Result<RunContentReplacementsResponse, ApiError> {
    let page = store.read_run_content_replacement_records_page(run_id, options)?;
    let replacement_count = page
        .records
        .iter()
        .map(|record| record.replacements.len())
        .sum();
    Ok(RunContentReplacementsResponse {
        contract: CONTENT_REPLACEMENT_REPLAY_CONTRACT,
        source: "coder-server",
        policy,
        run_id: run_id.to_string(),
        records_ref: format!(
            "content-replacements://runs/{}/content-replacements.jsonl",
            run_id.as_str()
        ),
        records_url: format!("/api/v3/runs/{}/content-replacements", run_id.as_str()),
        records: page.records,
        record_count: page.total_records,
        returned_count: page.returned_records,
        replacement_count,
        truncated: page.truncated,
        next_after_sequence: page.next_after_sequence,
    })
}

pub(crate) fn run_content_replacement_replay_summary(
    response: &RunContentReplacementsResponse,
) -> Value {
    json!({
        "contract": response.contract,
        "source": response.source,
        "policy": response.policy,
        "records_ref": response.records_ref,
        "records_url": response.records_url,
        "record_count": response.record_count,
        "returned_count": response.returned_count,
        "replacement_count": response.replacement_count,
        "truncated": response.truncated,
        "next_after_sequence": response.next_after_sequence
    })
}
