import type { AgentSpec, LibraryIndex, RunEvent, WorkflowSpec } from "./types";

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
}): Promise<{ run_id: string; status: string; events_url: string; result_url: string }> {
  return requestJson("/api/v2/live-runs", {
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
    "tool.called",
    "agent.called",
    "approval.required",
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
