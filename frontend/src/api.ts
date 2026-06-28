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
  PlannerChatConfirmResult,
  PlannerChatDraft,
  PlannerChatSession,
  PlannerChatTurnResponse,
  PlannerInteractionMode,
  ProviderSettings,
  ProviderStatus,
  PluginManifest,
  RoleCardSpec,
  RunEvent,
  RunEventsPage,
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
import { shouldUseRustApiV3 } from "./apiVersion";
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
  if (shouldUseRustApiV3()) {
    const payload = await requestJson<RustLibraryResponse>("/api/v3/library");
    return rustLibraryToLibraryIndex(payload);
  }
  return requestJson<LibraryIndex>("/api/v2/library");
}

export async function getHealth(): Promise<HealthStatus> {
  if (shouldUseRustApiV3()) {
    const payload = await requestJson<RustHealthResponse>("/api/v3/health");
    return rustHealthToHealthStatus(payload);
  }
  return requestJson<HealthStatus>("/api/v2/health");
}

export async function getCapabilities(): Promise<CapabilitySpec[]> {
  if (shouldUseRustApiV3()) {
    const payload = await requestJson<RustCapabilitiesResponse>("/api/v3/capabilities");
    return rustCapabilitiesToCapabilitySpecs(payload);
  }
  const payload = await requestJson<{ capabilities: CapabilitySpec[] }>("/api/v2/capabilities");
  return payload.capabilities;
}

export async function getAgentRoleCards(): Promise<RoleCardSpec[]> {
  if (shouldUseRustApiV3()) {
    const payload = await requestJson<{ role_cards: RoleCardSpec[] }>("/api/v3/agent-role-cards");
    return rustRoleCardsToRoleCards(payload);
  }
  const payload = await requestJson<{ role_cards: RoleCardSpec[] }>("/api/v2/agent-role-cards");
  return payload.role_cards;
}

export function getInstalledSkills(): Promise<InstalledSkillsPayload> {
  if (shouldUseRustApiV3()) {
    return requestJson<InstalledSkillsPayload>("/api/v3/skills/installed");
  }
  return requestJson<InstalledSkillsPayload>("/api/v2/skills/installed");
}

export async function getExtensionPlugins(): Promise<PluginManifest[]> {
  if (shouldUseRustApiV3()) {
    const payload = await requestJson<{ plugins: PluginManifest[] }>("/api/v3/extensions/plugins");
    return payload.plugins;
  }
  const payload = await requestJson<{ plugins: PluginManifest[] }>("/api/v2/extensions/plugins");
  return payload.plugins;
}

export async function searchExtensions(query: string): Promise<ExtensionManifest[]> {
  if (shouldUseRustApiV3()) {
    const payload = await requestJson<{ extensions: ExtensionManifest[] }>(`/api/v3/extensions/search?q=${encodeURIComponent(query)}`);
    return payload.extensions;
  }
  const payload = await requestJson<{ extensions: ExtensionManifest[] }>(`/api/v2/extensions/search?q=${encodeURIComponent(query)}`);
  return payload.extensions;
}

export function discoverSkills(registryUrl: string): Promise<DiscoverSkillsPayload> {
  if (shouldUseRustApiV3()) {
    return requestJson<DiscoverSkillsPayload>(`/api/v3/skills/discover?registry_url=${encodeURIComponent(registryUrl)}`);
  }
  return requestJson<DiscoverSkillsPayload>(`/api/v2/skills/discover?registry_url=${encodeURIComponent(registryUrl)}`);
}

export function getSkillUpdates(registryUrl: string): Promise<{ updates: SkillUpdateInfo[] }> {
  if (shouldUseRustApiV3()) {
    return requestJson<{ updates: SkillUpdateInfo[] }>(`/api/v3/skills/updates?registry_url=${encodeURIComponent(registryUrl)}`);
  }
  return requestJson<{ updates: SkillUpdateInfo[] }>(`/api/v2/skills/updates?registry_url=${encodeURIComponent(registryUrl)}`);
}

export function installSkill(skillId: string, registryUrl: string): Promise<Record<string, unknown>> {
  if (shouldUseRustApiV3()) {
    return requestJson("/api/v3/skills/install", {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify({ skill_id: skillId, registry_url: registryUrl })
    });
  }
  return requestJson("/api/v2/skills/install", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ skill_id: skillId, registry_url: registryUrl })
  });
}

export function updateSkill(skillId: string, registryUrl: string): Promise<Record<string, unknown>> {
  if (shouldUseRustApiV3()) {
    return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/update`, {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify({ registry_url: registryUrl })
    });
  }
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/update`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ registry_url: registryUrl })
  });
}

export function autoUpdateSkills(registryUrl: string): Promise<Record<string, unknown>> {
  if (shouldUseRustApiV3()) {
    return requestJson("/api/v3/skills/auto-update", {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify({ registry_url: registryUrl })
    });
  }
  return requestJson("/api/v2/skills/auto-update", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ registry_url: registryUrl })
  });
}

export function enableSkill(skillId: string): Promise<Record<string, unknown>> {
  if (shouldUseRustApiV3()) {
    return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/enable`, {
      method: "POST",
      headers: jsonHeaders
    });
  }
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/enable`, {
    method: "POST",
    headers: jsonHeaders
  });
}

export function disableSkill(skillId: string): Promise<Record<string, unknown>> {
  if (shouldUseRustApiV3()) {
    return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/disable`, {
      method: "POST",
      headers: jsonHeaders
    });
  }
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/disable`, {
    method: "POST",
    headers: jsonHeaders
  });
}

export function removeSkill(skillId: string): Promise<Record<string, unknown>> {
  if (shouldUseRustApiV3()) {
    return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}`, {
      method: "DELETE"
    });
  }
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}`, {
    method: "DELETE"
  });
}

export function pinSkill(skillId: string): Promise<Record<string, unknown>> {
  if (shouldUseRustApiV3()) {
    return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/pin`, {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify({})
    });
  }
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/pin`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({})
  });
}

export function unpinSkill(skillId: string): Promise<Record<string, unknown>> {
  if (shouldUseRustApiV3()) {
    return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/unpin`, {
      method: "POST",
      headers: jsonHeaders
    });
  }
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/unpin`, {
    method: "POST",
    headers: jsonHeaders
  });
}

export function rollbackSkill(skillId: string): Promise<Record<string, unknown>> {
  if (shouldUseRustApiV3()) {
    return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/rollback`, {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify({})
    });
  }
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/rollback`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({})
  });
}

export function setSkillUpdatePolicy(skillId: string, updatePolicy: "manual" | "auto_official_low_risk"): Promise<Record<string, unknown>> {
  if (shouldUseRustApiV3()) {
    return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/update-policy`, {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify({ update_policy: updatePolicy })
    });
  }
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/update-policy`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ update_policy: updatePolicy })
  });
}

export function importDeveloperSkill(path: string): Promise<Record<string, unknown>> {
  if (shouldUseRustApiV3()) {
    return requestJson("/api/v3/skills/developer-import", {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify({ path })
    });
  }
  return requestJson("/api/v2/skills/developer-import", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ path })
  });
}

export async function getProviderSettings(): Promise<ProviderSettings> {
  const payload = await requestJson<{ settings: ProviderSettings }>("/api/v2/providers/settings");
  return payload.settings;
}

export function getProviderStatus(): Promise<ProviderStatus> {
  return requestJson<ProviderStatus>("/api/v2/providers/status");
}

export async function saveProviderSettings(input: Record<string, unknown>): Promise<{
  settings: ProviderSettings;
  status: ProviderStatus;
}> {
  return requestJson("/api/v2/providers/settings", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(input)
  });
}

export async function testProvider(provider: string): Promise<ProviderStatus> {
  const payload = await requestJson<{ status: ProviderStatus }>("/api/v2/providers/test", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ provider })
  });
  return payload.status;
}

export async function getRuns(): Promise<RunSummaryItem[]> {
  if (shouldUseRustApiV3()) {
    const runs = await getRustRuns();
    return runs.map(rustRunSummaryToRunSummaryItem);
  }
  const payload = await requestJson<{ runs: RunSummaryItem[] }>("/api/v2/runs");
  return payload.runs;
}

export async function getRun(runId: string, includeEvents = true): Promise<StoredRunDetail> {
  if (shouldUseRustApiV3()) {
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
  return requestJson<StoredRunDetail>(`/api/v2/runs/${runId}?include_events=${includeEvents ? "true" : "false"}`);
}

export async function getRunEvents(runId: string, cursor = 0, limit = 200): Promise<RunEventsPage> {
  if (shouldUseRustApiV3()) {
    const payload = await getRustRunEvents(runId);
    return rustRunEventsToRunEventsPage(payload, cursor, limit);
  }
  return requestJson<RunEventsPage>(`/api/v2/runs/${runId}/events?cursor=${cursor}&limit=${limit}`);
}

export function getContextPacket(runId: string, packetId: string): Promise<ContextPacketDetail> {
  return requestJson<ContextPacketDetail>(`/api/v2/runs/${runId}/context-packets/${packetId}`);
}

export async function getArtifact(runId: string, artifactId: string): Promise<ArtifactDetail> {
  if (shouldUseRustApiV3()) {
    if (artifactId === "final-report.json" || artifactId === "final_report") {
      const report = await previewRustRunReport(runId);
      return rustRunReportToArtifactDetail(report);
    }
    const response = await getRustRunArtifact(runId, artifactId);
    return rustArtifactPayloadToArtifactDetail(response.artifact_name, response.payload);
  }
  return requestJson<ArtifactDetail>(`/api/v2/runs/${runId}/artifacts/${artifactId}`);
}

export function getToolResult(runId: string, toolResultId: string): Promise<ToolResultDetail> {
  return requestJson<ToolResultDetail>(`/api/v2/runs/${runId}/tool-results/${toolResultId}`);
}

export async function getBlob(runId: string, blobId: string): Promise<BlobDetail> {
  if (shouldUseRustApiV3()) {
    const digest = blobId.startsWith("blob://sha256/") ? blobId.slice("blob://sha256/".length) : blobId;
    const blob = await getRustBlobSha256(digest);
    return rustBlobToBlobDetail(blobId, blob);
  }
  return requestJson<BlobDetail>(`/api/v2/runs/${runId}/blobs/${encodeURIComponent(blobId)}`);
}

export function getLiveAgentRun(runId: string): Promise<LiveRunDetail> {
  return requestJson<LiveRunDetail>(`/api/v2/live-agent-runs/${runId}`);
}

export async function getDefaultAgentWorkflow(): Promise<{
  agent_workflow: AgentWorkflowSpec;
}> {
  if (shouldUseRustApiV3()) {
    const payload = await requestJson<RustDefaultWorkflowResponse>("/api/v3/workflows/default");
    return { agent_workflow: rustDefaultWorkflowToAgentWorkflow(payload) };
  }
  return requestJson("/api/v2/agent-workflows/default");
}

export async function validateAgentWorkflow(agentWorkflow: AgentWorkflowSpec): Promise<AgentWorkflowValidationResult> {
  if (shouldUseRustApiV3()) {
    const config = legacyCanvasToWorkflowSpec(agentWorkflow);
    const report = await validateRustWorkflowSpec(config, agentWorkflow.id);
    return rustValidationReportToAgentWorkflowValidationResult(report);
  }
  return requestJson<AgentWorkflowValidationResult>("/api/v2/agent-workflows/validate", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(agentWorkflow)
  });
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

export function validateRustMcpManifest(
  request: RustMcpManifestValidationRequest
): Promise<RustMcpManifestValidation> {
  return requestJson<RustMcpManifestValidation>("/api/v3/mcp/manifests/validate", {
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

export async function getAgentRuntimeProfiles(agentWorkflow: AgentWorkflowSpec): Promise<Record<string, unknown>[]> {
  const payload = await requestJson<{ profiles: Record<string, unknown>[] }>("/api/v2/agent-workflows/runtime-profiles", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(agentWorkflow)
  });
  return payload.profiles;
}

export async function getAgentWorkflow(workflowId: string): Promise<AgentWorkflowSpec> {
  if (shouldUseRustApiV3()) {
    const payload = await requestJson<RustLibraryWorkflowGetResponse>(
      `/api/v3/library/workflows/${encodeURIComponent(workflowId)}`
    );
    return rustLibraryWorkflowToAgentWorkflow(payload);
  }
  const payload = await requestJson<{ agent_workflow: AgentWorkflowSpec }>(`/api/v2/library/agent-workflows/${workflowId}`);
  return payload.agent_workflow;
}

export async function saveAgentWorkflow(agentWorkflow: AgentWorkflowSpec): Promise<AgentWorkflowSpec> {
  if (shouldUseRustApiV3()) {
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
  const payload = await requestJson<{ agent_workflow: AgentWorkflowSpec }>("/api/v2/library/agent-workflows", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(agentWorkflow)
  });
  return payload.agent_workflow;
}

export function createPlannerChatDraft(input: {
  repo: string;
  request: string;
  workflow_id: string;
  planner_agent_id: string;
  agent_workflow: AgentWorkflowSpec;
  scopes: string[];
  knowledge_pack_ids?: string[];
  skill_pack_ids?: string[];
  memory_pack_ids?: string[];
}): Promise<PlannerChatDraft> {
  return requestJson("/api/v2/planner-chat/draft", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(input)
  });
}

export function confirmPlannerChatDraft(input: {
  draft_id: string;
  approved: boolean;
  repo?: string;
  scopes?: string[];
  edits?: Record<string, unknown>;
  initial_data?: Record<string, unknown>;
}): Promise<PlannerChatConfirmResult> {
  return requestJson("/api/v2/planner-chat/confirm", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(input)
  });
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
  interaction_mode: PlannerInteractionMode;
}): Promise<PlannerChatSession> {
  return requestJson("/api/v2/planner-chat/sessions", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(input)
  });
}

export function sendPlannerChatTurn(input: {
  session_id: string;
  message: string;
  interaction_mode: PlannerInteractionMode;
  start_if_ready?: boolean;
}): Promise<PlannerChatTurnResponse> {
  return requestJson(`/api/v2/planner-chat/sessions/${encodeURIComponent(input.session_id)}/turn`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({
      message: input.message,
      interaction_mode: input.interaction_mode,
      start_if_ready: input.start_if_ready ?? true
    })
  });
}

export function getPlannerChatSession(sessionId: string): Promise<PlannerChatSession> {
  return requestJson(`/api/v2/planner-chat/sessions/${encodeURIComponent(sessionId)}`);
}

export async function startLiveAgentRun(input: {
  repo: string;
  request: string;
  agent_workflow: AgentWorkflowSpec;
  approved: boolean;
  scopes: string[];
  initial_data?: Record<string, unknown>;
}): Promise<{ run_id: string; status: string; events_url: string; result_url: string }> {
  return requestJson("/api/v2/live-agent-runs", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(input)
  });
}

export function deleteRun(runId: string): Promise<{ run_id: string; deleted: boolean; orphan_blobs_removed: number }> {
  return requestJson(`/api/v2/runs/${runId}`, {
    method: "DELETE"
  });
}

export async function rollbackPatch(input: {
  repo: string;
  snapshot_id: string;
  scopes: string[];
}): Promise<{ rollback: Record<string, unknown> }> {
  return requestJson("/api/v2/patches/rollback", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(input)
  });
}

export function subscribeRunEvents(url: string, onEvent: (event: RunEvent) => void, onError: (error: Event) => void) {
  const source = new EventSource(url);
  source.onmessage = (message) => {
    onEvent(JSON.parse(message.data) as RunEvent);
  };
  const eventTypes = [
    "run.started",
    "node.started",
    "node.completed",
    "node.skipped",
    "node.retry_requested",
    "loop.started",
    "loop.iteration.started",
    "loop.iteration.completed",
    "loop.completed",
    "loop.blocked",
    "agent.context_packet",
    "tool.called",
    "tool.result",
    "agent.called",
    "artifact.produced",
    "artifact.validation_failed",
    "approval.required",
    "approval.recorded",
    "edge.selected",
    "budget.warning",
    "run.completed",
    "run.blocked",
    "run.failed",
    "agent_graph.run.started",
    "agent_graph.round.started",
    "planner.order.produced",
    "planner.plan_cached",
    "skill.index.available",
    "skill.route.selected",
    "agent.context_packet_v2",
    "agent.coding_context_packet",
    "agent.context_compaction.applied",
    "token.ledger.entry",
    "agent_graph.agent_call.started",
    "agent_graph.agent_call.completed",
    "agent_graph.agent_call.schema_failed",
    "agent_graph.agent_call.repair_started",
    "agent_graph.agent_call.repair_completed",
    "agent_graph.agent_call.repair_failed",
    "agent_graph.wave.started",
    "agent_task.ready",
    "agent_task.started",
    "agent_task.completed",
    "agent_task.blocked",
    "join.waiting",
    "join.completed",
    "resource.deferred",
    "agent_graph.wave.completed",
    "planner.input_bundle.created",
    "round_summary.created",
    "planner.decision.produced",
    "final_report.created",
    "agent_graph.run.completed",
    "agent_graph.run.blocked",
    "agent_graph.run.failed"
  ];
  for (const type of eventTypes) {
    source.addEventListener(type, (message) => {
      onEvent(JSON.parse((message as MessageEvent).data) as RunEvent);
    });
  }
  source.onerror = onError;
  return source;
}
