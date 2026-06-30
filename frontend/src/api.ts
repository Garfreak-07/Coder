import type {
  AgentWorkflowValidationResult,
  AgentWorkflowSpec,
  ArtifactDetail,
  BlobDetail,
  CapabilitySpec,
  ContextPacketDetail,
  HealthStatus,
  DiscoverSkillsPayload,
  ExtensionManifest,
  InstalledSkillsPayload,
  LibraryIndex,
  LiveRunDetail,
  CacheStatusResponse,
  ChangeSetActionResponse,
  ChangeSetDiffResponse,
  HookSummary,
  PlannerChatSession,
  PlannerStartWorkResponse,
  PlannerChatTurn,
  PlannerChatTurnResponse,
  PlannerInteractionMode,
  PluginMarketplace,
  PluginMarketplaceListResponse,
  PluginReadResponse,
  ProviderSettings,
  ProviderStatus,
  ProviderTestResult,
  RunChangeSetListResponse,
  PluginManifest,
  RoleCardSpec,
  RunEvent,
  RunEventsPage,
  RunTimelineResponse,
  RunSummaryItem,
  RustRepoEvidenceResponse,
  RustCommandPreview,
  RustCommandPreviewRequest,
  RustPatchApplyRequest,
  RustPatchApplyResponse,
  RustPatchPreview,
  RustPatchPreviewRequest,
  RustMcpManifestValidation,
  RustMcpManifestValidationRequest,
  RustMcpServerListResponse,
  RustMcpToolCallRequest,
  RustMcpToolCallResult,
  RustMcpToolListResponse,
  RustKnowledgeRetrieveRequest,
  RustKnowledgeRetrieveResponse,
  RustProjectMemoryLoadRequest,
  RustProjectMemoryLoadResponse,
  RustProjectMemoryWriteProposalRequest,
  RustProjectMemoryWriteProposalResponse,
  RustRunArtifactResponse,
  RustRunCheckpointListResponse,
  RustRunCheckpointResponse,
  RustRunCheckpointWriteResponse,
  RustRunControlResponse,
  RustRunDetail,
  RustRunEventsResponse,
  RustRunHeartbeatResponse,
  RustRunListResponse,
  RustRunReportResponse,
  RustRunRepoEvidenceResponse,
  RustRunSummary,
  RustExtensionPluginListResponse,
  RustExtensionPluginValidationRequest,
  RustProjectConfig,
  RustPluginManifestValidation,
  RustToolRegistryResponse,
  RustValidationReport,
  SkillUpdateInfo,
  StoredRunDetail,
  ToolResultDetail
} from "./types";
import {
  agentWorkflowToRustLibrarySaveRequest,
  rustArtifactPayloadToArtifactDetail,
  rustBlobToBlobDetail,
  rustCapabilitiesToCapabilitySpecs,
  rustDefaultWorkflowToAgentWorkflow,
  rustHealthToHealthStatus,
  rustLibraryToLibraryIndex,
  rustLibraryWorkflowToAgentWorkflow,
  rustRoleCardsToRoleCards,
  rustRunDetailToStoredRunDetail,
  rustRunEventsToRunEventsPage,
  rustRunReportToArtifactDetail,
  rustRunSummaryToRunSummaryItem,
  rustValidationReportToAgentWorkflowValidationResult,
  type RustCapabilitiesResponse,
  type RustDefaultWorkflowResponse,
  type RustHealthResponse,
  type RustLibraryResponse,
  type RustLibraryWorkflowGetResponse
} from "./rustApiAdapter";
import { legacyCanvasToWorkflowSpec } from "./workflowSpecAdapter";

const jsonHeaders = {
  "Content-Type": "application/json"
};

interface RustPlannerChatSession {
  session_id: string;
  workflow_id: string;
  mode: PlannerInteractionMode | string;
  ready: boolean;
  readiness?: "ready" | "needs_clarification" | "blocked" | "casual" | string;
  plan_draft?: RustPlannerPlanDraft | null;
  open_questions?: string[];
  acceptance_criteria?: string[];
  risks?: string[];
  turns: Array<{
    role: string;
    content: string;
  }>;
}

interface RustPlannerPlanDraft {
  goal: string;
  scope?: string[];
  non_goals?: string[];
  assumptions?: string[];
  steps?: string[];
  affected_paths?: string[];
  acceptance_criteria?: string[];
  risks?: string[];
  open_questions?: string[];
  selected_workflow_id: string;
  memory_proposals?: Array<{
    scope: string;
    key: string;
    content: string;
    rationale: string;
    requires_confirmation: boolean;
  }>;
}

interface RustPlannerChatSessionResponse {
  session: RustPlannerChatSession;
}

interface RustPlannerChatTurnResponse {
  session: RustPlannerChatSession;
  assistant_message: string;
  plan_draft?: RustPlannerPlanDraft | null;
  readiness?: "ready" | "needs_clarification" | "blocked" | "casual" | string;
  open_questions?: string[];
  acceptance_criteria?: string[];
  risks?: string[];
  suggested_mode?: PlannerInteractionMode | string;
  should_start_workflow?: boolean;
  ready: boolean;
  execution_allowed: boolean;
  events?: Array<Record<string, unknown>>;
  run_preview?: {
    status?: string;
    requires_confirmation?: boolean;
    workflow_id?: string;
  } | null;
}

interface RustRunResponse {
  run_id: string;
  report_ref?: string;
  report?: {
    status?: string;
  };
  events_url?: string;
}

interface PlannerSessionContext {
  repo?: string | null;
  workflowId: string;
  plannerAgentId: string;
  agentWorkflow: AgentWorkflowSpec;
  scopes: string[];
  knowledgePackIds: string[];
  skillPackIds: string[];
  memoryPackIds: string[];
}

const rustPlannerSessionContexts = new Map<string, PlannerSessionContext>();

async function requestJson<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, init);
  if (!response.ok) {
    const detail = await response.text();
    throw new Error(`${response.status} ${response.statusText}: ${detail}`);
  }
  return (await response.json()) as T;
}

async function requestBlob(url: string, init?: RequestInit): Promise<Blob> {
  const response = await fetch(url, init);
  if (!response.ok) {
    const detail = await response.text();
    throw new Error(`${response.status} ${response.statusText}: ${detail}`);
  }
  return response.blob();
}

export async function getLibrary(): Promise<LibraryIndex> {
  const payload = await requestJson<RustLibraryResponse>("/api/v3/library");
  return rustLibraryToLibraryIndex(payload);
}

export async function getHealth(): Promise<HealthStatus> {
  const payload = await requestJson<RustHealthResponse>("/api/v3/health");
  return rustHealthToHealthStatus(payload);
}

export async function getCapabilities(): Promise<CapabilitySpec[]> {
  const payload = await requestJson<RustCapabilitiesResponse>("/api/v3/capabilities");
  return rustCapabilitiesToCapabilitySpecs(payload);
}

export async function getAgentRoleCards(): Promise<RoleCardSpec[]> {
  const payload = await requestJson<{ role_cards: RoleCardSpec[] }>("/api/v3/agent-role-cards");
  return rustRoleCardsToRoleCards(payload);
}

export function getInstalledSkills(): Promise<InstalledSkillsPayload> {
  return requestJson<InstalledSkillsPayload>("/api/v3/skills/installed");
}

export async function getExtensionPlugins(): Promise<PluginManifest[]> {
  const payload = await requestJson<{ plugins: PluginManifest[] }>("/api/v3/extensions/plugins");
  return payload.plugins;
}

export async function searchExtensions(query: string): Promise<ExtensionManifest[]> {
  const payload = await requestJson<{ extensions: ExtensionManifest[] }>(`/api/v3/extensions/search?q=${encodeURIComponent(query)}`);
  return payload.extensions;
}

export function discoverSkills(registryUrl: string): Promise<DiscoverSkillsPayload> {
  return requestJson<DiscoverSkillsPayload>(`/api/v3/skills/discover?registry_url=${encodeURIComponent(registryUrl)}`);
}

export function getSkillUpdates(registryUrl: string): Promise<{ updates: SkillUpdateInfo[] }> {
  return requestJson<{ updates: SkillUpdateInfo[] }>(`/api/v3/skills/updates?registry_url=${encodeURIComponent(registryUrl)}`);
}

export function installSkill(skillId: string, registryUrl: string): Promise<Record<string, unknown>> {
  return requestJson("/api/v3/skills/install", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ skill_id: skillId, registry_url: registryUrl })
  });
}

export function updateSkill(skillId: string, registryUrl: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/update`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ registry_url: registryUrl })
  });
}

export function autoUpdateSkills(registryUrl: string): Promise<Record<string, unknown>> {
  return requestJson("/api/v3/skills/auto-update", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ registry_url: registryUrl })
  });
}

export function enableSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/enable`, {
    method: "POST",
    headers: jsonHeaders
  });
}

export function disableSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/disable`, {
    method: "POST",
    headers: jsonHeaders
  });
}

export function removeSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}`, {
    method: "DELETE"
  });
}

export function pinSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/pin`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({})
  });
}

export function unpinSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/unpin`, {
    method: "POST",
    headers: jsonHeaders
  });
}

export function rollbackSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/rollback`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({})
  });
}

export function setSkillUpdatePolicy(skillId: string, updatePolicy: "manual" | "auto_official_low_risk"): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/update-policy`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ update_policy: updatePolicy })
  });
}

export function importDeveloperSkill(path: string): Promise<Record<string, unknown>> {
  return requestJson("/api/v3/skills/developer-import", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ path })
  });
}

export async function getProviderSettings(): Promise<ProviderSettings> {
  const payload = await requestJson<{ settings: ProviderSettings }>("/api/v3/providers/settings");
  return payload.settings;
}

export function getProviderStatus(): Promise<ProviderStatus> {
  return requestJson<ProviderStatus>("/api/v3/providers/status");
}

export async function saveProviderSettings(input: Record<string, unknown>): Promise<{
  settings: ProviderSettings;
  status: ProviderStatus;
}> {
  return requestJson("/api/v3/providers/settings", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(input)
  });
}

export async function testProvider(provider: string): Promise<{
  status: ProviderStatus;
  test: ProviderTestResult;
}> {
  return requestJson<{ status: ProviderStatus; test: ProviderTestResult }>("/api/v3/providers/test", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ provider })
  });
}

export async function getRuns(): Promise<RunSummaryItem[]> {
  const runs = await getRustRuns();
  return runs.map(rustRunSummaryToRunSummaryItem);
}

export async function getRun(runId: string, includeEvents = true): Promise<StoredRunDetail> {
  const detail = await getRustRun(runId);
  const mapped = rustRunDetailToStoredRunDetail(detail);
  if (!includeEvents) {
    return {
      ...mapped,
      result: {
        ...mapped.result,
        events: []
      }
    };
  }
  return mapped;
}

export async function getRunEvents(runId: string, cursor = 0, limit = 200): Promise<RunEventsPage> {
  const payload = await getRustRunEvents(runId);
  return rustRunEventsToRunEventsPage(payload, cursor, limit);
}

export function getRunTimeline(runId: string): Promise<RunTimelineResponse> {
  return requestJson<RunTimelineResponse>(`/api/v3/runs/${encodeURIComponent(runId)}/timeline`);
}

export function getRunChangeSets(runId: string): Promise<RunChangeSetListResponse> {
  return requestJson<RunChangeSetListResponse>(`/api/v3/runs/${encodeURIComponent(runId)}/changes`);
}

export function getChangeSetDiff(runId: string, changeSetId: string): Promise<ChangeSetDiffResponse> {
  return requestJson<ChangeSetDiffResponse>(
    `/api/v3/runs/${encodeURIComponent(runId)}/changes/${encodeURIComponent(changeSetId)}/diff`
  );
}

export function acceptChangeSet(runId: string, changeSetId: string): Promise<ChangeSetActionResponse> {
  return requestJson<ChangeSetActionResponse>(
    `/api/v3/runs/${encodeURIComponent(runId)}/changes/${encodeURIComponent(changeSetId)}/accept`,
    {
      method: "POST",
      headers: jsonHeaders
    }
  );
}

export function undoChangeSet(runId: string, changeSetId: string): Promise<ChangeSetActionResponse> {
  return requestJson<ChangeSetActionResponse>(
    `/api/v3/runs/${encodeURIComponent(runId)}/changes/${encodeURIComponent(changeSetId)}/undo`,
    {
      method: "POST",
      headers: jsonHeaders
    }
  );
}

export function getPluginMarketplaces(): Promise<PluginMarketplaceListResponse> {
  return requestJson<PluginMarketplaceListResponse>("/api/v3/plugins/marketplaces");
}

export function addPluginMarketplace(input: {
  name: string;
  url: string;
  enabled?: boolean;
}): Promise<{ status: string; marketplace: PluginMarketplace }> {
  return requestJson("/api/v3/plugins/marketplaces", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(input)
  });
}

export function getPlugins(): Promise<{ plugins: PluginManifest[] }> {
  return requestJson<{ plugins: PluginManifest[] }>("/api/v3/plugins");
}

export function getInstalledPlugins(): Promise<{ plugins: PluginManifest[] }> {
  return requestJson<{ plugins: PluginManifest[] }>("/api/v3/plugins/installed");
}

export function getPlugin(pluginId: string): Promise<PluginReadResponse> {
  return requestJson<PluginReadResponse>(`/api/v3/plugins/${encodeURIComponent(pluginId)}`);
}

export function getSkillExtraRoots(): Promise<{ roots: Array<{ path: string; scope: string; enabled: boolean }> }> {
  return requestJson("/api/v3/skills/extra-roots");
}

export function getHooks(): Promise<{ hooks: HookSummary[] }> {
  return requestJson<{ hooks: HookSummary[] }>("/api/v3/hooks");
}

export function getCacheStatus(): Promise<CacheStatusResponse> {
  return requestJson<CacheStatusResponse>("/api/v3/cache/status");
}

export function getContextPacket(runId: string, packetId: string): Promise<ContextPacketDetail> {
  void runId;
  void packetId;
  return Promise.reject(new Error("External context-packet lookup is not exposed by Rust API v3."));
}

export async function getArtifact(runId: string, artifactId: string): Promise<ArtifactDetail> {
  if (artifactId === "final-report.json" || artifactId === "final_report") {
    const report = await previewRustRunReport(runId);
    return rustRunReportToArtifactDetail(report);
  }
  const response = await getRustRunArtifact(runId, artifactId);
  return rustArtifactPayloadToArtifactDetail(response.artifact_name, response.payload);
}

export function getToolResult(runId: string, toolResultId: string): Promise<ToolResultDetail> {
  void runId;
  void toolResultId;
  return Promise.reject(new Error("External tool-result lookup is not exposed by Rust API v3."));
}

export async function getBlob(runId: string, blobId: string): Promise<BlobDetail> {
  void runId;
  const digest = blobId.startsWith("blob://sha256/") ? blobId.slice("blob://sha256/".length) : blobId;
  const blob = await getRustBlobSha256(digest);
  return rustBlobToBlobDetail(blobId, blob);
}

export async function getLiveAgentRun(runId: string): Promise<LiveRunDetail> {
  const detail = await getRun(runId);
  return {
    id: detail.id,
    workflow_id: detail.workflow_id,
    repo_root: detail.repo_root,
    request: detail.request,
    status: detail.result.status,
    events: detail.result.events,
    result: detail.result,
    stored_run_id: detail.id
  };
}

export async function getDefaultAgentWorkflow(): Promise<{
  agent_workflow: AgentWorkflowSpec;
}> {
  const payload = await requestJson<RustDefaultWorkflowResponse>("/api/v3/workflows/default");
  return { agent_workflow: rustDefaultWorkflowToAgentWorkflow(payload) };
}

export async function validateAgentWorkflow(agentWorkflow: AgentWorkflowSpec): Promise<AgentWorkflowValidationResult> {
  const config = legacyCanvasToWorkflowSpec(agentWorkflow);
  const report = await validateRustWorkflowSpec(config, agentWorkflow.id);
  return rustValidationReportToAgentWorkflowValidationResult(report);
}

export function validateRustWorkflowSpec(config: RustProjectConfig, workflowId: string): Promise<RustValidationReport> {
  return requestJson<RustValidationReport>("/api/v3/workflows/validate", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ config, workflow_id: workflowId })
  });
}

export function previewRustCommand(request: RustCommandPreviewRequest): Promise<RustCommandPreview> {
  return requestJson<RustCommandPreview>("/api/v3/tools/command/preview", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(request)
  });
}

export function previewRustPatch(request: RustPatchPreviewRequest): Promise<RustPatchPreview> {
  return requestJson<RustPatchPreview>("/api/v3/tools/patch/preview", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(request)
  });
}

export function applyRustPatch(request: RustPatchApplyRequest): Promise<RustPatchApplyResponse> {
  return requestJson<RustPatchApplyResponse>("/api/v3/tools/patch/apply", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(request)
  });
}

export function loadRustProjectMemory(
  request: RustProjectMemoryLoadRequest
): Promise<RustProjectMemoryLoadResponse> {
  return requestJson<RustProjectMemoryLoadResponse>("/api/v3/memory/project/load", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(request)
  });
}

export function proposeRustProjectMemoryWrite(
  request: RustProjectMemoryWriteProposalRequest
): Promise<RustProjectMemoryWriteProposalResponse> {
  return requestJson<RustProjectMemoryWriteProposalResponse>(
    "/api/v3/memory/project/propose-write",
    {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify(request)
    }
  );
}

export function retrieveRustKnowledge(
  request: RustKnowledgeRetrieveRequest
): Promise<RustKnowledgeRetrieveResponse> {
  return requestJson<RustKnowledgeRetrieveResponse>("/api/v3/knowledge/retrieve", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(request)
  });
}

export function validateRustMcpManifest(
  request: RustMcpManifestValidationRequest
): Promise<RustMcpManifestValidation> {
  return requestJson<RustMcpManifestValidation>("/api/v3/mcp/manifests/validate", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(request)
  });
}

export function validateRustMcpServerManifest(
  request: RustMcpManifestValidationRequest
): Promise<RustMcpManifestValidation> {
  return requestJson<RustMcpManifestValidation>("/api/v3/mcp/servers/validate", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(request)
  });
}

export function getRustMcpServers(): Promise<RustMcpServerListResponse> {
  return requestJson<RustMcpServerListResponse>("/api/v3/mcp/servers");
}

export function getRustMcpTools(): Promise<RustMcpToolListResponse> {
  return requestJson<RustMcpToolListResponse>("/api/v3/mcp/tools");
}

export function invokeRustMcpTool(
  request: RustMcpToolCallRequest
): Promise<RustMcpToolCallResult> {
  return requestJson<RustMcpToolCallResult>("/api/v3/mcp/tools/invoke", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(request)
  });
}

export function getRustHarnessTools(harnessId?: string | null): Promise<RustToolRegistryResponse> {
  const query = harnessId ? `?harness_id=${encodeURIComponent(harnessId)}` : "";
  return requestJson<RustToolRegistryResponse>(`/api/v3/harness/tools${query}`);
}

export function getRustExtensionPlugins(): Promise<RustExtensionPluginListResponse> {
  return requestJson<RustExtensionPluginListResponse>("/api/v3/extensions/plugins");
}

export function validateRustExtensionPlugin(
  request: RustExtensionPluginValidationRequest
): Promise<RustPluginManifestValidation> {
  return requestJson<RustPluginManifestValidation>("/api/v3/extensions/plugins/validate", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(request)
  });
}

export async function getRustRuns(): Promise<RustRunSummary[]> {
  const payload = await requestJson<RustRunListResponse>("/api/v3/runs");
  return payload.runs;
}

export function getRustRun(runId: string): Promise<RustRunDetail> {
  return requestJson<RustRunDetail>(`/api/v3/runs/${encodeURIComponent(runId)}`);
}

export function getRustRunEvents(runId: string): Promise<RustRunEventsResponse> {
  return requestJson<RustRunEventsResponse>(`/api/v3/runs/${encodeURIComponent(runId)}/events`);
}

export function getRustRunHeartbeat(runId: string): Promise<RustRunHeartbeatResponse> {
  return requestJson<RustRunHeartbeatResponse>(
    `/api/v3/runs/${encodeURIComponent(runId)}/heartbeat`
  );
}

export function pauseRustRun(runId: string): Promise<RustRunControlResponse> {
  return requestJson<RustRunControlResponse>(`/api/v3/runs/${encodeURIComponent(runId)}/pause`, {
    method: "POST"
  });
}

export function resumeRustRun(runId: string): Promise<RustRunControlResponse> {
  return requestJson<RustRunControlResponse>(`/api/v3/runs/${encodeURIComponent(runId)}/resume`, {
    method: "POST"
  });
}

export function cancelRustRun(runId: string): Promise<RustRunControlResponse> {
  return requestJson<RustRunControlResponse>(`/api/v3/runs/${encodeURIComponent(runId)}/cancel`, {
    method: "POST"
  });
}

export function getRustRunRepoEvidence(runId: string): Promise<RustRunRepoEvidenceResponse> {
  return requestJson<RustRunRepoEvidenceResponse>(
    `/api/v3/runs/${encodeURIComponent(runId)}/repo-evidence`
  );
}

export function previewRustRunReport(runId: string): Promise<RustRunReportResponse> {
  return requestJson<RustRunReportResponse>(
    `/api/v3/runs/${encodeURIComponent(runId)}/report/preview`
  );
}

export function writeRustRunReport(runId: string): Promise<RustRunReportResponse> {
  return requestJson<RustRunReportResponse>(`/api/v3/runs/${encodeURIComponent(runId)}/report`, {
    method: "POST"
  });
}

export function getRustRunArtifact(runId: string, artifactName: string): Promise<RustRunArtifactResponse> {
  return requestJson<RustRunArtifactResponse>(
    `/api/v3/runs/${encodeURIComponent(runId)}/artifacts/${encodeURIComponent(artifactName)}`
  );
}

export function getRustRunCheckpoints(runId: string): Promise<RustRunCheckpointListResponse> {
  return requestJson<RustRunCheckpointListResponse>(
    `/api/v3/runs/${encodeURIComponent(runId)}/checkpoints`
  );
}

export function getRustRunCheckpoint(
  runId: string,
  checkpointName: string
): Promise<RustRunCheckpointResponse> {
  return requestJson<RustRunCheckpointResponse>(
    `/api/v3/runs/${encodeURIComponent(runId)}/checkpoints/${encodeURIComponent(checkpointName)}`
  );
}

export function writeRustRunCheckpoint(
  runId: string,
  checkpointName: string,
  payload: unknown
): Promise<RustRunCheckpointWriteResponse> {
  return requestJson<RustRunCheckpointWriteResponse>(
    `/api/v3/runs/${encodeURIComponent(runId)}/checkpoints/${encodeURIComponent(checkpointName)}`,
    {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify(payload)
    }
  );
}

export function getRustRepoEvidence(refId: string): Promise<RustRepoEvidenceResponse> {
  return requestJson<RustRepoEvidenceResponse>(`/api/v3/repo-evidence/${encodeURIComponent(refId)}`);
}

export function getRustBlobSha256(digest: string): Promise<Blob> {
  return requestBlob(`/api/v3/blobs/sha256/${encodeURIComponent(digest)}`);
}

export async function getAgentWorkflow(workflowId: string): Promise<AgentWorkflowSpec> {
  const payload = await requestJson<RustLibraryWorkflowGetResponse>(
    `/api/v3/library/workflows/${encodeURIComponent(workflowId)}`
  );
  return rustLibraryWorkflowToAgentWorkflow(payload);
}

export async function saveAgentWorkflow(agentWorkflow: AgentWorkflowSpec): Promise<AgentWorkflowSpec> {
  const request = agentWorkflowToRustLibrarySaveRequest(agentWorkflow);
  const payload = await requestJson<{ workflow_id: string; workflow: unknown; saved: boolean }>(
    "/api/v3/library/workflows",
    {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify(request)
    }
  );
  return rustLibraryWorkflowToAgentWorkflow(payload);
}

export function createPlannerChatSession(input: {
  repo?: string;
  workflow_id: string;
  planner_agent_id: string;
  agent_workflow: AgentWorkflowSpec;
  scopes: string[];
  knowledge_pack_ids?: string[];
  skill_pack_ids?: string[];
  memory_pack_ids?: string[];
}): Promise<PlannerChatSession> {
  return createRustPlannerChatSession(input);
}

export function sendPlannerChatTurn(input: {
  session_id: string;
  message: string;
  repo?: string;
  workflow_id?: string;
  planner_agent_id?: string;
  agent_workflow?: AgentWorkflowSpec;
  scopes?: string[];
  knowledge_pack_ids?: string[];
  skill_pack_ids?: string[];
  memory_pack_ids?: string[];
}): Promise<PlannerChatTurnResponse> {
  return sendRustPlannerChatTurn(input);
}

export function startPlannerSessionWork(input: {
  session_id: string;
  repo?: string;
  workflow_id: string;
  planner_agent_id: string;
  agent_workflow: AgentWorkflowSpec;
  scopes: string[];
  knowledge_pack_ids?: string[];
  skill_pack_ids?: string[];
  memory_pack_ids?: string[];
}): Promise<PlannerStartWorkResponse> {
  return requestJson<PlannerStartWorkResponse>(
    `/api/v3/planner-chat/sessions/${encodeURIComponent(input.session_id)}/start-work`,
    {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify({
        repo: input.repo,
        workflow_id: input.workflow_id,
        planner_agent_id: input.planner_agent_id,
        config: legacyCanvasToWorkflowSpec(input.agent_workflow),
        scopes: input.scopes,
        knowledge_pack_ids: input.knowledge_pack_ids ?? [],
        skill_pack_ids: input.skill_pack_ids ?? [],
        memory_pack_ids: input.memory_pack_ids ?? []
      })
    }
  );
}

export function getPlannerChatSession(sessionId: string): Promise<PlannerChatSession> {
  return getRustPlannerChatSession(sessionId);
}

export async function startLiveAgentRun(input: {
  repo: string;
  request: string;
  agent_workflow: AgentWorkflowSpec;
  approved: boolean;
  scopes: string[];
  initial_data?: Record<string, unknown>;
}): Promise<{ run_id: string; status: string; events_url: string; result_url: string }> {
  return startRustAgentRun(input);
}

async function createRustPlannerChatSession(input: {
  repo?: string;
  workflow_id: string;
  planner_agent_id: string;
  agent_workflow: AgentWorkflowSpec;
  scopes: string[];
  knowledge_pack_ids?: string[];
  skill_pack_ids?: string[];
  memory_pack_ids?: string[];
}): Promise<PlannerChatSession> {
  const payload = await requestJson<RustPlannerChatSessionResponse>("/api/v3/planner-chat/sessions", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({
      workflow_id: input.workflow_id,
      planner_agent_id: input.planner_agent_id,
      config: legacyCanvasToWorkflowSpec(input.agent_workflow),
      mode: "discuss"
    })
  });
  const context = plannerSessionContextFromCreateInput(input);
  rustPlannerSessionContexts.set(payload.session.session_id, context);
  return mapRustPlannerSession(payload.session, context);
}

async function getRustPlannerChatSession(sessionId: string): Promise<PlannerChatSession> {
  const payload = await requestJson<RustPlannerChatSessionResponse>(
    `/api/v3/planner-chat/sessions/${encodeURIComponent(sessionId)}`
  );
  return mapRustPlannerSession(payload.session, rustPlannerSessionContexts.get(sessionId));
}

async function sendRustPlannerChatTurn(input: {
  session_id: string;
  message: string;
  repo?: string;
  workflow_id?: string;
  planner_agent_id?: string;
  agent_workflow?: AgentWorkflowSpec;
  scopes?: string[];
  knowledge_pack_ids?: string[];
  skill_pack_ids?: string[];
  memory_pack_ids?: string[];
}): Promise<PlannerChatTurnResponse> {
  const explicitContext = plannerSessionContextFromTurnInput(input);
  if (explicitContext) {
    rustPlannerSessionContexts.set(input.session_id, explicitContext);
  }
  const payload = await requestJson<RustPlannerChatTurnResponse>(
    `/api/v3/planner-chat/sessions/${encodeURIComponent(input.session_id)}/turn`,
    {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify({
        message: input.message,
        confirmed: false,
        mode: "discuss",
        planner_agent_id: input.planner_agent_id,
        config: input.agent_workflow ? legacyCanvasToWorkflowSpec(input.agent_workflow) : undefined
      })
    }
  );
  const context = explicitContext ?? rustPlannerSessionContexts.get(input.session_id);
  const mappedSession = mapRustPlannerSession(payload.session, context);
  const turn = mapRustPlannerTurn(payload, input.message, context);
  return {
    session_id: input.session_id,
    generation: mappedSession.generation,
    status: mappedSession.status,
    run_id: null,
    turn,
    session: {
      ...mappedSession,
      last_turn: turn,
      run_id: null,
      status: mappedSession.status
    }
  };
}

async function startRustAgentRun(input: {
  repo?: string;
  request: string;
  agent_workflow: AgentWorkflowSpec;
}): Promise<{ run_id: string; status: string; events_url: string; result_url: string }> {
  const config = legacyCanvasToWorkflowSpec(input.agent_workflow);
  const response = await requestJson<RustRunResponse>("/api/v3/runs", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({
      config,
      workflow_id: input.agent_workflow.id,
      task: input.request,
      repo_root: input.repo ?? ".",
      plan_context: {
        original_user_request: input.request,
        planner_conversation_summary: "Run started from direct agent run entry point.",
        plan_draft: null,
        acceptance_criteria: ["Run completes with an evidence-backed final report."],
        risks: [],
        affected_paths: [],
        selected_workflow_id: input.agent_workflow.id
      }
    })
  });
  return {
    run_id: response.run_id,
    status: response.report?.status ?? "completed",
    events_url: response.events_url ?? `/api/v3/runs/${encodeURIComponent(response.run_id)}/events`,
    result_url: `/api/v3/runs/${encodeURIComponent(response.run_id)}`
  };
}

function plannerSessionContextFromCreateInput(input: {
  repo?: string;
  workflow_id: string;
  planner_agent_id: string;
  agent_workflow: AgentWorkflowSpec;
  scopes: string[];
  knowledge_pack_ids?: string[];
  skill_pack_ids?: string[];
  memory_pack_ids?: string[];
}): PlannerSessionContext {
  return {
    repo: input.repo,
    workflowId: input.workflow_id,
    plannerAgentId: input.planner_agent_id,
    agentWorkflow: input.agent_workflow,
    scopes: input.scopes,
    knowledgePackIds: input.knowledge_pack_ids ?? [],
    skillPackIds: input.skill_pack_ids ?? [],
    memoryPackIds: input.memory_pack_ids ?? []
  };
}

function plannerSessionContextFromTurnInput(input: {
  repo?: string;
  workflow_id?: string;
  planner_agent_id?: string;
  agent_workflow?: AgentWorkflowSpec;
  scopes?: string[];
  knowledge_pack_ids?: string[];
  skill_pack_ids?: string[];
  memory_pack_ids?: string[];
}): PlannerSessionContext | null {
  if (!input.workflow_id || !input.planner_agent_id || !input.agent_workflow) {
    return null;
  }
  return {
    repo: input.repo,
    workflowId: input.workflow_id,
    plannerAgentId: input.planner_agent_id,
    agentWorkflow: input.agent_workflow,
    scopes: input.scopes ?? [],
    knowledgePackIds: input.knowledge_pack_ids ?? [],
    skillPackIds: input.skill_pack_ids ?? [],
    memoryPackIds: input.memory_pack_ids ?? []
  };
}

function mapRustPlannerSession(
  session: RustPlannerChatSession,
  context?: PlannerSessionContext
): PlannerChatSession {
  const mode = session.mode === "work" ? "work" : "discuss";
  const messages = session.turns.map((turn) => ({
    role: normalizePlannerMessageRole(turn.role),
    content: turn.content
  }));
  const latestAssistant = [...session.turns].reverse().find((turn) => turn.role === "assistant");
  const taskState = rustPlannerTaskState(session, context?.scopes ?? []);
  const ready = rustReadinessIsReady(session.readiness, session.ready);
  return {
    session_id: session.session_id,
    workflow_id: session.workflow_id,
    planner_agent_id: context?.plannerAgentId ?? "planner",
    agent_workflow: context?.agentWorkflow ?? fallbackAgentWorkflow(session.workflow_id),
    repo: context?.repo,
    scopes: context?.scopes ?? [],
    knowledge_pack_ids: context?.knowledgePackIds ?? [],
    skill_pack_ids: context?.skillPackIds ?? [],
    memory_pack_ids: context?.memoryPackIds ?? [],
    interaction_mode: mode,
    messages,
    task_state: taskState,
    generation: session.turns.length,
    last_turn: latestAssistant
      ? {
          artifact_type: "planner_chat_turn",
          assistant_message: latestAssistant.content,
          interaction_mode: mode,
          decision: ready ? "produce_plan" : session.readiness === "casual" ? "answer_without_workflow" : "continue_chat",
          visible_thinking: {
            phase: ready ? "ready_to_start" : session.readiness === "casual" ? "understanding" : "checking_readiness",
            summary: latestAssistant.content
          },
          task_state: taskState,
          handoff: ready
            ? {
                workflow_request: session.plan_draft?.goal ?? latestAssistant.content,
                scope: taskState.scope,
                success_criteria: taskState.success_criteria,
                risks: taskState.risks
              }
            : null
        }
      : null,
    run_id: null,
    status: ready ? "ready" : session.readiness === "blocked" ? "blocked" : "chatting"
  };
}

function mapRustPlannerTurn(
  response: RustPlannerChatTurnResponse,
  userMessage: string,
  context?: PlannerSessionContext
): PlannerChatTurn {
  const mode = response.session.mode === "work" ? "work" : "discuss";
  const taskState = rustPlannerTaskState(
    {
      ...response.session,
      readiness: response.readiness ?? response.session.readiness,
      plan_draft: response.plan_draft ?? response.session.plan_draft,
      open_questions: response.open_questions ?? response.session.open_questions,
      acceptance_criteria: response.acceptance_criteria ?? response.session.acceptance_criteria,
      risks: response.risks ?? response.session.risks
    },
    context?.scopes ?? []
  );
  const ready = rustReadinessIsReady(response.readiness, response.ready);
  return {
    artifact_type: "planner_chat_turn",
    assistant_message: response.assistant_message,
    interaction_mode: mode,
    decision: ready
      ? "produce_plan"
      : response.readiness === "casual"
        ? "answer_without_workflow"
        : "continue_chat",
    visible_thinking: {
      phase: ready ? "checking_readiness" : response.readiness === "casual" ? "understanding" : "understanding",
      summary: response.assistant_message
    },
    task_state: taskState,
    handoff: ready
      ? {
          workflow_request: response.plan_draft?.goal ?? userMessage,
          scope: taskState.scope,
          success_criteria: taskState.success_criteria,
          risks: taskState.risks
        }
      : null
  };
}

function rustPlannerTaskState(
  session: Pick<
    RustPlannerChatSession,
    "ready" | "readiness" | "plan_draft" | "open_questions" | "acceptance_criteria" | "risks"
  >,
  fallbackScopes: string[]
): PlannerChatSession["task_state"] {
  const plan = session.plan_draft ?? null;
  const openQuestions = session.open_questions ?? plan?.open_questions ?? [];
  const acceptanceCriteria = session.acceptance_criteria ?? plan?.acceptance_criteria ?? [];
  const risks = session.risks ?? plan?.risks ?? [];
  const memoryProposals = plan?.memory_proposals ?? [];
  const affectedPaths = plan?.affected_paths ?? [];
  const scope = plan?.scope && plan.scope.length > 0 ? plan.scope : fallbackScopes;
  return {
    goal: plan?.goal ?? null,
    user_intent: plan?.goal ?? null,
    scope,
    constraints: [],
    success_criteria: acceptanceCriteria,
    known_context: affectedPaths,
    missing_context: openQuestions,
    open_questions: openQuestions,
    assumptions: plan?.assumptions ?? [],
    risks,
    memory_proposals: memoryProposals,
    plan_steps: (plan?.steps ?? []).map((summary, index) => ({
      id: `step-${index + 1}`,
      summary,
      depends_on: index === 0 ? [] : [`step-${index}`],
      status: rustReadinessIsReady(session.readiness, session.ready) ? "ready" : "draft"
    })),
    readiness: rustPlannerReadiness(session.readiness, session.ready)
  };
}

function rustReadinessIsReady(readiness: string | undefined, fallbackReady: boolean): boolean {
  return readiness === "ready" || (!readiness && fallbackReady);
}

function rustPlannerReadiness(
  readiness: string | undefined,
  fallbackReady: boolean
): PlannerChatSession["task_state"]["readiness"] {
  if (readiness === "ready" || (!readiness && fallbackReady)) return "ready_to_execute";
  if (readiness === "needs_clarification") return "needs_clarification";
  if (readiness === "casual") return "not_ready";
  return "not_ready";
}

function normalizePlannerMessageRole(role: string): "user" | "assistant" | "system" {
  if (role === "assistant" || role === "system") return role;
  return "user";
}

function fallbackAgentWorkflow(workflowId: string): AgentWorkflowSpec {
  return {
    id: workflowId,
    version: "0.5",
    name: workflowId,
    description: "",
    primary_planner_id: "planner",
    agents: [
      {
        id: "planner",
        name: "Planner",
        role: "planner",
        model_tier: "standard",
        can_talk_to_human: true,
        capabilities: ["negotiate_contract", "make_plan"]
      },
      {
        id: "executor",
        name: "Executor",
        role: "executor",
        model_tier: "standard",
        can_talk_to_human: false,
        capabilities: ["follow_planner_order", "modify_files"]
      }
    ],
    edges: [{ from: "planner", to: "executor", handoff: "planner_order" }],
    loop_policy: {
      max_auto_rounds: 1,
      user_can_change: true
    },
    ui: { layout: {} }
  };
}

export function deleteRun(runId: string): Promise<{ run_id: string; deleted: boolean; orphan_blobs_removed: number }> {
  void runId;
  return Promise.reject(new Error("Stored run deletion is not exposed by Rust API v3."));
}

export async function rollbackPatch(input: {
  repo: string;
  snapshot_id: string;
  scopes: string[];
}): Promise<{ rollback: Record<string, unknown> }> {
  void input;
  return Promise.reject(new Error("Patch rollback is not exposed by Rust API v3."));
}
