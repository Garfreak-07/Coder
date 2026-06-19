import type {
  AgentSpec,
  ContextPacketDetail,
  HealthStatus,
  LibraryIndex,
  LiveRunDetail,
  RunEvent,
  RunEventsPage,
  RunSummaryItem,
  StoredRunDetail,
  WorkflowSpec
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

export async function getRuns(): Promise<RunSummaryItem[]> {
  const payload = await requestJson<{ runs: RunSummaryItem[] }>("/api/v2/runs");
  return payload.runs;
}

export async function getLiveRuns(): Promise<RunSummaryItem[]> {
  const payload = await requestJson<{ runs: RunSummaryItem[] }>("/api/v2/live-runs");
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

export function getLiveRun(runId: string): Promise<LiveRunDetail> {
  return requestJson<LiveRunDetail>(`/api/v2/live-runs/${runId}`);
}

export async function getWorkflow(workflowId: string): Promise<WorkflowSpec> {
  const payload = await requestJson<{ workflow: WorkflowSpec }>(`/api/v2/library/workflows/${workflowId}`);
  return payload.workflow;
}

export async function getAgent(agentId: string): Promise<AgentSpec> {
  const payload = await requestJson<{ agent: AgentSpec }>(`/api/v2/library/agents/${agentId}`);
  return payload.agent;
}

export async function saveAgent(agent: AgentSpec): Promise<AgentSpec> {
  const payload = await requestJson<{ agent: AgentSpec }>("/api/v2/library/agents", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(agent)
  });
  return payload.agent;
}

export async function saveWorkflow(workflow: WorkflowSpec): Promise<WorkflowSpec> {
  const payload = await requestJson<{ workflow: WorkflowSpec }>("/api/v2/library/workflows", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(workflow)
  });
  return payload.workflow;
}

export async function startLiveRun(input: {
  repo: string;
  request: string;
  workflow: WorkflowSpec;
  approved: boolean;
  scopes: string[];
}): Promise<{ run_id: string; status: string; events_url: string; result_url: string }> {
  return requestJson("/api/v2/live-runs", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(input)
  });
}

export async function approveLiveRun(
  runId: string,
  input: { approved?: boolean; reason?: string } = {}
): Promise<{ run_id: string; status: string; events_url: string; result_url: string }> {
  return requestJson(`/api/v2/live-runs/${runId}/approve`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ approved: input.approved ?? true, reason: input.reason ?? null })
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
    "loop.started",
    "loop.iteration.started",
    "loop.iteration.completed",
    "loop.completed",
    "loop.blocked",
    "agent.context_packet",
    "tool.called",
    "agent.called",
    "approval.required",
    "approval.recorded",
    "edge.selected",
    "budget.warning",
    "run.completed",
    "run.blocked",
    "run.failed"
  ];
  for (const type of eventTypes) {
    source.addEventListener(type, (message) => {
      onEvent(JSON.parse((message as MessageEvent).data) as RunEvent);
    });
  }
  source.onerror = onError;
  return source;
}
