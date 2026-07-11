
export type AgentModelTier = "best" | "standard" | "economy";
export type AgentWorkflowRole =
  | "planner"
  | "executor";
export type AgentCapability = string;
export type HandoffType =
  | "run_contract"
  | "planner_conversation"
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
  on?: string;
  handoff?: HandoffType | null;
  loop?: boolean;
}

export interface AgentWorkflowLoopPolicy {
  max_auto_rounds: number;
  token_budget?: number | null;
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

export type RustMemoryScope =
  | "user"
  | "project"
  | "agent"
  | "workflow"
  | "run"
  | "repo_facts"
  | "knowledge_hints"
  | "external_docs";

export type RustPermissionDecision = "allow" | "ask" | "deny";

export interface RustModelSpec {
  provider: string;
  model: string;
  base_url_env?: string | null;
  api_key_env?: string | null;
}

export interface RustMemoryAccess {
  read: RustMemoryScope[];
  write: RustMemoryScope[];
}

export interface RustMemoryRecord {
  id: string;
  scope: RustMemoryScope | string;
  key: string;
  content: string;
  tags: string[];
  evidence_refs: RustEvidenceRef[];
  source_ref?: string | null;
  trust_level: string;
}

export interface RustProjectMemoryFile {
  version: number;
  records: RustMemoryRecord[];
}

export interface RustProjectMemoryLoadRequest {
  repo_root: string;
  memory_path: string;
  requested_by_role: RustAgentMemoryRole;
  run_id?: string | null;
}

export interface RustProjectMemoryLoadResponse {
  record_count: number;
  event_recorded: boolean;
  memory: RustProjectMemoryFile;
}

export interface RustProjectMemoryWriteProposalRequest {
  run_id: string;
  proposed_by_role: RustAgentMemoryRole;
  record: RustMemoryRecord;
}

export interface RustProjectMemoryWriteProposalResponse {
  run_id: string;
  event_count: number;
  event: RustCoderEvent;
}

export type RustAgentMemoryRole =
  | "planning_chat"
  | "workflow_supervisor"
  | "task_execution"
  | "planner"
  | "executor"
  | "verifier"
  | string;

export type RustMemoryAllowedContext =
  | "assistant_message"
  | "planner_task_state"
  | "planner_order"
  | "execution_prompt"
  | "workflow_supervision"
  | "final_report"
  | string;

export type RustMemoryTrustLevel =
  | "source"
  | "user_confirmed"
  | "system_recorded"
  | "model_inferred"
  | string;

export type RustRetrievalBackendKind = "lexical" | "dense_mock" | "hybrid";

export interface RustKnowledgeRetrieveRequest {
  repo_root: string;
  role: RustAgentMemoryRole;
  query: string;
  requested_context: RustMemoryAllowedContext;
  backend?: RustRetrievalBackendKind;
  scope?: "project" | "private" | "public" | "all" | string;
  tags?: string[];
  token_budget?: number;
  top_k?: number;
  max_results?: number;
  include_content?: boolean;
}

export interface RustKnowledgeHint {
  id: string;
  source_id: string;
  title: string;
  summary: string;
  tags: string[];
  evidence_kind: string;
  requires_repo_verification: boolean;
  trust_level: RustMemoryTrustLevel;
  sensitivity: string;
  content_hash: string;
  token_estimate: number;
  score: number;
  backend: RustRetrievalBackendKind;
  content_preview?: string | null;
  content_truncated: boolean;
}

export interface RustKnowledgeRetrievalHit {
  source_id: string;
  chunk_id: string;
  score: number;
  backend: RustRetrievalBackendKind;
  preview: string;
  trust_level: RustMemoryTrustLevel;
  evidence_ref: string;
}

export interface RustKnowledgeRetrieveResponse {
  results: RustKnowledgeHint[];
  hits: RustKnowledgeRetrievalHit[];
}

export type RustMcpRiskLevel = "low" | "medium" | "high";
export type RustMcpSideEffectLevel = "none" | "read" | "write" | "external";

export interface RustMcpManifestOperation {
  name: string;
  description: string;
  risk: RustMcpRiskLevel;
  side_effect: RustMcpSideEffectLevel;
  enabled_by_default: boolean;
}

export interface RustMcpServerManifest {
  server_id: string;
  name: string;
  operations: RustMcpManifestOperation[];
  enabled_by_default: boolean;
}

export interface RustMcpManifestValidationRequest {
  manifest: unknown;
}

export interface RustMcpManifestValidation {
  ok: boolean;
  errors: string[];
  warnings: string[];
  manifest?: RustMcpServerManifest | null;
}

export interface RustMcpServerSummary {
  server_id: string;
  name: string;
  enabled: boolean;
  requires_approval: boolean;
  operations: RustMcpManifestOperation[];
}

export interface RustMcpServerListResponse {
  servers: RustMcpServerSummary[];
}

export interface RustMcpToolSummary {
  server_id: string;
  name: string;
  description: string;
  risk: RustMcpRiskLevel;
  side_effect: RustMcpSideEffectLevel;
  enabled: boolean;
  requires_approval: boolean;
}

export interface RustMcpToolListResponse {
  tools: RustMcpToolSummary[];
}

export interface RustMcpToolCallRequest {
  server_id: string;
  tool_name: string;
  args?: unknown;
  run_id?: string | null;
  approved?: boolean;
}

export interface RustMcpToolCallResult {
  status: "completed" | "blocked" | "failed" | string;
  requires_approval: boolean;
  approval_key: string;
  output: unknown;
  evidence_ref?: string | null;
}

export interface RustToolCapability {
  name: string;
  toolset: string;
  side_effect: RustMcpSideEffectLevel;
  risk: RustMcpRiskLevel;
}

export interface RustToolRegistryEntry {
  capability: RustToolCapability;
  description: string;
  harness_ids: string[];
  required_permission: string;
  approval_behavior: string;
  evidence_emitted: string;
  timeline_item: string;
  enabled_by_default: boolean;
  requires_approval: boolean;
}

export interface RustToolRegistryResponse {
  harness_id?: string | null;
  tools: RustToolRegistryEntry[];
}

export type RustExtensionType = "plugin" | "harness_runtime";
export type RustExtensionRiskLevel = "low" | "medium" | "high";
export type RustExtensionTrustLevel =
  | "official"
  | "verified"
  | "community"
  | "local"
  | "untrusted";

export interface RustPluginManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  extension_type: RustExtensionType;
  installed: boolean;
  enabled: boolean;
  risk_level: RustExtensionRiskLevel;
  trust_level: RustExtensionTrustLevel;
  tags: string[];
  operations: string[];
  external_effect: boolean;
  requires_preview: boolean;
}

export interface RustExtensionPluginListResponse {
  plugins: RustPluginManifest[];
}

export interface RustExtensionPluginValidationRequest {
  manifest: unknown;
}

export interface RustPluginManifestValidation {
  ok: boolean;
  errors: string[];
  warnings: string[];
  manifest?: RustPluginManifest | null;
}

export interface RustAgentSpec {
  role: string;
  model: string;
  system: string;
  memory: RustMemoryAccess;
  output_contract: string;
}

export interface RustPermissionPolicy {
  read_files: RustPermissionDecision;
  write_files: RustPermissionDecision;
  run_commands: RustPermissionDecision;
  child_harness_permissions: RustPermissionDecision;
  network: RustPermissionDecision;
  secrets: RustPermissionDecision;
  publish_external: RustPermissionDecision;
  git_commit: RustPermissionDecision;
  git_push: RustPermissionDecision;
  deploy: RustPermissionDecision;
}

export interface RustVerificationPolicy {
  require_evidence: boolean;
  allowed_checks: string[];
}

export interface RustHarnessSpec {
  backend: string;
  tools: string[];
  permissions: RustPermissionPolicy;
  memory: RustMemoryAccess;
  verification: RustVerificationPolicy;
}

export interface RustWorkflowNodeSpec {
  id: string;
  agent: string;
  harness: string;
}

export interface RustWorkflowEdgeSpec {
  from: string;
  to: string;
  on: string;
}

export interface RustStopPolicy {
  on_status: string[];
  final_report_agent?: string | null;
}

export interface RustWorkflowSpec {
  name: string;
  max_rounds: number;
  token_budget?: number | null;
  nodes: RustWorkflowNodeSpec[];
  edges: RustWorkflowEdgeSpec[];
  stop: RustStopPolicy;
}

export interface RustHookMatcherSpec {
  matcher?: string | null;
  hooks: Array<Record<string, unknown>>;
}

export interface RustHookSettings {
  PreToolUse?: RustHookMatcherSpec[];
  PostToolUse?: RustHookMatcherSpec[];
  PostToolUseFailure?: RustHookMatcherSpec[];
}

export interface RustProjectConfig {
  version: 1;
  disable_all_hooks?: boolean;
  disableAllHooks?: boolean;
  hooks?: RustHookSettings;
  models: Record<string, RustModelSpec>;
  agents: Record<string, RustAgentSpec>;
  harnesses: Record<string, RustHarnessSpec>;
  workflows: Record<string, RustWorkflowSpec>;
}

export interface RustWorkflowExport extends RustProjectConfig {
  kind: "coder.workflow";
  workflow_id: string;
  workflow: RustWorkflowSpec;
  ui?: AgentWorkflowSpec["ui"];
  legacy_agent_workflow?: AgentWorkflowSpec;
}

export interface RustValidationIssue {
  level: "Error" | "Warning" | string;
  code: string;
  message: string;
  target: string;
}

export interface RustValidationReport {
  status: "pass" | "warning" | "error" | string;
  issues: RustValidationIssue[];
}

export type RustRunStatus = "queued" | "running" | "completed" | "blocked" | "failed" | "cancelled" | string;

export interface RustRunState {
  run_id: string;
  workflow_id: string;
  status: RustRunStatus;
  created_at: string;
  updated_at: string;
}

export interface RustEventRef {
  label: string;
  uri: string;
}

export interface RustCoderEvent {
  event_id: string;
  run_id: string;
  sequence: number;
  timestamp: string;
  kind: string;
  payload?: unknown;
  refs: RustEventRef[];
}

export interface RustEvidenceRef {
  kind: string;
  reference: string;
}

export interface RustFinalReport {
  status: "completed" | "blocked" | "failed" | "cancelled" | string;
  summary: string;
  changed_files: string[];
  checks: string[];
  patch_refs: string[];
  artifact_refs: string[];
  evidence_refs: RustEvidenceRef[];
  blockers: string[];
  next_steps: string[];
}

export interface RustRunSummary {
  run_id: string;
  metadata?: RustRunState | null;
  event_count: number;
  has_report: boolean;
  repo_evidence_count: number;
}

export interface RustRunListResponse {
  runs: RustRunSummary[];
}

export interface RustRunDetail {
  run_id: string;
  metadata?: RustRunState | null;
  events: RustCoderEvent[];
  event_count?: number;
  returned_count?: number;
  report?: RustFinalReport | null;
  repo_evidence_count: number;
}

export interface RustRunReportResponse {
  run_id: string;
  report_ref?: string | null;
  report: RustFinalReport;
}

export interface RustRunControlResponse {
  run_id: string;
  status: RustRunStatus;
  control_state: string;
  event_count: number;
  report_ref?: string | null;
  content_replacement_replay?: RustRunContentReplacementsResponse | null;
}

export interface RustRunHeartbeatResponse {
  run_id: string;
  status?: RustRunStatus | null;
  event_count: number;
  has_report: boolean;
  repo_evidence_count: number;
}

export interface RustRepoEvidenceRef {
  ref_id: string;
  kind: string;
  repo_root: string;
  scope_paths: string[];
  summary: string;
  payload_path: string;
  created_at: string;
  token_estimate: number;
}

export interface RustRunRepoEvidenceResponse {
  run_id: string;
  evidence: RustRepoEvidenceRef[];
}

export interface RustRunEventsResponse {
  run_id: string;
  events: RustCoderEvent[];
  event_count?: number;
  returned_count?: number;
  truncated?: boolean;
  next_after_sequence?: number | null;
}

export interface RustContentReplacementRecord {
  kind: string;
  toolUseId: string;
  replacement: string;
}

export interface RustRunContentReplacementEntry {
  run_id: string;
  sequence: number;
  kind: string;
  created_at: string;
  replacements: RustContentReplacementRecord[];
}

export interface RustRunContentReplacementsResponse {
  contract: string;
  source: string;
  policy: string;
  run_id: string;
  records_ref: string;
  records_url: string;
  records: RustRunContentReplacementEntry[];
  record_count: number;
  returned_count: number;
  replacement_count: number;
  truncated: boolean;
  next_after_sequence?: number | null;
}

export interface RustRunArtifactResponse {
  run_id: string;
  artifact_name: string;
  payload: unknown;
}

export interface RustRunCheckpointRef {
  name: string;
  checkpoint_ref: string;
}

export interface RustRunCheckpointListResponse {
  run_id: string;
  checkpoints: RustRunCheckpointRef[];
}

export interface RustRunCheckpointResponse {
  run_id: string;
  checkpoint_name: string;
  payload: unknown;
}

export interface RustRunCheckpointWriteResponse {
  run_id: string;
  checkpoint_name: string;
  checkpoint_ref: string;
}

export interface RustRepoEvidenceResponse {
  ref_id: string;
  payload: unknown;
}

export interface RustCommandPolicyDecision {
  allowed: boolean;
  requires_approval: boolean;
  risk: string;
  reason: string;
}

export interface RustCommandPreviewRequest {
  repo_root: string;
  cwd?: string | null;
  argv: string[];
  source?: string | null;
  sandbox?: boolean | null;
}

export interface RustCommandPreview {
  repo_root: string;
  cwd: string;
  argv: string[];
  command: string;
  requires_approval: boolean;
  approval_key: string;
  policy: RustCommandPolicyDecision;
  evidence_kind: string;
}

export interface RustPatchFilePreview {
  old_path?: string | null;
  new_path?: string | null;
  status: string;
  hunks: number;
  additions: number;
  deletions: number;
  target_exists: boolean;
}

export interface RustPatchPreview {
  repo_root: string;
  files: RustPatchFilePreview[];
  file_count: number;
  hunk_count: number;
  additions: number;
  deletions: number;
  truncated: boolean;
  evidence_kind: string;
}

export interface RustPatchPreviewRequest {
  repo_root: string;
  patch_file: string;
  max_patch_bytes?: number | null;
}

export interface RustPatchApplyRequest extends RustPatchPreviewRequest {
  source?: string | null;
  approved?: boolean | null;
  run_id: string;
}

export interface RustPatchApplyResult {
  repo_root: string;
  patch_file: string;
  status: string;
  applied: boolean;
  requires_approval: boolean;
  approval_key: string;
  reason: string;
  preview: RustPatchPreview;
  evidence_kind: string;
}

export interface RustPatchApplyResponse {
  run_id: string;
  evidence_ref: RustRepoEvidenceRef;
  result: RustPatchApplyResult;
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

export type PlannerInteractionMode = "discuss" | "work";

export interface PlannerPlanStep {
  id: string;
  summary: string;
  depends_on: string[];
  status: "draft" | "ready" | "executing" | "done" | "blocked";
}

export interface PlannerMemoryProposal {
  scope: "user" | "project" | string;
  key: string;
  content: string;
  rationale: string;
  requires_confirmation: boolean;
}

export interface PlannerTaskState {
  goal?: string | null;
  user_intent?: string | null;
  scope: string[];
  constraints: string[];
  success_criteria: string[];
  known_context: string[];
  missing_context: string[];
  open_questions: string[];
  assumptions: string[];
  risks: string[];
  memory_proposals: PlannerMemoryProposal[];
  plan_steps: PlannerPlanStep[];
  readiness: "not_ready" | "needs_clarification" | "ready_to_plan" | "ready_to_execute";
}

export interface PlannerVisibleThinking {
  phase:
    | "understanding"
    | "gathering_context"
    | "clarifying"
    | "planning"
    | "checking_readiness"
    | "ready_to_start"
    | "reporting";
  summary: string;
}

export interface PlannerWorkflowHandoff {
  workflow_request: string;
  scope: string[];
  success_criteria: string[];
  risks: string[];
}

export type PlannerArtifact =
  | { type: "table"; title: string; columns: string[]; rows: string[][]; collapsed?: boolean }
  | { type: "notes"; title: string; items: string[]; collapsed?: boolean }
  | { type: "text"; title: string; content: string; collapsed?: boolean };

export interface PlannerChatTurn {
  artifact_id?: string | null;
  artifact_type: "planner_chat_turn";
  assistant_message: string;
  interaction_mode: PlannerInteractionMode;
  decision:
    | "continue_chat"
    | "produce_plan"
    | "answer_without_workflow"
    | "start_workflow"
    | "blocked_needs_clarification";
  visible_thinking: PlannerVisibleThinking;
  task_state: PlannerTaskState;
  handoff?: PlannerWorkflowHandoff | null;
  response_truncated?: boolean;
  artifacts?: PlannerArtifact[];
  large_artifacts?: boolean;
}

export interface PlannerProviderTrace {
  requested_stream: boolean;
  response_transport: string;
  streaming_fallback: boolean;
  fallback_status?: number | null;
  finish_reason?: string | null;
  provider_turns: number;
  estimated_input_tokens: number;
  estimated_output_tokens: number;
  input_tokens?: number | null;
  output_tokens?: number | null;
  total_tokens?: number | null;
  cache_read_tokens?: number | null;
  usage_reported: boolean;
}

export interface PlannerChatMessage {
  role: "user" | "assistant" | "system";
  content: string;
  created_at?: string | null;
  response_truncated?: boolean;
  artifacts?: PlannerArtifact[];
  large_artifacts?: boolean;
}

export interface PlannerChatSession {
  session_id: string;
  workflow_id: string;
  planner_agent_id: string;
  agent_workflow: AgentWorkflowSpec;
  repo?: string | null;
  scopes: string[];
  knowledge_pack_ids: string[];
  skill_pack_ids: string[];
  memory_pack_ids: string[];
  interaction_mode: PlannerInteractionMode;
  messages: PlannerChatMessage[];
  task_state: PlannerTaskState;
  generation: number;
  last_turn?: PlannerChatTurn | null;
  run_id?: string | null;
  work_in_progress: boolean;
  status: "chatting" | "ready" | "running" | "completed" | "blocked";
}

export interface PlannerChatTurnResponse {
  session_id: string;
  generation: number;
  status: PlannerChatSession["status"];
  run_id?: string | null;
  turn: PlannerChatTurn;
  session: PlannerChatSession;
  ready_for_start_work?: boolean;
  missing_information?: string[];
  concise_plan_summary?: string;
  response_truncated?: boolean;
  artifacts?: PlannerArtifact[];
  structured_artifacts?: PlannerArtifact[];
  large_artifacts?: boolean;
  provider_trace?: PlannerProviderTrace | null;
}

export interface PlannerStartWorkResponse {
  session: PlannerChatSession;
  assistant_message?: string | null;
  run_id?: string | null;
  status: string;
  events_url?: string | null;
  timeline_url?: string | null;
}

export type TimelineItem =
  | ({ type: "user_message" | "planner_message" } & MessageTimelineItem)
  | ({ type: "reasoning_summary" } & ReasoningSummaryItem)
  | ({ type: "plan_update" } & PlanUpdateItem)
  | ({ type: "executor_step" } & ExecutorStepItem)
  | ({ type: "tool_call" } & ToolCallItem)
  | ({ type: "command_execution" } & CommandExecutionItem)
  | ({ type: "file_change" } & FileChangeItem)
  | ({ type: "approval" } & ApprovalItem)
  | ({ type: "verification" } & VerificationItem)
  | ({ type: "final_summary" } & FinalSummaryItem);

export interface MessageTimelineItem {
  id: string;
  agent_id: string;
  content: string;
  created_at: string;
}

export interface ReasoningSummaryItem {
  id: string;
  agent_id: string;
  summary_text: string[];
  created_at: string;
}

export interface PlanUpdateItem {
  id: string;
  agent_id: string;
  title: string;
  summary: string;
  created_at: string;
}

export interface ExecutorStepItem {
  id: string;
  agent_id: string;
  title: string;
  status: string;
  summary?: string | null;
  created_at: string;
}

export interface ToolCallItem {
  id: string;
  agent_id: string;
  tool_name: string;
  status: string;
  summary?: string | null;
  evidence_ref?: string | null;
  created_at: string;
}

export interface CommandExecutionItem {
  id: string;
  agent_id: string;
  command: string[];
  cwd: string;
  status: string;
  stdout_preview?: string | null;
  stderr_preview?: string | null;
  exit_code?: number | null;
  duration_ms?: number | null;
  evidence_ref?: string | null;
  created_at: string;
}

export interface FileChangeItem {
  id: string;
  agent_id: string;
  path: string;
  change_type: string;
  diff_ref?: string | null;
  created_at: string;
}

export interface ApprovalItem {
  id: string;
  agent_id: string;
  risk_level: string;
  action_type: string;
  summary: string;
  status: string;
  created_at: string;
}

export interface VerificationItem {
  id: string;
  agent_id: string;
  status: string;
  summary: string;
  evidence_ref?: string | null;
  created_at: string;
}

export interface FinalSummaryItem {
  id: string;
  agent_id: string;
  status: string;
  summary: string;
  changed_files: string[];
  checks: string[];
  evidence_refs: RustEvidenceRef[];
  blockers: string[];
  next_steps: string[];
  created_at: string;
}

export interface RunTimelineResponse {
  run_id: string;
  items: TimelineItem[];
  event_count?: number;
  returned_count?: number;
  truncated?: boolean;
  next_after_sequence?: number | null;
}

export type ChangeSetStatus = "pending_review" | "accepted" | "undone" | "failed_to_undo" | string;

export interface ChangedFileSummary {
  path: string;
  change_type: string;
  additions?: number | null;
  deletions?: number | null;
}

export interface CommandCheckSummary {
  command: string;
  status: string;
  exit_code?: number | null;
}

export interface ChangeSet {
  change_set_id: string;
  run_id: string;
  repo_root: string;
  status: ChangeSetStatus;
  created_at: string;
  base_git_head?: string | null;
  before_checkpoint_ref?: string | null;
  after_diff_ref?: string | null;
  reverse_patch_ref?: string | null;
  changed_files: ChangedFileSummary[];
  command_checks: CommandCheckSummary[];
  evidence_refs: RustEvidenceRef[];
  after_diff: string;
  diff_truncated: boolean;
  undo_conflict?: string | null;
}

export interface RunChangeSetListResponse {
  run_id: string;
  changes: ChangeSet[];
}

export interface ChangeSetDiffResponse {
  run_id: string;
  change_set_id: string;
  diff: string;
  truncated: boolean;
}

export interface ChangeSetActionResponse {
  run_id: string;
  change_set: ChangeSet;
  status: string;
  message: string;
}

export interface WorkflowActivityStep {
  id: string;
  label: string;
  status: "done" | "active" | "pending" | "blocked" | "failed";
}

export interface WorkflowActivityUpdate {
  artifact_id?: string | null;
  artifact_type: "workflow_activity_update";
  visible_phase: "planning" | "assigning_work" | "executing" | "checking" | "summarizing" | "completed" | "blocked" | "failed";
  user_message: string;
  steps: WorkflowActivityStep[];
  safety: Record<string, string>[];
  technical_refs: Record<string, unknown>;
}

export interface ProviderKeyState {
  configured: boolean;
  source: string;
}

export interface ProviderSettings {
  default_provider: string;
  default_model: string;
  base_urls: Record<string, string>;
  proxy_urls: Record<string, string>;
  proxy_modes: Record<string, string>;
  api_keys: Record<string, ProviderKeyState>;
  mock_mode: boolean;
}

export interface ProviderStatusItem {
  provider: string;
  configured: boolean;
  credential_configured: boolean;
  credential_source: string;
  base_url?: string | null;
  proxy_url?: string | null;
  proxy_mode: string;
  mode: string;
}

export interface ProviderStatus {
  default_provider: string;
  default_model: string;
  mock_mode: boolean;
  default_status: ProviderStatusItem;
  providers: ProviderStatusItem[];
}

export interface ProviderTestResult {
  provider: string;
  ok: boolean;
  mode: string;
  model: string;
  endpoint?: string | null;
  message: string;
}

export interface ProviderFormState {
  default_provider: string;
  default_model: string;
  base_url: string;
  proxy_mode: "direct" | "explicit" | "environment" | string;
  proxy_url: string;
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
  extension_type: "plugin" | "skill" | "harness_runtime" | string;
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

export interface PluginMarketplace {
  name: string;
  url: string;
  enabled: boolean;
}

export interface PluginMarketplaceListResponse {
  marketplaces: PluginMarketplace[];
}

export interface HookSummary {
  id: string;
  trigger: string;
  enabled: boolean;
  description: string;
}

export interface PluginReadResponse {
  plugin: PluginManifest;
  skills: RemoteSkillEntry[];
  mcp_dependencies: unknown[];
  hooks: HookSummary[];
}

export interface SkillExtraRoot {
  path: string;
  scope: string;
  enabled: boolean;
}

export interface SkillExtraRootsResponse {
  roots: SkillExtraRoot[];
}

export interface CacheBucketStatus {
  entries: number;
  bytes: number;
  stale: boolean;
  scanned_entries: number;
  entry_scan_limit: number;
  truncated: boolean;
}

export interface BrowserVerifierRuntimeCandidateStatus {
  source: string;
  path: string;
  path_exists: boolean;
  has_playwright_package: boolean;
}

export interface BrowserVerifierCacheStatus {
  status: string;
  runtime_root: string;
  browsers_path: string;
  runtime_cache: CacheBucketStatus;
  node_path?: string | null;
  resolved_node_modules?: string | null;
  candidates: BrowserVerifierRuntimeCandidateStatus[];
  candidate_count: number;
  message: string;
}

export interface CacheStatusResponse {
  repo_index: CacheBucketStatus;
  plugin_cache: CacheBucketStatus;
  skill_cache: CacheBucketStatus;
  blob_store: CacheBucketStatus;
  browser_verifier: BrowserVerifierCacheStatus;
}
