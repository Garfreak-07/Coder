import type { WorkflowSpec } from "./types";
import { codingWorkbenchWorkflow } from "./examples";

export const workflowTemplate: WorkflowSpec = {
  id: "new-workflow",
  version: "0.2",
  name: "New Workflow",
  description: "Edit this JSON or use the canvas to shape the workflow.",
  max_steps: 20,
  max_agent_calls: 6,
  max_tool_calls: 6,
  token_budget: 40000,
  agents: [],
  nodes: [
    { id: "start", type: "start" },
    { id: "finish", type: "end" }
  ],
  edges: [{ from: "start", to: "finish" }],
  stop_conditions: ["max_steps reached"]
};

export interface WorkflowTemplateCard {
  id: "default-coding" | "blank";
  workflow: WorkflowSpec;
  agentCount: number;
  tools: string[];
  approvals: string;
  modelRequirement: string;
  knowledgeRequirement: string;
  risk: string;
}

export const workflowTemplateCards: WorkflowTemplateCard[] = [
  {
    id: "default-coding",
    workflow: codingWorkbenchWorkflow,
    agentCount: codingWorkbenchWorkflow.agents.length,
    tools: ["project_index", "recommend_modules", "propose_patch", "apply_patch", "rollback_patch", "run_check"],
    approvals: "requiredApprovals",
    modelRequirement: "optionalModel",
    knowledgeRequirement: "projectKnowledge",
    risk: "mediumRisk"
  },
  {
    id: "blank",
    workflow: workflowTemplate,
    agentCount: 0,
    tools: [],
    approvals: "none",
    modelRequirement: "optionalModel",
    knowledgeRequirement: "projectKnowledge",
    risk: "lowRisk"
  }
];

export function instantiateWorkflowTemplate(template: WorkflowTemplateCard): WorkflowSpec {
  return {
    ...template.workflow,
    id: `${template.workflow.id}-${Date.now()}`,
    agents: template.workflow.agents.map((agent) => ({
      ...agent,
      permissions: { ...agent.permissions },
      context: {
        ...agent.context,
        input_keys: [...agent.context.input_keys],
        summary_keys: [...agent.context.summary_keys]
      },
      tools: [...agent.tools]
    })),
    nodes: template.workflow.nodes.map((node) => ({
      ...node,
      input: node.input ? { ...node.input } : undefined
    })),
    edges: template.workflow.edges.map((edge) => ({ ...edge })),
    stop_conditions: [...template.workflow.stop_conditions]
  };
}
