use axum::{extract::State, Json};

use crate::api_types::{CapabilitiesResponse, HealthResponse};
use crate::ApiState;

pub(crate) async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "coder-server",
        api_version: "v3",
    })
}

pub(crate) async fn capabilities(State(state): State<ApiState>) -> Json<CapabilitiesResponse> {
    Json(CapabilitiesResponse {
        api_version: "v3",
        capabilities: state
            .session_host
            .capability_ids()
            .into_iter()
            .map(str::to_owned)
            .collect(),
        tasks: vec!["preview", "run"],
        runs: vec![
            "list",
            "detail",
            "events",
            "async_notifications",
            "async_notifications_drain",
            "transcript_compaction",
            "permission_updates",
            "pause",
            "resume",
            "cancel",
            "heartbeat",
            "report_preview",
            "report_write",
            "verification_evidence",
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
            "command_background",
            "read_command_output",
            "write_stdin",
            "cancel_command_background",
            "model_tool_execute",
            "model_tool_turn",
            "agent_subagent",
            "read_subagent_status",
            "cancel_subagent_background",
            "patch_preview",
            "patch_apply",
            "apply_patch",
        ],
        conversations: vec!["sessions", "turns", "plain_chat"],
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
            "mcp_stdio_invoke",
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
