import type { AgentWorkflowSpec } from "./types";

export const defaultPlannerLedAgentWorkflow: AgentWorkflowSpec = {
  id: "default-planner-led",
  version: "0.4",
  name: "Planner-led Agent Workflow",
  description: "Planner decides. Code Worker proposes changes by order. Tester returns evidence. Runtime hides graph details.",
  primary_planner_id: "planner",
  agents: [
    {
      id: "planner",
      name: "Planner Agent",
      role: "planner",
      model_tier: "best",
      can_talk_to_human: true,
      capabilities: ["negotiate_contract", "make_plan", "judge_completion", "judge_risk", "make_next_decision", "round_summarize"]
    },
    {
      id: "executor",
      name: "Code Worker Agent",
      role: "executor",
      model_tier: "standard",
      can_talk_to_human: false,
      capabilities: ["modify_files", "follow_planner_order", "return_execution_result"]
    },
    {
      id: "tester",
      name: "Tester Agent",
      role: "tester",
      model_tier: "standard",
      can_talk_to_human: false,
      capabilities: ["model_review", "optional_check_command", "return_test_result"]
    }
  ],
  edges: [
    { from: "planner", to: "executor" },
    { from: "executor", to: "tester" },
    { from: "tester", to: "planner", loop: true }
  ],
  loop_policy: { max_auto_rounds: 3, user_can_change: true },
  ui: {
    layout: {
      planner: { x: 60, y: 120 },
      executor: { x: 360, y: 120 },
      tester: { x: 660, y: 120 }
    }
  }
};

