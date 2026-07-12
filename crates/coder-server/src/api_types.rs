use std::collections::BTreeMap;

use async_trait::async_trait;
use coder_config::{
    AgentSpec as ConfigAgentSpec, HarnessSpec as ConfigHarnessSpec, ModelSpec as ConfigModelSpec,
    PermissionSettingsUpdateApplication, PermissionUpdate, PermissionUpdateApplication,
    PermissionUpdateDestination, ProjectConfig, ValidationIssue, ValidationLevel, ValidationReport,
};
use coder_core::{FinalReport, RunState, RunStatus};
use coder_extensions::{
    ExtensionManifestSummary, PluginManifest, RemoteSkillEntry, SkillSummary, SkillUpdateInfo,
};
use coder_harness::{HarnessRunEvent, HarnessRunEventRef};
use coder_harness::{McpServerSummary, McpToolSummary};
use coder_memory::{
    AgentMemoryRole, KnowledgeChunk, KnowledgeRetrievalHit, KnowledgeSource, MemoryAllowedContext,
    MemoryPurpose, MemoryRecord, MemorySensitivity, ProjectMemoryFile, RetrievalBackendKind,
};
use coder_store::{
    CacheCleanupSummary, RepoEvidenceRef, RunCheckpointRef, RunContentReplacementEntry,
    StoredRunSummary,
};
use coder_tools::{
    CommandRunEvidence, GitDiffEvidence, GitStatusEvidence, PatchApplyEvidence, RepoFileEvidence,
    RepoFileRef, RepoReadSnippet, RepoSearchMatch,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(crate) struct HealthResponse {
    pub(crate) status: &'static str,
    pub(crate) service: &'static str,
    pub(crate) api_version: &'static str,
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
    #[serde(default)]
    pub repo_root: Option<String>,
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
    #[serde(default)]
    pub work_in_progress: bool,
    #[serde(default)]
    pub active_run_id: Option<String>,
    #[serde(default)]
    pub latest_run_id: Option<String>,
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
    #[serde(default)]
    pub artifacts: Vec<PlannerArtifact>,
    #[serde(default)]
    pub response_truncated: bool,
}

pub(crate) fn planner_chat_user_turn(content: String) -> PlannerChatTurn {
    PlannerChatTurn {
        role: "user".to_owned(),
        content,
        artifacts: Vec::new(),
        response_truncated: false,
    }
}

pub(crate) fn planner_chat_assistant_turn(
    content: String,
    artifacts: Vec<PlannerArtifact>,
    response_truncated: bool,
) -> PlannerChatTurn {
    PlannerChatTurn {
        role: "assistant".to_owned(),
        content,
        artifacts,
        response_truncated,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlannerArtifact {
    Table {
        title: String,
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
        #[serde(default)]
        collapsed: bool,
    },
    Notes {
        title: String,
        items: Vec<String>,
        #[serde(default)]
        collapsed: bool,
    },
    Text {
        title: String,
        content: String,
        #[serde(default)]
        collapsed: bool,
    },
}

#[derive(Debug, Deserialize)]
pub struct PlannerChatSessionCreateRequest {
    pub repo: Option<String>,
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
    #[serde(default)]
    pub operation: PlannerTurnOperation,
    pub confirmed: Option<bool>,
    pub mode: Option<String>,
    pub repo: Option<String>,
    pub planner_agent_id: Option<String>,
    pub config: Option<ProjectConfig>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlannerTurnOperation {
    #[default]
    Chat,
    UserInput,
    Status,
    Interrupt,
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
    pub ready_for_start_work: bool,
    pub missing_information: Vec<String>,
    pub concise_plan_summary: String,
    pub execution_allowed: bool,
    pub run_preview: Option<Value>,
    pub response_truncated: bool,
    #[serde(default)]
    pub artifacts: Vec<PlannerArtifact>,
    #[serde(default)]
    pub structured_artifacts: Vec<PlannerArtifact>,
    pub large_artifacts: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_trace: Option<PlannerProviderTrace>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Blocked,
    BudgetLimited,
    UsageLimited,
    MaxTurns,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalState {
    pub session_id: String,
    pub objective: String,
    pub status: GoalStatus,
    pub token_budget: Option<u64>,
    pub tokens_used: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub active_started_at_ms: u64,
    pub paused_at_ms: Option<u64>,
    pub accumulated_active_ms: u64,
    pub blocked_attempts: u32,
    pub last_block_reason: Option<String>,
    pub turns_executed: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct GoalRuntimePolicy {
    pub blocked_consecutive_threshold: u32,
    pub max_goal_turns: u32,
}

#[derive(Debug, Deserialize)]
pub struct GoalCreateRequest {
    pub objective: String,
    pub token_budget: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct GoalTokenUpdateRequest {
    pub delta: u64,
}

#[derive(Debug, Deserialize)]
pub struct GoalBlockedAttemptRequest {
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct GoalGetResponse {
    pub goal: Option<GoalState>,
    pub policy: GoalRuntimePolicy,
}

#[derive(Debug, Serialize)]
pub struct GoalMutationResponse {
    pub goal: GoalState,
    pub state_ref: String,
    pub policy: GoalRuntimePolicy,
}

#[derive(Debug, Serialize)]
pub struct GoalClearResponse {
    pub session_id: String,
    pub removed: bool,
    pub policy: GoalRuntimePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannerReadiness {
    Ready,
    NeedsClarification,
    Blocked,
    Casual,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanExecutionMode {
    ReadOnly,
    MustWrite,
    #[serde(other)]
    #[default]
    MayWrite,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewMode {
    Qualitative,
    #[serde(other)]
    #[default]
    Standard,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanDraft {
    pub goal: String,
    #[serde(default)]
    pub execution_mode: PlanExecutionMode,
    #[serde(default)]
    pub review_mode: PlanReviewMode,
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
    #[serde(default)]
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
    pub repo_root: Option<String>,
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
    #[serde(default)]
    pub artifacts: Vec<PlannerArtifact>,
    pub response_truncated: bool,
    pub large_artifacts: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_trace: Option<PlannerProviderTrace>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannerProviderTrace {
    pub requested_stream: bool,
    pub response_transport: String,
    pub streaming_fallback: bool,
    pub fallback_status: Option<u16>,
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub provider_turns: u32,
    #[serde(default)]
    pub tool_turns: u32,
    #[serde(default)]
    pub tool_calls: u32,
    #[serde(default)]
    pub tool_result_bytes: u64,
    #[serde(default)]
    pub estimated_input_tokens: u64,
    #[serde(default)]
    pub estimated_output_tokens: u64,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_tokens: Option<u64>,
    #[serde(default)]
    pub usage_reported: bool,
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

#[derive(Debug, Deserialize)]
pub struct McpServerRegistrationRequest {
    pub manifest: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct McpServerRegistrationResponse {
    pub server: McpServerSummary,
    pub tools: Vec<McpToolSummary>,
}

#[derive(Debug, Serialize)]
pub struct McpServerRemoveResponse {
    pub server_id: String,
    pub removed: bool,
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
pub(crate) struct InstalledSkillRecord {
    pub(crate) summary: SkillSummary,
    pub(crate) source_url: Option<String>,
    pub(crate) pinned_version: Option<String>,
    pub(crate) update_policy: String,
    pub(crate) history: Vec<SkillSummary>,
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

#[derive(Debug, Deserialize)]
pub struct SkillInvocationRecordRequest {
    pub skill_name: String,
    pub skill_path: String,
    pub content: String,
    pub agent_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SkillInvocationRecordResponse {
    pub contract: &'static str,
    pub source: &'static str,
    pub run_id: String,
    pub skill_name: String,
    pub skill_path: String,
    pub agent_id: Option<String>,
    pub event_sequence: u64,
    pub content_truncated: bool,
    pub content_estimated_tokens: u32,
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

#[derive(Debug, Clone, Default, Serialize)]
pub struct CacheBucketStatus {
    pub entries: usize,
    pub bytes: u64,
    pub stale: bool,
    pub scanned_entries: usize,
    pub entry_scan_limit: usize,
    pub truncated: bool,
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
    pub store: CacheCleanupSummary,
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
    #[serde(default)]
    pub proxy_urls: BTreeMap<String, String>,
    #[serde(default)]
    pub proxy_modes: BTreeMap<String, String>,
    #[serde(default)]
    pub network: BTreeMap<String, ProviderNetworkSettings>,
    pub api_keys: BTreeMap<String, ProviderKeyState>,
    pub mock_mode: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderNetworkSettings {
    pub request_max_retries: Option<u64>,
    pub stream_max_retries: Option<u64>,
    pub stream_idle_timeout_ms: Option<u64>,
    pub websocket_connect_timeout_ms: Option<u64>,
    #[serde(default)]
    pub supports_websockets: bool,
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
            proxy_urls: BTreeMap::new(),
            proxy_modes: BTreeMap::new(),
            network: BTreeMap::new(),
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
    pub proxy_urls: Option<BTreeMap<String, String>>,
    pub proxy_modes: Option<BTreeMap<String, String>>,
    pub network: Option<BTreeMap<String, ProviderNetworkSettings>>,
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
    pub proxy_url: Option<String>,
    pub proxy_mode: String,
    pub request_max_retries: u64,
    pub stream_max_retries: u64,
    pub stream_idle_timeout_ms: u64,
    pub websocket_connect_timeout_ms: u64,
    pub supports_websockets: bool,
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
pub struct MockRunRequest {
    pub config: ProjectConfig,
    pub workflow_id: String,
    pub task: String,
    pub run_id: Option<String>,
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
    pub foreground_timeout_seconds: Option<u64>,
    pub background_on_timeout: Option<bool>,
    pub max_output_bytes: Option<usize>,
    pub interactive: Option<bool>,
    pub source: Option<String>,
    pub sandbox: Option<bool>,
    pub approved: Option<bool>,
    pub run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommandRunResponse {
    pub evidence_ref: Option<RepoEvidenceRef>,
    pub result: CommandRunEvidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_task: Option<CommandBackgroundStartResponse>,
}

#[derive(Debug, Deserialize)]
pub struct CommandBackgroundStartRequest {
    pub repo_root: String,
    pub cwd: Option<String>,
    pub argv: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub max_output_bytes: Option<usize>,
    pub interactive: Option<bool>,
    pub source: Option<String>,
    pub sandbox: Option<bool>,
    pub approved: Option<bool>,
    pub run_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommandBackgroundStartResponse {
    pub task_id: String,
    pub status: String,
    pub command: String,
    pub status_url: String,
    pub output_url: String,
    pub cancel_url: String,
    pub evidence_ref: Option<RepoEvidenceRef>,
}

#[derive(Debug, Serialize)]
pub struct CommandBackgroundStatusResponse {
    pub task_id: String,
    pub status: String,
    pub command: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub output_preview: String,
    pub output_truncated: bool,
    pub output_cursor: u64,
    pub next_output_cursor: u64,
    pub output_gap: bool,
    pub evidence_ref: Option<RepoEvidenceRef>,
    pub result: Option<CommandRunEvidence>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommandBackgroundOutputResponse {
    pub task_id: String,
    pub status: String,
    pub output: String,
    pub output_truncated: bool,
    pub output_cursor: u64,
    pub next_output_cursor: u64,
    pub output_gap: bool,
}

#[derive(Debug, Serialize)]
pub struct CommandBackgroundCancelResponse {
    pub task_id: String,
    pub cancelled: bool,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct CommandWriteStdinRequest {
    #[serde(default)]
    pub input: String,
    #[serde(default)]
    pub close_stdin: bool,
}

#[derive(Debug, Serialize)]
pub struct CommandWriteStdinResponse {
    pub task_id: String,
    pub status: String,
    pub bytes_written: usize,
    pub stdin_closed: bool,
}

#[derive(Debug, Deserialize)]
pub struct SubagentRunToolRequest {
    pub config: ProjectConfig,
    pub workflow_id: String,
    pub node_id: String,
    pub parent_agent_id: String,
    pub parent_harness_id: String,
    pub repo_root: Option<String>,
    pub task: String,
    pub run_id: Option<String>,
    pub agent_id: Option<String>,
    pub subagent_name: Option<String>,
    #[serde(default)]
    pub is_built_in: bool,
    pub invoking_request_id: Option<String>,
    pub invocation_kind: Option<String>,
    #[serde(default)]
    pub parent_query_depth: u32,
    pub parent_sequence: Option<u64>,
    pub run_in_background: Option<bool>,
    pub model_override: Option<String>,
    pub effort_override: Option<Value>,
    #[serde(default)]
    pub backend_context: Value,
}

#[derive(Debug, Serialize)]
pub struct SubagentRunToolResponse {
    pub run_id: String,
    pub agent_id: String,
    pub metadata_ref: String,
    pub transcript_ref: String,
    pub status: String,
    pub report: Option<FinalReport>,
    pub event_count: usize,
    pub event_preview_limit: usize,
    pub events_truncated: bool,
    pub events: Vec<HarnessRunEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_task: Option<SubagentBackgroundStartResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubagentBackgroundStartResponse {
    pub task_id: String,
    pub status: String,
    pub run_id: String,
    pub agent_id: String,
    pub status_url: String,
    pub cancel_url: String,
    pub metadata_ref: String,
    pub transcript_ref: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubagentBackgroundStatusResponse {
    pub task_id: String,
    pub status: String,
    pub run_id: String,
    pub agent_id: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub metadata_ref: String,
    pub transcript_ref: String,
    pub report: Option<FinalReport>,
    pub event_count: usize,
    pub event_preview_limit: usize,
    pub events_truncated: bool,
    pub events: Vec<HarnessRunEvent>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SubagentBackgroundCancelResponse {
    pub task_id: String,
    pub cancelled: bool,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct ModelToolExecuteRequest {
    pub tool_use_id: String,
    pub tool_name: String,
    pub run_id: Option<String>,
    pub harness_id: Option<String>,
    #[serde(default, alias = "agentId")]
    pub agent_id: Option<String>,
    #[serde(default, alias = "currentModel", alias = "mainLoopModel")]
    pub current_model: Option<String>,
    #[serde(default, alias = "currentEffort", alias = "effortValue")]
    pub current_effort: Option<Value>,
    #[serde(default, alias = "skillContextModifiers")]
    pub skill_context_modifiers: Vec<Value>,
    #[serde(default)]
    pub input: Value,
}

#[derive(Debug, Deserialize)]
pub struct ModelToolTurnRequest {
    #[serde(default)]
    pub tool_uses: Vec<ModelToolUseRequestBlock>,
    pub max_tool_use_concurrency: Option<usize>,
    pub run_id: Option<String>,
    pub harness_id: Option<String>,
    #[serde(default, alias = "agentId")]
    pub agent_id: Option<String>,
    #[serde(default, alias = "currentModel", alias = "mainLoopModel")]
    pub current_model: Option<String>,
    #[serde(default, alias = "currentEffort", alias = "effortValue")]
    pub current_effort: Option<Value>,
    #[serde(default, alias = "skillContextModifiers")]
    pub skill_context_modifiers: Vec<Value>,
}

#[derive(Debug, Deserialize)]
pub struct ModelToolUseRequestBlock {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub input: Value,
}

#[derive(Debug, Serialize)]
pub struct ModelToolTurnResponse {
    pub contract: &'static str,
    pub source: &'static str,
    pub result_contract: &'static str,
    pub model_tool_result_bridge: &'static str,
    pub results: Vec<ModelToolExecuteResponse>,
    pub attachments: Vec<Value>,
}

#[derive(Debug, Serialize)]
pub struct ModelToolExecuteResponse {
    pub contract: &'static str,
    pub source: &'static str,
    #[serde(rename = "type")]
    pub result_type: &'static str,
    pub tool_use_id: String,
    pub tool_name: String,
    pub status: String,
    pub is_error: bool,
    pub content: String,
    pub content_truncated: bool,
    pub payload: Value,
    pub refs: Vec<HarnessRunEventRef>,
    pub phases: Vec<Value>,
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
    pub event_count: usize,
    pub returned_count: usize,
    pub truncated: bool,
    pub next_after_sequence: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct RunAsyncNotificationsResponse {
    pub contract: &'static str,
    pub source: &'static str,
    pub policy: &'static str,
    pub run_id: String,
    pub notifications_url: String,
    pub notifications: Vec<coder_events::CoderEvent>,
    pub event_count: usize,
    pub notification_count: usize,
    pub returned_count: usize,
    pub truncated: bool,
    pub next_after_sequence: Option<u64>,
    pub delivery_status: &'static str,
}

#[derive(Debug, Deserialize)]
pub struct RunPermissionUpdateRequest {
    pub harness_id: Option<String>,
    #[serde(default)]
    pub updates: Vec<PermissionUpdate>,
    pub source: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunPermissionUpdateResponse {
    pub contract: &'static str,
    pub source: &'static str,
    pub run_id: String,
    pub harness_id: String,
    pub status: String,
    pub config_source: String,
    pub config_ref: Option<String>,
    pub event_sequence: Option<u64>,
    pub applications: Vec<PermissionUpdateApplication>,
    #[serde(default)]
    pub persistence: Vec<RunPermissionUpdatePersistence>,
    pub validation: ValidationReport,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunPermissionUpdatePersistence {
    pub destination: PermissionUpdateDestination,
    pub status: String,
    pub update_count: usize,
    pub settings_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applications: Vec<PermissionSettingsUpdateApplication>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunAsyncNotificationDrainResponse {
    pub contract: &'static str,
    pub source: &'static str,
    pub policy: &'static str,
    pub run_id: String,
    pub delivery_channel: &'static str,
    pub mode: &'static str,
    pub processed: bool,
    pub delivery_status: &'static str,
    pub attachments: Vec<Value>,
    pub event_count: usize,
    pub notification_count: usize,
    pub returned_count: usize,
    pub next_after_sequence: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RunTranscriptCompactionRequest {
    pub custom_instructions: Option<String>,
    pub scope_id: Option<String>,
    pub max_events: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunTranscriptCompactionCircuitResponse {
    pub scope_id: String,
    pub max_consecutive_failures: u8,
    pub consecutive_failures: u8,
    pub circuit_breaker_open: bool,
    pub updated_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunTranscriptCompactionResponse {
    pub contract: &'static str,
    pub source: &'static str,
    pub policy: &'static str,
    pub run_id: String,
    pub status: String,
    pub success: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub endpoint: Option<String>,
    pub summary: Option<String>,
    pub summary_estimated_tokens: u32,
    pub transcript_event_count: usize,
    pub transcript_events_included: usize,
    pub transcript_events_omitted: usize,
    pub transcript_truncated: bool,
    pub transcript_estimated_tokens: u32,
    pub artifact_ref: Option<String>,
    pub event_sequence: Option<u64>,
    pub error: Option<String>,
    pub circuit: RunTranscriptCompactionCircuitResponse,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunContentReplacementsResponse {
    pub contract: &'static str,
    pub source: &'static str,
    pub policy: &'static str,
    pub run_id: String,
    pub records_ref: String,
    pub records_url: String,
    pub records: Vec<RunContentReplacementEntry>,
    pub record_count: usize,
    pub returned_count: usize,
    pub replacement_count: usize,
    pub truncated: bool,
    pub next_after_sequence: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RunEventsQuery {
    pub after_sequence: Option<u64>,
    pub limit: Option<usize>,
    #[serde(default)]
    pub tail: bool,
}

#[derive(Debug, Deserialize)]
pub struct RunDetailQuery {
    #[serde(default = "default_true")]
    pub include_events: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct RunTimelineResponse {
    pub run_id: String,
    pub items: Vec<TimelineItem>,
    pub event_count: usize,
    pub returned_count: usize,
    pub truncated: bool,
    pub next_after_sequence: Option<u64>,
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
    pub status: String,
    pub summary: String,
    pub changed_files: Vec<String>,
    pub checks: Vec<String>,
    pub evidence_refs: Vec<coder_core::EvidenceRef>,
    pub blockers: Vec<String>,
    pub next_steps: Vec<String>,
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
    #[serde(default)]
    pub undo_conflict: Option<String>,
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
    pub event_count: usize,
    pub returned_count: usize,
    pub report: Option<FinalReport>,
    pub repo_evidence_count: usize,
}

#[derive(Debug, Serialize)]
pub struct RunReportResponse {
    pub run_id: String,
    pub report_ref: Option<String>,
    pub report: FinalReport,
}

#[derive(Debug, Deserialize)]
pub struct RunVerificationEvidenceRequest {
    pub status: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub evidence: Value,
    #[serde(default)]
    pub remaining_work: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RunVerificationEvidenceResponse {
    pub run_id: String,
    pub status: String,
    pub event_count: usize,
    pub evidence_ref: String,
    pub report: FinalReport,
}

#[derive(Debug, Serialize)]
pub struct RunControlResponse {
    pub run_id: String,
    pub status: RunStatus,
    pub control_state: String,
    pub event_count: usize,
    pub report_ref: Option<String>,
    pub content_replacement_replay: Option<RunContentReplacementsResponse>,
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

pub(crate) fn validation_issue(
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
