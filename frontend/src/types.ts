
export type AgentModelTier = "best" | "standard" | "economy";
export type AgentWorkflowRole =
  | "planner"
  | "executor";
export type AgentCapability = string;
export type HandoffType =
  | "run_contract"
  | "planner_order"
  | "execution_result"
  | "planner_decision"
  | "round_summary";

export interface CapabilityPermissions {
  read_files: boolean;
  edit_files: boolean;
  run_commands: boolean;
  use_network: boolean;
}

export interface CapabilitySpec {
  id: AgentCapability;
  label: string;
  description: string;
  allowed_roles: AgentWorkflowRole[];
  requires: HandoffType[];
  produces: HandoffType[];
  permissions: CapabilityPermissions;
  can_talk_to_human: boolean;
  runtime_effects: string[];
}

export interface RoleCardSpec {
  id: string;
  label: string;
  archetype: string;
  role: AgentWorkflowRole;
  engine_id: string;
  default_capabilities: AgentCapability[];
  description: string;
}

export interface AgentWorkflowAgent {
  id: string;
  name: string;
  role: AgentWorkflowRole;
  role_card?: string | null;
  purpose?: string;
  model_tier: AgentModelTier;
  can_talk_to_human: boolean;
  capabilities: AgentCapability[];
  runtime_profile_id?: string | null;
  skill_pack_ids?: string[];
  knowledge_pack_ids?: string[];
  memory_pack_ids?: string[];
}

export interface AgentWorkflowEdge {
  from: string;
  to: string;
  handoff?: HandoffType | null;
  loop?: boolean;
}

export interface AgentWorkflowLoopPolicy {
  max_auto_rounds: number;
  user_can_change: boolean;
}

export interface HarnessModeBinding {
  profile_id: string;
  provider_id?: string;
}

export interface HarnessBindings {
  planning_chat: HarnessModeBinding;
  workflow_supervisor: HarnessModeBinding;
  task_execution: HarnessModeBinding;
  agent_overrides?: Record<string, Record<string, HarnessModeBinding>>;
}

export interface AgentWorkflowSpec {
  id: string;
  version: string;
  name: string;
  description: string;
  primary_planner_id: string;
  agents: AgentWorkflowAgent[];
  edges: AgentWorkflowEdge[];
  harness_bindings?: HarnessBindings;
  loop_policy: AgentWorkflowLoopPolicy;
  ui?: {
    layout?: Record<string, { x: number; y: number }>;
  };
}

export interface PreflightIssue {
  level: "error" | "warning" | string;
  code: string;
  message: string;
  target_type: string;
  target_id?: string | null;
}

export interface PreflightResult {
  status: "pass" | "warning" | "error" | string;
  issues: PreflightIssue[];
  summary: Record<string, unknown>;
}

export interface AgentWorkflowValidationIssue {
  level: "error" | "warning";
  code: string;
  message: string;
  target_type: string;
  target_id?: string | null;
}

export interface AgentWorkflowValidationResult {
  status: "pass" | "warning" | "error";
  issues: AgentWorkflowValidationIssue[];
  summary: Record<string, unknown>;
}

export interface PlannerChatDraft {
  draft_id: string;
  artifact_type: "project_plan_draft";
  summary: string;
  proposed_scope: string[];
  success_criteria: string[];
  risks: string[];
  requires_confirmation: boolean;
}

export interface PlannerChatConfirmResult {
  draft_id?: string;
  run_id?: string;
  status: string;
}

export interface ProviderKeyState {
  configured: boolean;
  source: string;
}

export interface ProviderSettings {
  default_provider: string;
  default_model: string;
  base_urls: Record<string, string>;
  api_keys: Record<string, ProviderKeyState>;
  mock_mode: boolean;
}

export interface ProviderStatusItem {
  provider: string;
  configured: boolean;
  credential_configured: boolean;
  credential_source: string;
  base_url?: string | null;
  mode: string;
}

export interface ProviderStatus {
  default_provider: string;
  default_model: string;
  mock_mode: boolean;
  default_status: ProviderStatusItem;
  providers: ProviderStatusItem[];
}

export interface ProviderFormState {
  default_provider: string;
  default_model: string;
  base_url: string;
  api_key: string;
  mock_mode: boolean;
}

export interface WorkflowSummary {
  id: string;
  version?: string;
  name?: string;
  description?: string;
  nodes: number;
  edges: number;
  agents: number;
}

export interface AgentWorkflowSummary {
  id: string;
  version?: string;
  name?: string;
  description?: string;
  agents: number;
  edges: number;
  max_auto_rounds?: number | null;
}
export interface AgentSummary {
  id: string;
  name?: string;
  role?: AgentWorkflowRole;
  purpose?: string;
  model_tier?: AgentModelTier;
  capabilities: AgentCapability[];
}

export interface LibraryIndex {
  agents: AgentSummary[];
  agent_workflows: AgentWorkflowSummary[];
}

export interface RunEvent {
  id?: string;
  type: string;
  node_id?: string | null;
  message?: string | null;
  payload?: Record<string, unknown>;
  created_at?: string;
}

export interface RunSummaryItem {
  id: string;
  workflow_id: string;
  runtime_type?: "agent_graph";
  repo_root: string;
  request: string;
  status: string;
  events: number;
  agent_calls?: number;
  tool_calls?: number;
  estimated_tokens_used?: number;
  stored_run_id?: string | null;
  error?: string | null;
  status_reason?: string | null;
  status_code?: string | null;
  run_group_id?: string | null;
  parent_run_id?: string | null;
  continued_from_run_id?: string | null;
  turn_index?: number | null;
  approval_required?: boolean;
}

export interface RunResult {
  status: string;
  data: Record<string, unknown>;
  summaries: Record<string, string>;
  events: RunEvent[];
  estimated_tokens_used: number;
  agent_calls: number;
  tool_calls: number;
  blocked_node_id?: string | null;
  resume_checkpoint?: Record<string, unknown> | null;
  status_reason?: string | null;
  status_code?: string | null;
}

export interface StoredRunDetail {
  id: string;
  workflow_id: string;
  repo_root: string;
  request: string;
  result: RunResult;
}

export interface RunEventsPage {
  events: RunEvent[];
  cursor: number;
  next_cursor: number;
  has_more: boolean;
}

export interface ContextPacketDetail {
  packet_id: string;
  packet: Record<string, unknown>;
}

export interface ArtifactDetail {
  artifact_id: string;
  artifact: Record<string, unknown>;
}

export interface ToolResultDetail {
  tool_result_id: string;
  result: Record<string, unknown>;
}

export interface BlobDetail {
  blob_id: string;
  size_bytes: number;
  media_type: string;
  content: string;
}

export interface LiveRunDetail {
  id: string;
  workflow_id: string;
  runtime_type?: "agent_graph";
  repo_root: string;
  request: string;
  status: string;
  events: RunEvent[];
  result?: RunResult | null;
  stored_run_id?: string | null;
  error?: string | null;
  status_reason?: string | null;
  status_code?: string | null;
  run_group_id?: string | null;
  parent_run_id?: string | null;
  continued_from_run_id?: string | null;
  turn_index?: number | null;
  approval_required?: boolean;
}

export interface HealthStatus {
  status: string;
  tools: string[];
}

export interface ConnectorOperationSummary {
  connector_id: string;
  operation_id: string;
  risk_level: string;
  external_effect: boolean;
  requires_preview: boolean;
  requires_human_approval: boolean;
  descriptor_sha256?: string | null;
  package_sha256?: string;
}

export interface SkillSummary {
  id: string;
  name: string;
  version: string;
  description: string;
  category: string;
  risk_level: "low" | "medium" | "high" | string;
  publisher: string;
  requires: string[];
  produces: string[];
  connectors: string[];
  connector_operations: ConnectorOperationSummary[];
  trust_level: "official" | "verified" | "community" | "local" | "untrusted" | string;
  enabled: boolean;
  external_effect: boolean;
  when_to_use: string[];
}

export interface SkillIndexEntry {
  id: string;
  name: string;
  description: string;
  when_to_use: string[];
  category: string;
  risk_level: string;
  produces: string[];
  requires: string[];
  connectors: string[];
  connector_operations: ConnectorOperationSummary[];
  trust_level: string;
  enabled: boolean;
  max_skill_tokens: number;
}

export interface SkillIndexPayload {
  skills: SkillIndexEntry[];
}

export interface InstalledSkillsPayload {
  skills: SkillSummary[];
  index: SkillIndexPayload;
}

export interface RemoteSkillEntry {
  id: string;
  name: string;
  version: string;
  description: string;
  category: string;
  publisher: string;
  package_url: string;
  manifest_url?: string | null;
  sha256: string;
  signature?: string | null;
  risk_level: string;
  external_effect: boolean;
  requires_connectors: string[];
  connector_operations: ConnectorOperationSummary[];
  trust_level: string;
}

export interface DiscoverSkillsPayload {
  registry: {
    registry_version: string;
    generated_at: string;
    skills: RemoteSkillEntry[];
  };
  skills: Array<RemoteSkillEntry & { installed: boolean }>;
}

export interface SkillUpdateInfo {
  skill_id: string;
  installed_version: string;
  available_version?: string | null;
  update_available: boolean;
  auto_update_eligible: boolean;
  pinned_version?: string | null;
  update_policy: "manual" | "auto_official_low_risk" | string;
  reason?: string;
  risk_level?: string;
  trust_level?: string;
  external_effect?: boolean;
}

export interface ExtensionManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  extension_type: "plugin" | "skill" | "agent_engine" | string;
  installed: boolean;
  enabled: boolean;
  risk_level: string;
  trust_level: string;
  tags: string[];
}

export interface PluginManifest extends ExtensionManifest {
  operations: string[];
  external_effect: boolean;
  requires_preview: boolean;
}
