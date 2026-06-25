import type { Edge as FlowEdge, Node as FlowNode } from "@xyflow/react";

import type { AgentWorkflowAgent, AgentWorkflowEdge, AgentWorkflowSpec, HarnessBindings } from "./types";

const agentPositions: Record<string, { x: number; y: number }> = {
  planner: { x: 60, y: 105 },
  executor: { x: 365, y: 105 }
};

const roleLabels: Record<string, string> = {
  planner: "Planner",
  executor: "Executor"
};

const defaultHarnessBindings: HarnessBindings = {
  planning_chat: { profile_id: "openhands-planning-chat-default" },
  workflow_supervisor: { profile_id: "openhands-workflow-supervisor-default" },
  task_execution: { profile_id: "openhands-task-executor-default" },
  agent_overrides: {}
};

export function cloneAgentWorkflow(workflow: AgentWorkflowSpec): AgentWorkflowSpec {
  return {
    ...workflow,
    agents: workflow.agents.map((agent) => ({
      ...agent,
      capabilities: [...agent.capabilities],
      skill_pack_ids: [...(agent.skill_pack_ids ?? [])],
      knowledge_pack_ids: [...(agent.knowledge_pack_ids ?? [])],
      memory_pack_ids: [...(agent.memory_pack_ids ?? [])]
    })),
    edges: workflow.edges.map((edge) => ({ ...edge })),
    harness_bindings: cloneHarnessBindings(workflow.harness_bindings),
    loop_policy: { ...workflow.loop_policy },
    ui: {
      layout: Object.fromEntries(
        Object.entries(workflow.ui?.layout ?? {}).map(([agentId, position]) => [agentId, { ...position }])
      )
    }
  };
}

export function normalizeAgentWorkflow(workflow: AgentWorkflowSpec): AgentWorkflowSpec {
  const primaryPlannerId = workflow.primary_planner_id;
  return {
    ...workflow,
    id: workflow.id?.trim() || slugFromName(workflow.name) || `agent-workflow-${Date.now()}`,
    version: ["", "0.3", "0.4"].includes(workflow.version || "") ? "0.5" : workflow.version,
    description: workflow.description ?? "",
    agents: workflow.agents.map((agent) => ({
      ...agent,
      capabilities: [...agent.capabilities],
      skill_pack_ids: [...(agent.skill_pack_ids ?? [])],
      knowledge_pack_ids: [...(agent.knowledge_pack_ids ?? [])],
      memory_pack_ids: [...(agent.memory_pack_ids ?? [])]
    })),
    edges: workflow.edges.map((edge) => cleanAgentWorkflowEdge(edge, primaryPlannerId)),
    harness_bindings: cloneHarnessBindings(workflow.harness_bindings),
    loop_policy: {
      ...workflow.loop_policy,
      user_can_change: true
    },
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
      label: agentRoleLabel(agent)
    },
    className: `workflow-node agent-workflow-node agent-role-${agent.role}`
  }));
}

export function agentRoleLabel(agent: AgentWorkflowAgent): string {
  return roleLabels[agent.role] ?? agent.name;
}

export function toAgentFlowEdges(workflow: AgentWorkflowSpec): FlowEdge[] {
  return workflow.edges.map((edge, index) => ({
    id: agentEdgeIdFromIndex(index),
    source: edge.from,
    target: edge.to,
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

export function cleanAgentWorkflowEdge(edge: AgentWorkflowEdge, primaryPlannerId?: string): AgentWorkflowEdge {
  const loopsToPlanner = Boolean(primaryPlannerId && edge.to === primaryPlannerId);
  return {
    from: edge.from,
    to: edge.to,
    ...(loopsToPlanner ? { loop: true } : {})
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

function slugFromName(name: string): string {
  return name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

function cloneHarnessBindings(bindings: AgentWorkflowSpec["harness_bindings"]): AgentWorkflowSpec["harness_bindings"] {
  const source = bindings ?? defaultHarnessBindings;
  return {
    planning_chat: { ...source.planning_chat },
    workflow_supervisor: { ...source.workflow_supervisor },
    task_execution: { ...source.task_execution },
    agent_overrides: Object.fromEntries(
      Object.entries(source.agent_overrides ?? {}).map(([agentId, overrides]) => [
        agentId,
        Object.fromEntries(
          Object.entries(overrides).map(([mode, binding]) => [mode, { ...binding }])
        )
      ])
    )
  };
}
