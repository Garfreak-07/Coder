import type { Edge as FlowEdge, Node as FlowNode } from "@xyflow/react";

import { nodeTypeLabels } from "./i18n";
import type { AgentSpec, AgentWorkflowEdge, AgentWorkflowSpec, EdgeSpec, NodeSpec, NodeType, WorkflowSpec } from "./types";

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

export function toFlowNodes(workflow: WorkflowSpec): FlowNode[] {
  return workflow.nodes.map((node, index) => ({
    id: node.id,
    type: "default",
    position: { x: (index % 3) * 260, y: Math.floor(index / 3) * 150 },
    data: {
      label: nodeDisplayLabel(node, workflow)
    },
    className: `workflow-node node-${node.type}`
  }));
}

export function nodeDisplayLabel(node: NodeSpec, workflow: WorkflowSpec): string {
  const typeLabel = nodeTypeLabels[node.type] ?? node.type;
  if (node.type === "agent") {
    const agent = workflow.agents.find((candidate) => candidate.id === node.agent_id);
    return `${typeLabel}: ${agent?.name ?? node.agent_id ?? "Unassigned"}\n${node.id}`;
  }
  if (node.type === "tool" || node.type === "mcp_tool") {
    return `${typeLabel}: ${node.tool ?? "Unconfigured"}\n${node.id}`;
  }
  if (node.type === "loop") {
    return `${typeLabel}: ${node.loop_mode ?? "retry_until"} x${node.max_iterations ?? 3}\n${node.id}`;
  }
  return `${typeLabel}\n${node.id}`;
}

export function toFlowEdges(workflow: WorkflowSpec): FlowEdge[] {
  return workflow.edges.map((edge, index) => ({
    id: edgeIdFromIndex(index),
    source: edge.from,
    target: edge.to,
    label: edge.when ?? undefined,
    animated: Boolean(edge.when)
  }));
}

export function fromFlowEdges(flowEdges: FlowEdge[], workflow: WorkflowSpec) {
  return flowEdges
    .filter((edge) => edge.source && edge.target)
    .map((edge) => {
      const existing = workflow.edges.find((candidate) => candidate.from === edge.source && candidate.to === edge.target);
      return {
        from: edge.source,
        to: edge.target,
        when: existing?.when ?? null,
        priority: existing?.priority ?? 0,
        max_traversals: existing?.max_traversals ?? null
      };
    });
}

export function uniqueNodeId(workflow: WorkflowSpec, type: NodeType): string {
  const used = new Set(workflow.nodes.map((node) => node.id));
  let index = 1;
  let candidate: string = type;
  while (used.has(candidate)) {
    candidate = `${type}_${index}`;
    index += 1;
  }
  return candidate;
}

export function uniqueAgentId(workflow: WorkflowSpec): string {
  const used = new Set(workflow.agents.map((agent) => agent.id));
  let index = 1;
  let candidate = "agent";
  while (used.has(candidate)) {
    candidate = `agent_${index}`;
    index += 1;
  }
  return candidate;
}

export function createDefaultAgent(id: string): AgentSpec {
  return {
    id,
    name: "New Agent",
    role: "Agent",
    goal: "Describe this agent's purpose.",
    instructions: "",
    provider: null,
    model: null,
    tools: [],
    output_key: id,
    permissions: {
      read_files: true,
      edit_files: false,
      run_commands: false,
      use_network: false,
      requires_approval: true
    },
    context: {
      input_keys: [],
      summary_keys: [],
      max_items_per_key: 20,
      max_chars_per_value: 4000,
      include_all_state: false,
      include_event_history: false,
      include_full_outputs: false
    }
  };
}

export function cleanNode(node: NodeSpec): NodeSpec {
  return {
    id: node.id,
    type: node.type,
    ...(node.type === "agent" ? { agent_id: node.agent_id || "agent_id" } : {}),
    ...(node.type === "tool" ? { tool: node.tool || "project_index" } : {}),
    ...(node.type === "mcp_tool" ? { tool: node.tool || "tool_name" } : {}),
    ...(node.type === "condition" ? { condition: node.condition || "state.value == True" } : {}),
    ...(node.type === "loop"
      ? {
          loop_mode: node.loop_mode || "retry_until",
          ...(node.condition ? { condition: node.condition } : {}),
          ...(node.items_key ? { items_key: node.items_key } : {}),
          ...(node.item_key ? { item_key: node.item_key } : {}),
          ...(node.iteration_key ? { iteration_key: node.iteration_key } : {}),
          max_iterations: node.max_iterations || 3,
          ...(node.collect_key ? { collect_key: node.collect_key } : {}),
          ...(node.summary_key ? { summary_key: node.summary_key } : {})
        }
      : {}),
    ...(node.type === "human_gate" && node.approval_reason ? { approval_reason: node.approval_reason } : {}),
    ...(node.output_key ? { output_key: node.output_key } : {}),
    ...(node.input && Object.keys(node.input).length > 0 ? { input: node.input } : {})
  };
}

export function cleanEdge(edge: EdgeSpec): EdgeSpec {
  return {
    from: edge.from,
    to: edge.to,
    ...(edge.when ? { when: edge.when } : {}),
    ...(edge.priority ? { priority: edge.priority } : {}),
    ...(edge.max_traversals ? { max_traversals: edge.max_traversals } : {})
  };
}

export function cleanAgent(agent: AgentSpec): AgentSpec {
  return {
    ...agent,
    name: agent.name || null,
    provider: agent.provider || null,
    model: agent.model || null,
    output_key: agent.output_key || null
  };
}

export function upsertAgent(agents: AgentSpec[], next: AgentSpec): AgentSpec[] {
  return agents.some((agent) => agent.id === next.id)
    ? agents.map((agent) => (agent.id === next.id ? next : agent))
    : [...agents, next];
}

export function csvToList(value: string): string[] {
  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

export function linesToList(value: string): string[] {
  return value
    .split(/\r?\n/)
    .map((item) => item.trim())
    .filter(Boolean);
}

export function edgeIdFromIndex(index: number): string {
  return `edge-${index}`;
}

export function edgeIndexFromId(id: string): number | null {
  const match = /^edge-(\d+)$/.exec(id);
  return match ? Number(match[1]) : null;
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
