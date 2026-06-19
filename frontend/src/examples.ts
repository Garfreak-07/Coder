import type { WorkflowSpec } from "./types";

export const codingWorkbenchWorkflow: WorkflowSpec = {
  id: "coding-workbench-v2",
  version: "0.2",
  name: "Token-Conscious Coding Workbench",
  description:
    "A JSON-driven workflow where canvas edges determine execution. Agents receive compact state summaries by default.",
  max_steps: 20,
  max_agent_calls: 6,
  max_tool_calls: 6,
  token_budget: 40000,
  agents: [
    {
      id: "planner",
      role: "Planning Agent",
      goal: "Select the smallest useful scope and produce implementation instructions.",
      instructions:
        "Use project_index and module recommendations. Return concise JSON with summary, target_files, risks, and next_action.",
      tools: [],
      output_key: "plan",
      permissions: {
        read_files: true,
        edit_files: false,
        run_commands: false,
        use_network: false,
        requires_approval: false
      },
      context: {
        input_keys: ["project_index", "module_recommendations"],
        summary_keys: ["project_index", "module_recommendations"],
        max_items_per_key: 12,
        max_chars_per_value: 3500,
        include_event_history: false,
        include_full_outputs: false
      }
    },
    {
      id: "executor",
      role: "Implementation Agent",
      goal: "Prepare a scoped patch plan without mutating files in this first v2 slice.",
      instructions: "Follow the approved plan. Keep output compact. Do not request broad edits.",
      tools: ["dry_run_patch"],
      output_key: "execution",
      permissions: {
        read_files: true,
        edit_files: true,
        run_commands: false,
        use_network: false,
        requires_approval: true
      },
      context: {
        input_keys: ["plan", "approval"],
        summary_keys: ["project_index"],
        max_items_per_key: 8,
        max_chars_per_value: 2500,
        include_event_history: false,
        include_full_outputs: false
      }
    },
    {
      id: "reviewer",
      role: "Review Agent",
      goal: "Judge whether the workflow should finish, retry, or block.",
      instructions: "Use only compact execution and check summaries. Return JSON with status and reason.",
      tools: [],
      output_key: "review",
      permissions: {
        read_files: true,
        edit_files: false,
        run_commands: false,
        use_network: false,
        requires_approval: false
      },
      context: {
        input_keys: ["execution", "check_result"],
        summary_keys: ["plan"],
        max_items_per_key: 8,
        max_chars_per_value: 2500,
        include_event_history: false,
        include_full_outputs: false
      }
    }
  ],
  nodes: [
    { id: "start", type: "start" },
    { id: "index_project", type: "tool", tool: "project_index", input: { max_files: 800 }, output_key: "project_index" },
    {
      id: "recommend_scope",
      type: "tool",
      tool: "recommend_modules",
      input: { query: "$request" },
      output_key: "module_recommendations"
    },
    { id: "plan", type: "agent", agent_id: "planner", output_key: "plan" },
    {
      id: "approval",
      type: "human_gate",
      approval_reason: "Approve before an implementation-capable agent runs.",
      output_key: "approval"
    },
    { id: "execute", type: "agent", agent_id: "executor", output_key: "execution" },
    { id: "dry_patch", type: "tool", tool: "dry_run_patch", input: { changes: "$execution" }, output_key: "patch_preview" },
    { id: "check", type: "tool", tool: "run_check", input: { command: "", approved: false }, output_key: "check_result" },
    { id: "review", type: "agent", agent_id: "reviewer", output_key: "review" },
    { id: "finish", type: "end" }
  ],
  edges: [
    { from: "start", to: "index_project" },
    { from: "index_project", to: "recommend_scope" },
    { from: "recommend_scope", to: "plan" },
    { from: "plan", to: "approval" },
    { from: "approval", to: "execute", when: "approval.approved == True" },
    { from: "execute", to: "dry_patch" },
    { from: "dry_patch", to: "check" },
    { from: "check", to: "review" },
    { from: "review", to: "finish" }
  ],
  stop_conditions: [
    "approval required",
    "max_steps reached",
    "max_agent_calls reached",
    "max_tool_calls reached",
    "token budget warning"
  ]
};
