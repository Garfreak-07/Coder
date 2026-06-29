import type { AgentWorkflowSpec } from "./types";

export const defaultPlannerLedAgentWorkflow: AgentWorkflowSpec = {
  id: "default-planner-led",
  version: "0.5",
  name: "Planner-led Agent Workflow",
  description: "Planner decides. Executor executes, verifies, and returns execution evidence. Runtime hides graph details.",
  primary_planner_id: "planner",
  harness_bindings: {
    planning_chat: { profile_id: "planner-conversation" },
    workflow_supervisor: { profile_id: "planner-conversation" },
    task_execution: { profile_id: "openhands-task-executor-default" },
    agent_overrides: {}
  },
  agents: [
    {
      id: "planner",
      name: "Planner",
      role: "planner",
      model_tier: "best",
      can_talk_to_human: true,
      capabilities: ["negotiate_contract", "make_plan", "judge_completion", "judge_risk", "make_next_decision", "round_summarize"]
    },
    {
      id: "executor",
      name: "Executor",
      role: "executor",
      role_card: "executor",
      model_tier: "standard",
      can_talk_to_human: false,
      capabilities: ["follow_planner_order", "modify_files", "optional_check_command", "return_execution_result"]
    }
  ],
  edges: [
    { from: "planner", to: "executor" },
    { from: "executor", to: "planner", loop: true }
  ],
  loop_policy: { max_auto_rounds: 3, user_can_change: true },
  ui: {
    layout: {
      planner: { x: 60, y: 120 },
      executor: { x: 360, y: 120 }
    }
  }
};
