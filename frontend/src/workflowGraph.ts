import type { Edge as FlowEdge, Node as FlowNode } from "@xyflow/react";

import type { AgentWorkflowEdge, AgentWorkflowSpec } from "./types";

const agentPositions: Record<string, { x: number; y: number }> = {
  planner: { x: 60, y: 105 },
  executor: { x: 365, y: 105 },
  worker: { x: 365, y: 105 },
  tester: { x: 670, y: 105 },
  reviewer: { x: 670, y: 255 }
};

const roleLabels: Record<string, string> = {
  planner: "Planner",
  executor: "Executor",
  worker: "Worker",
  tester: "Tester",
  reviewer: "Reviewer",
  writer: "Writer",
  researcher: "Researcher",
  summarizer: "Summarizer",
  custom: "Custom"
};

export function cloneAgentWorkflow(workflow: AgentWorkflowSpec): AgentWorkflowSpec {
  return {
    ...workflow,
    agents: workflow.agents.map((agent) => ({
      ...agent,
      capabilities: [...agent.capabilities]
    })),
    edges: workflow.edges.map((edge) => ({ ...edge })),
    loop_policy: { ...workflow.loop_policy },
    ui: {
      layout: Object.fromEntries(
        Object.entries(workflow.ui?.layout ?? {}).map(([agentId, position]) => [agentId, { ...position }])
      )
    }
  };
}

export function toAgentFlowNodes(workflow: AgentWorkflowSpec): FlowNode[] {
  return workflow.agents.map((agent, index) => ({
    id: agent.id,
    type: "default",
    position: workflow.ui?.layout?.[agent.id] ?? agentPositions[agent.role] ?? { x: 80 + index * 280, y: 120 },
    data: {
      label: `${agent.name}\n${roleLabels[agent.role] ?? agent.role}${agent.can_talk_to_human ? " - can ask user" : ""}`
    },
    className: `workflow-node agent-workflow-node agent-role-${agent.role}`
  }));
}

export function toAgentFlowEdges(workflow: AgentWorkflowSpec): FlowEdge[] {
  return workflow.edges.map((edge, index) => ({
    id: agentEdgeIdFromIndex(index),
    source: edge.from,
    target: edge.to,
    label: edge.label ?? (edge.loop ? "loop" : undefined),
    animated: Boolean(edge.loop),
    className: edge.loop ? "agent-loop-edge" : "agent-handoff-edge"
  }));
}

export function agentEdgeIdFromIndex(index: number): string {
  return `agent-edge-${index}`;
}

export function agentEdgeIndexFromId(id: string): number | null {
  const match = /^agent-edge-(\d+)$/.exec(id);
  return match ? Number(match[1]) : null;
}

export function cleanAgentWorkflowEdge(edge: AgentWorkflowEdge): AgentWorkflowEdge {
  return {
    from: edge.from,
    to: edge.to,
    ...(edge.handoff ? { handoff: edge.handoff } : {}),
    ...(edge.loop ? { loop: true } : {}),
    ...(edge.label ? { label: edge.label } : {})
  };
}

export function linesToList(value: string): string[] {
  return value
    .split(/\r?\n/)
    .map((item) => item.trim())
    .filter(Boolean);
}

export function downloadJson(filename: string, value: unknown) {
  const blob = new Blob([JSON.stringify(value, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.click();
  URL.revokeObjectURL(url);
}

export function formatJson(value: unknown) {
  return JSON.stringify(value, null, 2);
}
