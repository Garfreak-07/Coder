#[cfg(test)]
use std::env;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
#[cfg(test)]
use coder_config::PermissionSettingsRecord;
#[cfg(test)]
use coder_config::ProjectConfig;
use coder_config::{ValidationLevel, ValidationReport};
#[cfg(test)]
use coder_core::FinalReport;
#[cfg(test)]
use coder_core::RunId;
#[cfg(test)]
use coder_core::RunStatus;
use coder_memory::MemoryError;
#[cfg(test)]
use coder_store::RepoEvidenceKind;
use coder_store::{RunStore, StoreError};
use coder_tools::RepoToolError;
use coder_workflow::WorkflowError;
#[cfg(test)]
use coder_workflow::WorkflowRunControl;
use serde_json::{json, Value};

mod api_types;
mod background_commands;
mod cache_runtime;
mod capability_registry;
mod change_sets;
mod code_task_runtime;
mod conversation;
mod conversation_provider;
mod conversation_runtime;
mod credential_store;
mod extension_endpoints;
mod local_api_transport;
mod mcp_runtime;
mod memory_endpoints;
mod model_tool_agent_hooks;
mod model_tool_async_attachments;
mod model_tool_background_tasks;
mod model_tool_builtin_operations;
mod model_tool_command_hooks;
mod model_tool_dispatch;
mod model_tool_execute_pipeline;
mod model_tool_execution;
mod model_tool_hook_output;
mod model_tool_hook_phase;
mod model_tool_hook_runtime;
mod model_tool_input;
mod model_tool_permissions;
mod model_tool_phase;
mod model_tool_prompt_hooks;
mod model_tool_response;
mod model_tool_result_storage;
mod model_tool_run_context;
mod model_tool_server_executor;
mod model_tool_skill_context;
mod model_tool_skill_execution;
mod model_tool_webhook_hooks;
mod native_model_backend;
mod native_model_mcp;
mod outbound_http;
mod output_hub;
mod provider_runtime;
mod provider_settings;
mod run_control;
mod run_permission_updates;
mod run_records;
mod run_reports;
mod run_token_budget;
mod run_transcript_compaction;
mod session_host;
mod skill_model_tool;
mod subagent_tools;
mod surface_endpoints;
mod timeline_projection;
mod tool_endpoints;
mod workflow_endpoints;
use api_types::InstalledSkillRecord;
pub use api_types::*;
use background_commands::{
    cancel_background_command_endpoint, get_background_command_endpoint,
    get_background_command_output_endpoint, start_background_command_endpoint,
    write_background_command_stdin_endpoint, BackgroundCommandTask,
};
use extension_endpoints::{
    add_plugin_marketplace, add_skill_extra_root, auto_update_skills, developer_import_skill,
    disable_skill, discover_skills_endpoint, enable_skill, install_skill, list_extension_plugins,
    list_extension_skills, list_extensions_installed, list_hooks, list_installed_plugins,
    list_installed_skills, list_plugin_marketplaces, list_plugins, list_skill_extra_roots,
    list_skill_updates, pin_skill, read_plugin, read_plugin_skill, record_invoked_skill,
    remove_plugin_marketplace, remove_skill, rollback_skill, search_extensions_endpoint,
    set_skill_update_policy, unpin_skill, update_skill, upgrade_plugin_marketplace,
    validate_extension_plugin, validate_extension_skill,
};
use memory_endpoints::{
    confirm_project_memory_write, import_knowledge_text, list_knowledge_source_chunks,
    list_knowledge_sources, load_project_memory, propose_project_memory_write, retrieve_knowledge,
};
use model_tool_execution::{execute_model_tool_endpoint, execute_model_tool_turn_endpoint};
use provider_settings::{
    apply_provider_settings_to_project_config, get_provider_settings, get_provider_status,
    load_provider_settings, save_provider_settings, test_provider_status,
};
pub(crate) use run_records::stored_run_exists;
use run_records::{
    drain_run_async_notifications, get_blob_sha256, get_repo_evidence, get_run_artifact,
    get_run_checkpoint, get_run_detail, list_run_async_notifications, list_run_checkpoints,
    list_run_events, list_run_repo_evidence, list_run_timeline, list_runs, write_run_checkpoint,
};
use run_transcript_compaction::{compact_run_transcript, list_run_content_replacements};
use subagent_tools::{
    cancel_background_subagent_endpoint, get_background_subagent_endpoint, run_subagent_endpoint,
    BackgroundSubagentTask,
};
use surface_endpoints::{capabilities, health};
use tool_endpoints::{
    apply_patch_endpoint, ensure_tool_boundary, git_diff_endpoint, git_status_endpoint,
    preview_command_endpoint, preview_patch_endpoint, record_command_events,
    repo_find_files_endpoint, repo_read_file_endpoint, repo_read_file_range_endpoint,
    repo_search_text_endpoint, run_command_endpoint, write_tool_evidence,
};
pub(crate) use workflow_endpoints::default_project_config;
pub use workflow_endpoints::run_embedded_workflow;
use workflow_endpoints::{preview_run, run_workflow, validate_config};

pub(crate) fn validation_issue_summary(report: &ValidationReport) -> String {
    report
        .issues
        .iter()
        .filter(|issue| issue.level == ValidationLevel::Error)
        .take(3)
        .map(|issue| format!("{} at {}", issue.code, issue.target))
        .collect::<Vec<_>>()
        .join("; ")
}

// Claude Code caps per-session transcript write queues at 1000 entries. Coder
// applies the same bounded-queue size to retained run transcript slices so
// event capture cannot grow without limit during long agent runs.
const CLAUDE_CODE_BOUNDED_QUEUE_ENTRIES: usize = 1000;
const CONTENT_REPLACEMENT_REPLAY_CONTRACT: &str = "coder.content_replacement_replay.v1";
const RUN_TRANSCRIPT_COMPACTION_CONTRACT: &str = "coder.run_transcript_compaction.v1";
const RUN_TRANSCRIPT_COMPACTION_ATTACHMENT_CONTRACT: &str =
    "coder.run_transcript_compaction_attachment.v1";
const RUN_TRANSCRIPT_COMPACTION_EVENT_KIND: &str = "run.transcript_compaction.outcome";
const RUN_RESUME_CONTENT_REPLACEMENT_RECORD_LIMIT: usize = 100;
const RUN_TRANSCRIPT_COMPACTION_MAX_EVENTS: usize = CLAUDE_CODE_BOUNDED_QUEUE_ENTRIES;
const RUN_TRANSCRIPT_COMPACTION_MAX_EVENT_CHARS: usize = 4_000;
const RUN_TRANSCRIPT_COMPACTION_MAX_OUTPUT_TOKENS: u32 = 20_000;
const POST_COMPACT_FILE_RESTORE_CONTRACT: &str = "coder.post_compact_file_restore.v1";
const POST_COMPACT_MAX_FILES_TO_RESTORE: usize = 5;
const POST_COMPACT_TOKEN_BUDGET: u32 = 50_000;
const POST_COMPACT_MAX_TOKENS_PER_FILE: u32 = 5_000;
const POST_COMPACT_MAX_CHARS_PER_FILE: usize = (POST_COMPACT_MAX_TOKENS_PER_FILE as usize) * 4;
const POST_COMPACT_MAX_TOKENS_PER_SKILL: u32 = 5_000;
const POST_COMPACT_SKILLS_TOKEN_BUDGET: u32 = 25_000;
const POST_COMPACT_MAX_CHARS_PER_SKILL: usize = (POST_COMPACT_MAX_TOKENS_PER_SKILL as usize) * 4;
const INVOKED_SKILL_EVENT_KIND: &str = "skill.invoked";
const INVOKED_SKILL_CONTRACT: &str = "coder.invoked_skill.v1";

#[derive(Debug, Clone)]
pub struct ApiState {
    pub store: RunStore,
    pub(crate) session_host: session_host::SessionHost,
    background_commands: Arc<Mutex<BTreeMap<String, Arc<Mutex<BackgroundCommandTask>>>>>,
    background_subagents: Arc<Mutex<BTreeMap<String, Arc<Mutex<BackgroundSubagentTask>>>>>,
    pub(crate) installed_skills: Arc<Mutex<BTreeMap<String, InstalledSkillRecord>>>,
    pub(crate) plugin_marketplaces: Arc<Mutex<BTreeMap<String, PluginMarketplace>>>,
    pub(crate) skill_extra_roots: Arc<Mutex<Vec<SkillExtraRoot>>>,
    pub(crate) provider_settings: Arc<Mutex<ProviderSettings>>,
    pub(crate) credential_store: Arc<dyn credential_store::KeyringStore>,
    pub(crate) mcp_runtime: coder_extensions::StdioMcpRuntime,
}

impl ApiState {
    pub fn new(store: RunStore) -> Self {
        Self::new_with_credential_store(store, credential_store::default_keyring_store())
    }

    pub(crate) fn new_with_credential_store(
        store: RunStore,
        credential_store: Arc<dyn credential_store::KeyringStore>,
    ) -> Self {
        let provider_settings = load_provider_settings(&store, credential_store.as_ref());
        Self {
            store,
            session_host: session_host::SessionHost::default(),
            background_commands: Arc::new(Mutex::new(BTreeMap::new())),
            background_subagents: Arc::new(Mutex::new(BTreeMap::new())),
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
            provider_settings: Arc::new(Mutex::new(provider_settings)),
            credential_store,
            mcp_runtime: coder_extensions::StdioMcpRuntime::default(),
        }
    }
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/v3/health", get(health))
        .route("/api/v3/capabilities", get(capabilities))
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
        .route("/api/v3/conversations", post(conversation::create_session))
        .route(
            "/api/v3/conversations/{session_id}",
            get(conversation::get_session),
        )
        .route(
            "/api/v3/conversations/{session_id}/turn",
            post(conversation::turn),
        )
        .route(
            "/api/v3/conversations/{session_id}/events",
            get(conversation::output_events),
        )
        .route(
            "/api/v3/conversations/{session_id}/turns/{turn_id}/interrupt",
            post(conversation::interrupt_turn),
        )
        .route(
            "/api/v3/conversations/{session_id}/turns/{turn_id}/steer",
            post(conversation::steer_turn),
        )
        .route(
            "/api/v3/mcp/servers",
            get(mcp_runtime::list_mcp_servers).post(mcp_runtime::register_mcp_server),
        )
        .route(
            "/api/v3/mcp/servers/{server_id}",
            axum::routing::delete(mcp_runtime::remove_mcp_server),
        )
        .route(
            "/api/v3/mcp/servers/validate",
            post(mcp_runtime::validate_mcp),
        )
        .route("/api/v3/mcp/tools", get(mcp_runtime::list_mcp_tools))
        .route(
            "/api/v3/mcp/tools/invoke",
            post(mcp_runtime::invoke_mcp_tool),
        )
        .route(
            "/api/v3/mcp/manifests/validate",
            post(mcp_runtime::validate_mcp),
        )
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
        .route("/api/v3/cache/status", get(cache_runtime::cache_status))
        .route("/api/v3/cache/clear", post(cache_runtime::cache_clear))
        .route("/api/v3/cache/reindex", post(cache_runtime::cache_reindex))
        .route("/api/v3/cache/tasks", get(cache_runtime::cache_tasks))
        .route(
            "/api/v3/cache/tasks/{task_id}",
            axum::routing::delete(cache_runtime::cancel_cache_task),
        )
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
            "/api/v3/tools/command/background",
            post(start_background_command_endpoint),
        )
        .route(
            "/api/v3/tools/command/background/{task_id}",
            get(get_background_command_endpoint).delete(cancel_background_command_endpoint),
        )
        .route(
            "/api/v3/tools/command/background/{task_id}/output",
            get(get_background_command_output_endpoint),
        )
        .route(
            "/api/v3/tools/command/background/{task_id}/stdin",
            post(write_background_command_stdin_endpoint),
        )
        .route(
            "/api/v3/tools/model/execute",
            post(execute_model_tool_endpoint),
        )
        .route(
            "/api/v3/tools/model/turn",
            post(execute_model_tool_turn_endpoint),
        )
        .route("/api/v3/tools/subagent/run", post(run_subagent_endpoint))
        .route(
            "/api/v3/tools/subagent/background/{task_id}",
            get(get_background_subagent_endpoint).delete(cancel_background_subagent_endpoint),
        )
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
        .route("/api/v3/runs/{run_id}", get(get_run_detail))
        .route("/api/v3/runs/{run_id}/events", get(list_run_events))
        .route(
            "/api/v3/runs/{run_id}/async-notifications",
            get(list_run_async_notifications),
        )
        .route(
            "/api/v3/runs/{run_id}/async-notifications/drain",
            post(drain_run_async_notifications),
        )
        .route(
            "/api/v3/runs/{run_id}/transcript/compact",
            post(compact_run_transcript),
        )
        .route(
            "/api/v3/runs/{run_id}/skills/invoked",
            post(record_invoked_skill),
        )
        .route(
            "/api/v3/runs/{run_id}/permissions/updates",
            post(run_permission_updates::apply_run_permission_updates),
        )
        .route("/api/v3/runs/{run_id}/timeline", get(list_run_timeline))
        .route(
            "/api/v3/runs/{run_id}/content-replacements",
            get(list_run_content_replacements),
        )
        .route(
            "/api/v3/runs/{run_id}/changes",
            get(change_sets::list_run_changes),
        )
        .route(
            "/api/v3/runs/{run_id}/changes/{change_set_id}/diff",
            get(change_sets::get_change_diff),
        )
        .route(
            "/api/v3/runs/{run_id}/changes/{change_set_id}/accept",
            post(change_sets::accept_change_set),
        )
        .route(
            "/api/v3/runs/{run_id}/changes/{change_set_id}/undo",
            post(change_sets::undo_change_set),
        )
        .route("/api/v3/runs/{run_id}/pause", post(run_control::pause_run))
        .route(
            "/api/v3/runs/{run_id}/resume",
            post(run_control::resume_run),
        )
        .route(
            "/api/v3/runs/{run_id}/cancel",
            post(run_control::cancel_run),
        )
        .route(
            "/api/v3/runs/{run_id}/heartbeat",
            get(run_control::run_heartbeat),
        )
        .route(
            "/api/v3/runs/{run_id}/report/preview",
            get(run_reports::preview_run_report),
        )
        .route(
            "/api/v3/runs/{run_id}/report",
            post(run_reports::write_run_report),
        )
        .route(
            "/api/v3/runs/{run_id}/verification/evidence",
            post(run_reports::record_run_verification_evidence),
        )
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
        .layer(local_api_transport::cors_layer())
}

pub async fn serve(addr: SocketAddr, state: ApiState) -> std::io::Result<()> {
    local_api_transport::validate_bind_address(addr)?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(state)).await
}

fn report_status_string(status: coder_core::ReportStatus) -> String {
    match status {
        coder_core::ReportStatus::Completed => "completed",
        coder_core::ReportStatus::Blocked => "blocked",
        coder_core::ReportStatus::Failed => "failed",
        coder_core::ReportStatus::Cancelled => "cancelled",
    }
    .to_owned()
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
    coder_events::redact_secret_text(text)
}

fn now_timestamp_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| format!("unix:{}", duration.as_secs()))
        .unwrap_or_else(|_| "unix:0".to_owned())
}

pub(crate) fn estimate_text_tokens(value: &str) -> u32 {
    value
        .chars()
        .count()
        .div_ceil(4)
        .max(1)
        .min(u32::MAX as usize) as u32
}

fn truncate_text_to_chars(value: &str, max_chars: usize) -> (String, bool) {
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    let was_truncated = chars.next().is_some();
    (truncated, was_truncated)
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
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
            | StoreError::InvalidBlobDigest(_)
            | StoreError::SessionRecordSecretLikeText => Self {
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
mod tests;
