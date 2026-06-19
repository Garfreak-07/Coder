import type { WorkflowSpec } from "./types";

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
