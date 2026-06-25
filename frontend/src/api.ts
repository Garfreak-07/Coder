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
  ProviderSettings,
  ProviderStatus,
  PluginManifest,
  RoleCardSpec,
  RunEvent,
  RunEventsPage,
  RunSummaryItem,
  SkillUpdateInfo,
  StoredRunDetail,
  ToolResultDetail
} from "./types";

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

export function getLibrary(): Promise<LibraryIndex> {
  return requestJson<LibraryIndex>("/api/v2/library");
}

export function getHealth(): Promise<HealthStatus> {
  return requestJson<HealthStatus>("/api/v2/health");
}

export async function getCapabilities(): Promise<CapabilitySpec[]> {
  const payload = await requestJson<{ capabilities: CapabilitySpec[] }>("/api/v2/capabilities");
  return payload.capabilities;
}

export async function getAgentRoleCards(): Promise<RoleCardSpec[]> {
  const payload = await requestJson<{ role_cards: RoleCardSpec[] }>("/api/v2/agent-role-cards");
  return payload.role_cards;
}

export function getInstalledSkills(): Promise<InstalledSkillsPayload> {
  return requestJson<InstalledSkillsPayload>("/api/v2/skills/installed");
}

export async function getExtensionPlugins(): Promise<PluginManifest[]> {
  const payload = await requestJson<{ plugins: PluginManifest[] }>("/api/v2/extensions/plugins");
  return payload.plugins;
}

export async function searchExtensions(query: string): Promise<ExtensionManifest[]> {
  const payload = await requestJson<{ extensions: ExtensionManifest[] }>(`/api/v2/extensions/search?q=${encodeURIComponent(query)}`);
  return payload.extensions;
}

export function discoverSkills(registryUrl: string): Promise<DiscoverSkillsPayload> {
  return requestJson<DiscoverSkillsPayload>(`/api/v2/skills/discover?registry_url=${encodeURIComponent(registryUrl)}`);
}

export function getSkillUpdates(registryUrl: string): Promise<{ updates: SkillUpdateInfo[] }> {
  return requestJson<{ updates: SkillUpdateInfo[] }>(`/api/v2/skills/updates?registry_url=${encodeURIComponent(registryUrl)}`);
}

export function installSkill(skillId: string, registryUrl: string): Promise<Record<string, unknown>> {
  return requestJson("/api/v2/skills/install", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ skill_id: skillId, registry_url: registryUrl })
  });
}

export function updateSkill(skillId: string, registryUrl: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/update`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ registry_url: registryUrl })
  });
}

export function autoUpdateSkills(registryUrl: string): Promise<Record<string, unknown>> {
  return requestJson("/api/v2/skills/auto-update", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ registry_url: registryUrl })
  });
}

export function enableSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/enable`, {
    method: "POST",
    headers: jsonHeaders
  });
}

export function disableSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/disable`, {
    method: "POST",
    headers: jsonHeaders
  });
}

export function removeSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}`, {
    method: "DELETE"
  });
}

export function pinSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/pin`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({})
  });
}

export function unpinSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/unpin`, {
    method: "POST",
    headers: jsonHeaders
  });
}

export function rollbackSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/rollback`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({})
  });
}

export function setSkillUpdatePolicy(skillId: string, updatePolicy: "manual" | "auto_official_low_risk"): Promise<Record<string, unknown>> {
  return requestJson(`/api/v2/skills/${encodeURIComponent(skillId)}/update-policy`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ update_policy: updatePolicy })
  });
}

export function importDeveloperSkill(path: string): Promise<Record<string, unknown>> {
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
  const payload = await requestJson<{ runs: RunSummaryItem[] }>("/api/v2/runs");
  return payload.runs;
}

export function getRun(runId: string, includeEvents = true): Promise<StoredRunDetail> {
  return requestJson<StoredRunDetail>(`/api/v2/runs/${runId}?include_events=${includeEvents ? "true" : "false"}`);
}

export function getRunEvents(runId: string, cursor = 0, limit = 200): Promise<RunEventsPage> {
  return requestJson<RunEventsPage>(`/api/v2/runs/${runId}/events?cursor=${cursor}&limit=${limit}`);
}

export function getContextPacket(runId: string, packetId: string): Promise<ContextPacketDetail> {
  return requestJson<ContextPacketDetail>(`/api/v2/runs/${runId}/context-packets/${packetId}`);
}

export function getArtifact(runId: string, artifactId: string): Promise<ArtifactDetail> {
  return requestJson<ArtifactDetail>(`/api/v2/runs/${runId}/artifacts/${artifactId}`);
}

export function getToolResult(runId: string, toolResultId: string): Promise<ToolResultDetail> {
  return requestJson<ToolResultDetail>(`/api/v2/runs/${runId}/tool-results/${toolResultId}`);
}

export function getBlob(runId: string, blobId: string): Promise<BlobDetail> {
  return requestJson<BlobDetail>(`/api/v2/runs/${runId}/blobs/${encodeURIComponent(blobId)}`);
}

export function getLiveAgentRun(runId: string): Promise<LiveRunDetail> {
  return requestJson<LiveRunDetail>(`/api/v2/live-agent-runs/${runId}`);
}

export async function getDefaultAgentWorkflow(): Promise<{
  agent_workflow: AgentWorkflowSpec;
}> {
  return requestJson("/api/v2/agent-workflows/default");
}

export function validateAgentWorkflow(agentWorkflow: AgentWorkflowSpec): Promise<AgentWorkflowValidationResult> {
  return requestJson<AgentWorkflowValidationResult>("/api/v2/agent-workflows/validate", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(agentWorkflow)
  });
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
  const payload = await requestJson<{ agent_workflow: AgentWorkflowSpec }>(`/api/v2/library/agent-workflows/${workflowId}`);
  return payload.agent_workflow;
}

export async function saveAgentWorkflow(agentWorkflow: AgentWorkflowSpec): Promise<AgentWorkflowSpec> {
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
