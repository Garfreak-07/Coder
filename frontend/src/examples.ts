import type { AgentSpec, AgentWorkflowAgent, AgentWorkflowSpec, ContextPolicy, WorkflowSpec } from "./types";

const plannerInstructions =
  "Return strict JSON for the requested artifact. Do not include full transcripts. Use only structured runtime inputs.";

const baseContext: Omit<ContextPolicy, "input_keys" | "summary_keys"> = {
  max_items_per_key: 12,
  max_chars_per_value: 3500,
  include_all_state: false,
  include_event_history: false,
  include_full_outputs: false
};

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

// Legacy runtime preview fixture only. Product AgentWorkflow runs use AgentGraphRuntime.
export function compileAgentWorkflow(spec: AgentWorkflowSpec): WorkflowSpec {
  const planner = requiredAgent(spec, "planner");
  const executor = requiredAgent(spec, "executor");
  const tester = requiredAgent(spec, "tester");
  const maxRounds = spec.loop_policy.max_auto_rounds;

  return {
    id: `${spec.id}-runtime`,
    version: spec.version,
    name: spec.name,
    description: spec.description,
    max_steps: Math.max(12, 6 * Math.max(1, maxRounds)),
    max_agent_calls: Math.max(6, 6 * Math.max(1, maxRounds)),
    max_tool_calls: 0,
    token_budget: 80000,
    agents: [
      runtimeAgent(planner, {
        runtimeId: "planner_contract",
        role: "Planner Agent",
        goal: "Negotiate the RunContract with the human-facing request.",
        artifactType: "run_contract",
        outputKey: "run_contract"
      }),
      runtimeAgent(planner, {
        runtimeId: "planner_order",
        role: "Planner Agent",
        goal: "Produce the next executable order for the Code Worker.",
        artifactType: "planner_order",
        outputKey: "planner_order",
        inputKeys: ["run_contract", "round_summary", "execution_result", "test_result"],
        summaryKeys: ["round_summary"]
      }),
      runtimeAgent(executor, {
        runtimeId: "executor",
        role: "Code Worker Agent",
        goal: "Follow the PlannerOrder and return only execution facts.",
        artifactType: "execution_result",
        outputKey: "execution_result",
        inputKeys: ["run_contract", "planner_order"],
        summaryKeys: ["run_contract", "planner_order"],
        canEdit: true
      }),
      runtimeAgent(tester, {
        runtimeId: "tester",
        role: "Tester Agent",
        goal: "Review execution evidence and return only a TestResult.",
        artifactType: "test_result",
        outputKey: "test_result",
        inputKeys: ["planner_order", "execution_result"],
        summaryKeys: ["planner_order", "execution_result"]
      }),
      runtimeAgent(planner, {
        runtimeId: "planner_decision",
        role: "Planner Agent",
        goal: "Decide whether to finish, continue, ask the human, or stop.",
        artifactType: "planner_decision",
        outputKey: "planner_decision",
        inputKeys: ["run_contract", "execution_result", "test_result", "round_summary"],
        summaryKeys: ["execution_result", "test_result", "round_summary"]
      }),
      runtimeAgent(planner, {
        runtimeId: "round_summarizer",
        role: "Planner Agent",
        goal: "Compress this round into a compact carry-forward summary.",
        artifactType: "round_summary",
        outputKey: "round_summary",
        inputKeys: ["planner_order", "execution_result", "test_result", "planner_decision"],
        summaryKeys: ["planner_order", "execution_result", "test_result", "planner_decision"]
      })
    ],
    nodes: [
      { id: "start", type: "start" },
      { id: "run_contract", type: "agent", agent_id: "planner_contract", output_key: "run_contract" },
      { id: "planner_order", type: "agent", agent_id: "planner_order", output_key: "planner_order" },
      { id: "execute", type: "agent", agent_id: "executor", output_key: "execution_result" },
      { id: "test", type: "agent", agent_id: "tester", output_key: "test_result" },
      { id: "planner_decision", type: "agent", agent_id: "planner_decision", output_key: "planner_decision" },
      { id: "round_summary", type: "agent", agent_id: "round_summarizer", output_key: "round_summary" },
      {
        id: "planner_loop",
        type: "loop",
        loop_mode: "retry_until",
        condition:
          "planner_decision.next_action == 'finish' or planner_decision.next_action == 'stop' or planner_decision.next_action == 'ask_human'",
        max_iterations: maxRounds,
        output_key: "planner_loop"
      },
      { id: "finish", type: "end" }
    ],
    edges: [
      { from: "start", to: "run_contract" },
      { from: "run_contract", to: "planner_order" },
      { from: "planner_order", to: "execute" },
      { from: "execute", to: "test" },
      { from: "test", to: "planner_decision" },
      { from: "planner_decision", to: "round_summary" },
      { from: "round_summary", to: "planner_loop" },
      { from: "planner_loop", to: "planner_order", when: "planner_loop.should_continue == True", max_traversals: maxRounds },
      { from: "planner_loop", to: "finish", when: "planner_loop.should_continue == False" }
    ],
    stop_conditions: [
      "planner_decision.next_action == finish",
      "planner_decision.next_action == ask_human",
      "planner_decision.next_action == stop",
      "max_auto_rounds reached",
      "token budget exceeded"
    ]
  };
}

function requiredAgent(spec: AgentWorkflowSpec, role: AgentWorkflowAgent["role"]): AgentWorkflowAgent {
  const agent = spec.agents.find((candidate) => candidate.role === role);
  if (!agent) {
    throw new Error(`Agent workflow is missing required role: ${role}`);
  }
  return agent;
}

function runtimeAgent(
  source: AgentWorkflowAgent,
  options: {
    runtimeId: string;
    role: string;
    goal: string;
    artifactType: NonNullable<AgentSpec["artifact_type"]>;
    outputKey: string;
    inputKeys?: string[];
    summaryKeys?: string[];
    canEdit?: boolean;
  }
): AgentSpec {
  const canEdit = options.canEdit ?? false;
  return {
    id: options.runtimeId,
    name: source.name,
    role: options.role,
    goal: options.goal,
    instructions: plannerInstructions,
    provider: null,
    model: source.model_tier === "standard" ? null : source.model_tier,
    tools: [],
    output_key: options.outputKey,
    artifact_type: options.artifactType,
    permissions: {
      read_files: true,
      edit_files: canEdit,
      run_commands: false,
      use_network: false,
      requires_approval: canEdit
    },
    context: {
      input_keys: options.inputKeys ?? [],
      summary_keys: options.summaryKeys ?? [],
      ...baseContext
    }
  };
}

export const codingWorkbenchWorkflow: WorkflowSpec = compileAgentWorkflow(defaultPlannerLedAgentWorkflow);
