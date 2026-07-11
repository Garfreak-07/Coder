import type { AgentWorkflowSpec } from "./types";

export const defaultPlannerLedAgentWorkflow: AgentWorkflowSpec = {
  id: "default-planner-led",
  version: "0.5",
  name: "Verified Execution Workflow",
  description: "The chat Planner approves one compact plan. Executor runs it, verifier supplies evidence, and the workflow Planner makes a bounded finish-or-improve decision.",
  primary_planner_id: "planner",
  harness_bindings: {
    planning_chat: { profile_id: "planner-conversation" },
    workflow_supervisor: { profile_id: "workflow-planner" },
    task_execution: { profile_id: "native-code-edit" },
    agent_overrides: {
      verifier: { task_execution: { profile_id: "browser-verification" } },
      "workflow-planner": { workflow_supervisor: { profile_id: "workflow-planner" } }
    }
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
    },
    {
      id: "verifier",
      name: "Verifier",
      role: "executor",
      model_tier: "standard",
      can_talk_to_human: false,
      capabilities: ["read_outputs", "run_safe_checks", "return_verification_result"]
    },
    {
      id: "workflow-planner",
      name: "Workflow Planner",
      role: "planner",
      model_tier: "best",
      can_talk_to_human: false,
      capabilities: ["read_execution_evidence", "read_verification_result", "decide_continue_or_finish", "route_repair"]
    }
  ],
  edges: [
    { from: "executor", to: "verifier", on: "completed" },
    { from: "executor", to: "verifier", on: "blocked" },
    { from: "verifier", to: "workflow-planner", on: "completed" },
    { from: "verifier", to: "workflow-planner", on: "failed" },
    { from: "verifier", to: "workflow-planner", on: "blocked" },
    { from: "workflow-planner", to: "executor", on: "ready" },
    { from: "workflow-planner", to: "executor", on: "continue", loop: true }
  ],
  loop_policy: { max_auto_rounds: 3, user_can_change: true },
  ui: {
    layout: {
      planner: { x: 60, y: 120 },
      executor: { x: 320, y: 120 },
      verifier: { x: 580, y: 120 },
      "workflow-planner": { x: 840, y: 120 }
    }
  }
};
