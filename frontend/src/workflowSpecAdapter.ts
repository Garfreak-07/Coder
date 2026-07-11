import type {
  AgentModelTier,
  AgentWorkflowAgent,
  AgentWorkflowEdge,
  AgentWorkflowRole,
  AgentWorkflowSpec,
  AgentWorkflowValidationIssue,
  AgentWorkflowValidationResult,
  HarnessModeBinding,
  RustAgentSpec,
  RustHarnessSpec,
  RustMemoryAccess,
  RustModelSpec,
  RustPermissionPolicy,
  RustProjectConfig,
  RustValidationReport,
  RustWorkflowExport,
  RustWorkflowSpec
} from "./types";
import { cloneAgentWorkflow, normalizeAgentWorkflow } from "./workflowGraph";

type HarnessMode = "planning_chat" | "workflow_supervisor" | "task_execution";

const defaultModelSpecs: Record<AgentModelTier, RustModelSpec> = {
  best: {
    provider: "openai-compatible",
    model: "best",
    base_url_env: "LLM_BASE_URL",
    api_key_env: "LLM_API_KEY"
  },
  standard: {
    provider: "openai-compatible",
    model: "standard",
    base_url_env: "LLM_BASE_URL",
    api_key_env: "LLM_API_KEY"
  },
  economy: {
    provider: "openai-compatible",
    model: "economy",
    base_url_env: "LLM_BASE_URL",
    api_key_env: "LLM_API_KEY"
  }
};

const readonlyPermissions: RustPermissionPolicy = {
  read_files: "allow",
  write_files: "deny",
  run_commands: "deny",
  child_harness_permissions: "deny",
  network: "deny",
  secrets: "deny",
  publish_external: "deny",
  git_commit: "deny",
  git_push: "deny",
  deploy: "deny"
};

const taskPermissions: RustPermissionPolicy = {
  read_files: "allow",
  write_files: "ask",
  run_commands: "ask",
  child_harness_permissions: "ask",
  network: "ask",
  secrets: "ask",
  publish_external: "deny",
  git_commit: "deny",
  git_push: "deny",
  deploy: "deny"
};

const plannerCapabilities = [
  "negotiate_contract",
  "make_plan",
  "judge_completion",
  "judge_risk",
  "make_next_decision",
  "round_summarize"
];

const executorCapabilities = [
  "follow_planner_order",
  "modify_files",
  "optional_check_command",
  "return_execution_result"
];

export function canvasToWorkflowSpec(agentWorkflow: AgentWorkflowSpec): RustProjectConfig {
  const workflow = normalizeAgentWorkflow(agentWorkflow);
  const models: Record<string, RustModelSpec> = {};
  const agents: Record<string, RustAgentSpec> = {};
  const harnesses: Record<string, RustHarnessSpec> = {};

  for (const agent of workflow.agents) {
    const modelId = modelRefForTier(agent.model_tier);
    models[modelId] = defaultModelSpecs[agent.model_tier];
    agents[agent.id] = rustAgentSpecFor(agent, modelId);
  }

  const executionAgents = workflow.agents
    .filter((agent) => agent.id !== workflow.primary_planner_id)
    .sort((left, right) => executionAgentPriority(left) - executionAgentPriority(right));
  const nodes = executionAgents.map((agent) => {
    const mode: HarnessMode = agent.role === "planner" ? "workflow_supervisor" : "task_execution";
    const binding = harnessBindingForAgent(workflow, agent.id, mode);
    harnesses[binding.profile_id] = rustHarnessSpecFor(binding, mode);
    return {
      id: agent.id,
      agent: agent.id,
      harness: binding.profile_id
    };
  });

  const planningChatBinding = workflow.harness_bindings?.planning_chat;
  if (planningChatBinding) {
    harnesses[planningChatBinding.profile_id] = rustHarnessSpecFor(planningChatBinding, "planning_chat");
  }

  const executionNodeIds = new Set(nodes.map((node) => node.id));
  const finalReportAgent =
    executionAgents.find((agent) => agent.id.toLowerCase().includes("verifier"))?.id ??
    executionAgents.at(-1)?.id ??
    workflow.primary_planner_id;
  const rustWorkflow: RustWorkflowSpec = {
    name: workflow.name,
    max_rounds: workflow.loop_policy.max_auto_rounds,
    ...(workflow.loop_policy.token_budget != null
      ? { token_budget: workflow.loop_policy.token_budget }
      : {}),
    nodes,
    edges: workflow.edges
      .filter((edge) => executionNodeIds.has(edge.from) && executionNodeIds.has(edge.to))
      .map((edge) => ({
        from: edge.from,
        to: edge.to,
        on: transitionForCanvasEdge(workflow, edge)
      })),
    stop: {
      on_status: ["completed", "blocked", "failed"],
      final_report_agent: finalReportAgent
    }
  };

  return {
    version: 1,
    models,
    agents,
    harnesses,
    workflows: {
      [workflow.id]: rustWorkflow
    }
  };
}

export function canvasToWorkflowExport(agentWorkflow: AgentWorkflowSpec): RustWorkflowExport {
  const workflow = normalizeAgentWorkflow(agentWorkflow);
  const config = canvasToWorkflowSpec(workflow);
  return {
    ...config,
    kind: "coder.workflow",
    workflow_id: workflow.id,
    workflow: config.workflows[workflow.id],
    ui: workflow.ui,
    agent_workflow: cloneAgentWorkflow(workflow)
  };
}

export function workflowSpecToCanvas(input: RustProjectConfig | RustWorkflowExport, workflowId?: string): AgentWorkflowSpec {
  const config = workflowExportToProjectConfig(input);
  const exportEnvelope = isRustWorkflowExport(input) ? input : null;
  const selectedWorkflowId = workflowId ?? exportEnvelope?.workflow_id ?? Object.keys(config.workflows)[0] ?? "imported-workflow";
  const workflow = config.workflows[selectedWorkflowId] ?? exportEnvelope?.workflow;
  if (!workflow) {
    throw new Error(`Workflow '${selectedWorkflowId}' was not found in the imported spec.`);
  }

  const canvasSnapshot = exportEnvelope?.agent_workflow ?? exportEnvelope?.legacy_agent_workflow;
  const workflowAgentIds = new Set(workflow.nodes.map((node) => node.agent));
  const detachedPlannerId = Object.entries(config.agents).find(
    ([agentId, agent]) => agent.role === "planner" && !workflowAgentIds.has(agentId)
  )?.[0];
  const primaryPlannerId =
    canvasSnapshot?.primary_planner_id ??
    detachedPlannerId ??
    workflow.nodes.find((node) => config.agents[node.agent]?.role === "planner")?.id ??
    workflow.nodes[0]?.id ??
    "planner";
  const nodes = workflow.nodes.length > 0
    ? workflow.nodes
    : [{ id: "executor", agent: "executor", harness: "review-only" }];

  const executionAgents = nodes.map((node) => {
    const rustAgent = config.agents[node.agent] ?? config.agents[node.id];
    const previous = canvasSnapshot?.agents.find((agent) => agent.id === node.id || agent.id === node.agent);
    return canvasAgentForNode(node.id, rustAgent, previous, node.id === primaryPlannerId);
  });
  const primaryPlanner = nodes.some((node) => node.id === primaryPlannerId)
    ? []
    : [
        canvasAgentForNode(
          primaryPlannerId,
          config.agents[primaryPlannerId],
          canvasSnapshot?.agents.find((agent) => agent.id === primaryPlannerId),
          true
        )
      ];
  const agents = [...primaryPlanner, ...executionAgents];

  return normalizeAgentWorkflow({
    id: selectedWorkflowId,
    version: canvasSnapshot?.version ?? "0.5",
    name: workflow.name || canvasSnapshot?.name || selectedWorkflowId,
    description: canvasSnapshot?.description ?? "",
    primary_planner_id: primaryPlannerId,
    agents,
    edges: workflow.edges.map((edge) => ({
      from: edge.from,
      to: edge.to,
      on: edge.on,
      ...(edge.to === primaryPlannerId || edge.on === "continue" ? { loop: true } : {})
    })),
    harness_bindings: canvasSnapshot?.harness_bindings ?? harnessBindingsForWorkflow(nodes, primaryPlannerId, agents),
    loop_policy: {
      max_auto_rounds: workflow.max_rounds || canvasSnapshot?.loop_policy.max_auto_rounds || 3,
      token_budget: workflow.token_budget ?? canvasSnapshot?.loop_policy.token_budget ?? null,
      user_can_change: true
    },
    ui: exportEnvelope?.ui ?? canvasSnapshot?.ui ?? { layout: {} }
  });
}

export function workflowExportToProjectConfig(input: RustProjectConfig | RustWorkflowExport): RustProjectConfig {
  if (isRustWorkflowExport(input)) {
    return {
      version: 1,
      disable_all_hooks: input.disable_all_hooks,
      disableAllHooks: input.disableAllHooks,
      hooks: input.hooks,
      models: input.models,
      agents: input.agents,
      harnesses: input.harnesses,
      workflows: input.workflows
    };
  }
  return input;
}

export function parseWorkflowImport(value: unknown): AgentWorkflowSpec {
  if (isRustWorkflowExport(value)) {
    return workflowSpecToCanvas(value);
  }
  if (isRustProjectConfig(value)) {
    return workflowSpecToCanvas(value);
  }
  if (isAgentWorkflowSpec(value)) {
    return normalizeAgentWorkflow(value);
  }
  throw new Error("Expected a Coder workflow export or Agent workflow JSON.");
}

export function validateWorkflowSpec(config: RustProjectConfig, workflowId: string): AgentWorkflowValidationResult {
  const issues: AgentWorkflowValidationIssue[] = [];
  const workflow = config.workflows[workflowId];
  if (!workflow) {
    issues.push(issue("error", "workflow_not_found", `Workflow '${workflowId}' does not exist.`, "workflow", workflowId));
    return validationResult(issues, config, workflowId);
  }

  if (!workflow.name.trim()) {
    issues.push(issue("error", "workflow_name_empty", "Workflow must have a name.", "workflow", workflowId));
  }
  if (workflow.max_rounds < 1 || workflow.max_rounds > 20) {
    issues.push(issue("error", "workflow_max_rounds_out_of_range", "Max rounds must be between 1 and 20.", "workflow", workflowId));
  }
  if (workflow.token_budget != null && (!Number.isSafeInteger(workflow.token_budget) || workflow.token_budget <= 0)) {
    issues.push(issue("error", "workflow_token_budget_invalid", "Token budget must be a positive integer.", "workflow", workflowId));
  }
  if (workflow.nodes.length === 0) {
    issues.push(issue("error", "workflow_nodes_empty", "Workflow must contain at least one agent.", "workflow", workflowId));
  }

  const nodeIds = new Set<string>();
  for (const node of workflow.nodes) {
    if (nodeIds.has(node.id)) {
      issues.push(issue("error", "duplicate_workflow_node", `Agent node '${node.id}' is duplicated.`, "node", node.id));
    }
    nodeIds.add(node.id);
    if (!config.agents[node.agent]) {
      issues.push(issue("error", "workflow_node_agent_not_found", `Agent '${node.agent}' does not exist.`, "node", node.id));
    }
    if (!config.harnesses[node.harness]) {
      issues.push(issue("error", "workflow_node_harness_not_found", `Harness '${node.harness}' does not exist.`, "node", node.id));
    }
  }

  for (const [agentId, agent] of Object.entries(config.agents)) {
    if (!config.models[agent.model]) {
      issues.push(issue("error", "agent_model_not_found", `Agent '${agentId}' references missing model '${agent.model}'.`, "agent", agentId));
    }
  }

  for (const edge of workflow.edges) {
    if (!edge.on.trim()) {
      issues.push(issue("error", "workflow_edge_condition_empty", "Connection must define a transition.", "edge", `${edge.from}->${edge.to}`));
    }
    if (!nodeIds.has(edge.from)) {
      issues.push(issue("error", "workflow_edge_source_not_found", `Connection source '${edge.from}' does not exist.`, "edge", edge.from));
    }
    if (!nodeIds.has(edge.to)) {
      issues.push(issue("error", "workflow_edge_target_not_found", `Connection target '${edge.to}' does not exist.`, "edge", edge.to));
    }
  }

  return validationResult(issues, config, workflowId);
}

export const validateRustCanvasConfig = validateWorkflowSpec;

export function rustValidationReportToAgentWorkflowValidationResult(report: RustValidationReport): AgentWorkflowValidationResult {
  const issues = report.issues.map((rustIssue) => {
    const level = rustIssue.level.toLowerCase() === "error" ? "error" : "warning";
    return issue(level, rustIssue.code, rustIssue.message, targetTypeFromRustTarget(rustIssue.target), rustIssue.target);
  });
  return {
    status: report.status === "pass" || report.status === "warning" || report.status === "error" ? report.status : "warning",
    issues,
    summary: {
      source: "rust_workflow_spec"
    }
  };
}

export function isRustWorkflowExport(value: unknown): value is RustWorkflowExport {
  const record = asRecord(value);
  return Boolean(
    record &&
      record.kind === "coder.workflow" &&
      record.version === 1 &&
      isRecord(record.models) &&
      isRecord(record.agents) &&
      isRecord(record.harnesses) &&
      isRecord(record.workflows)
  );
}

export function isRustProjectConfig(value: unknown): value is RustProjectConfig {
  const record = asRecord(value);
  return Boolean(
    record &&
      record.version === 1 &&
      isRecord(record.models) &&
      isRecord(record.agents) &&
      isRecord(record.harnesses) &&
      isRecord(record.workflows)
  );
}

function rustAgentSpecFor(agent: AgentWorkflowAgent, model: string): RustAgentSpec {
  const planner = agent.role === "planner";
  return {
    role: agent.role,
    model,
    system: agent.purpose?.trim() || systemInstructionsFor(agent),
    memory: memoryAccessFor(agent.role),
    output_contract: planner ? "planner_conversation" : "execution_result"
  };
}

function rustHarnessSpecFor(binding: HarnessModeBinding, mode: HarnessMode): RustHarnessSpec {
  const backend = backendForBinding(binding, mode);
  const taskMode = mode === "task_execution" && backend === "native-rust";
  const browserMode = backend === "browser-verifier";
  const plannerChatMode = mode === "planning_chat";
  return {
    backend,
    tools: toolsForHarness(mode, backend),
    permissions: taskMode
      ? { ...taskPermissions }
      : browserMode
        ? { ...readonlyPermissions, run_commands: "allow" }
        : { ...readonlyPermissions },
    memory: taskMode || browserMode
      ? {
          read: ["workflow", "run"],
          write: ["run"]
        }
      : plannerChatMode
        ? {
            read: ["user", "project", "run", "repo_facts", "knowledge_hints"],
            write: ["run"]
          }
      : {
          read: ["workflow", "run"],
          write: ["workflow", "run"]
        },
    verification: {
      require_evidence: taskMode || browserMode,
      allowed_checks: taskMode ? ["cargo test", "npm run build"] : browserMode ? ["auto"] : []
    }
  };
}

function toolsForHarness(mode: HarnessMode, backend: string): string[] {
  if (mode === "planning_chat") {
    return ["memory_read", "knowledge_retrieve", "repo_search", "read_file", "git_diff"];
  }
  if (backend === "browser-verifier") {
    return ["browser_static", "gameplay_static", "browser_dynamic", "gameplay_browser"];
  }
  if (mode === "task_execution") {
    return [
      "repo_find_files",
      "repo_search_text",
      "repo_read_file",
      "repo_read_file_range",
      "git_status",
      "git_diff",
      "agent_subagent",
      "read_subagent_status",
      "cancel_subagent_background",
      "command_preview",
      "command_run",
      "command_background",
      "read_command_output",
      "cancel_command_background",
      "patch_preview",
      "patch_apply",
      "Skill"
    ];
  }
  return ["repo_search", "read_file", "git_diff"];
}

function backendForBinding(binding: HarnessModeBinding, mode: HarnessMode): string {
  if (mode === "planning_chat") return "planner-model";
  const profileId = binding.profile_id.toLowerCase();
  if (mode === "workflow_supervisor" || profileId.includes("workflow-planner")) {
    return "planner-model";
  }
  if (profileId.includes("browser")) return "browser-verifier";
  return "native-rust";
}

function harnessBindingForAgent(workflow: AgentWorkflowSpec, agentId: string, mode: HarnessMode): HarnessModeBinding {
  const override = workflow.harness_bindings?.agent_overrides?.[agentId]?.[mode];
  if (override?.profile_id) return override;
  const binding = workflow.harness_bindings?.[mode];
  if (binding?.profile_id) return binding;
  return {
    profile_id: mode === "planning_chat" ? "planner-conversation" : mode === "task_execution" ? "native-code-edit" : "workflow-planner"
  };
}

function harnessBindingsForWorkflow(
  nodes: Array<{ id: string; agent: string; harness: string }>,
  primaryPlannerId: string,
  agents: AgentWorkflowAgent[]
): NonNullable<AgentWorkflowSpec["harness_bindings"]> {
  const roleById = new Map(agents.map((agent) => [agent.id, agent.role]));
  const primaryHarness = nodes.find((node) => roleById.get(node.id) === "planner")?.harness ?? "workflow-planner";
  const taskHarness = nodes.find((node) => roleById.get(node.id) !== "planner")?.harness ?? "native-code-edit";
  return {
    planning_chat: { profile_id: "planner-conversation" },
    workflow_supervisor: { profile_id: primaryHarness },
    task_execution: { profile_id: taskHarness },
    agent_overrides: Object.fromEntries(
      nodes.map((node) => [
        node.id,
        {
          [roleById.get(node.id) === "planner" && node.id !== primaryPlannerId
            ? "workflow_supervisor"
            : "task_execution"]: {
            profile_id: node.harness
          }
        }
      ])
    )
  };
}

function transitionForCanvasEdge(workflow: AgentWorkflowSpec, edge: AgentWorkflowEdge): string {
  if (edge?.on?.trim()) return edge.on.trim();
  if (edge?.handoff === "execution_result" || edge?.loop || edge.to === workflow.primary_planner_id) return "completed";
  if (edge?.handoff === "planner_decision" || edge?.handoff === "round_summary") return "completed";
  return "ready";
}

function canvasAgentForNode(
  id: string,
  rustAgent: RustAgentSpec | undefined,
  previous: AgentWorkflowAgent | undefined,
  primary: boolean
): AgentWorkflowAgent {
  const role = canvasRole(rustAgent?.role, primary);
  return {
    id,
    name: previous?.name ?? displayNameForAgent(id, role),
    role,
    role_card: previous?.role_card ?? (role === "executor" ? "executor" : undefined),
    purpose: previous?.purpose ?? rustAgent?.system ?? "",
    model_tier: previous?.model_tier ?? tierFromModelRef(rustAgent?.model, role),
    can_talk_to_human: previous?.can_talk_to_human ?? primary,
    capabilities: previous?.capabilities ?? (role === "planner" ? plannerCapabilities : executorCapabilities),
    runtime_profile_id: previous?.runtime_profile_id,
    skill_pack_ids: [...(previous?.skill_pack_ids ?? [])],
    knowledge_pack_ids: [...(previous?.knowledge_pack_ids ?? [])],
    memory_pack_ids: [...(previous?.memory_pack_ids ?? [])]
  };
}

function canvasRole(role: string | undefined, primary: boolean): AgentWorkflowRole {
  if (role === "planner" || role === "executor") return role;
  return primary ? "planner" : "executor";
}

function displayNameForAgent(id: string, role: AgentWorkflowRole): string {
  if (role === "planner") return "Planner";
  if (role === "executor") return "Executor";
  return id;
}

function executionAgentPriority(agent: AgentWorkflowAgent): number {
  if (agent.id.toLowerCase().includes("verifier")) return 1;
  if (agent.role === "planner") return 2;
  return 0;
}

function modelRefForTier(tier: AgentModelTier): string {
  return `default-${tier}`;
}

function tierFromModelRef(model: string | undefined, role: AgentWorkflowRole): AgentModelTier {
  if (model?.includes("best")) return "best";
  if (model?.includes("economy")) return "economy";
  return role === "planner" ? "best" : "standard";
}

function memoryAccessFor(role: AgentWorkflowRole): RustMemoryAccess {
  if (role === "planner") {
    return {
      read: ["user", "project", "workflow", "run", "repo_facts", "knowledge_hints"],
      write: ["workflow", "run"]
    };
  }
  return {
    read: ["workflow", "run"],
    write: ["run"]
  };
}

function systemInstructionsFor(agent: AgentWorkflowAgent): string {
  if (agent.role === "planner") {
    return "Plan the work, decide when execution is ready, and assemble the final evidence-based report.";
  }
  return "Execute planner-approved coding tasks, make scoped changes, run checks when allowed, and return evidence.";
}

function issue(
  level: "error" | "warning",
  code: string,
  message: string,
  targetType: string,
  targetId?: string | null
): AgentWorkflowValidationIssue {
  return {
    level,
    code,
    message,
    target_type: targetType,
    target_id: targetId
  };
}

function validationResult(issues: AgentWorkflowValidationIssue[], config: RustProjectConfig, workflowId: string): AgentWorkflowValidationResult {
  const status = issues.some((candidate) => candidate.level === "error")
    ? "error"
    : issues.some((candidate) => candidate.level === "warning")
      ? "warning"
      : "pass";
  return {
    status,
    issues,
    summary: {
      source: "rust_workflow_spec",
      workflow_id: workflowId,
      agents: Object.keys(config.agents).length,
      harnesses: Object.keys(config.harnesses).length,
      workflows: Object.keys(config.workflows).length
    }
  };
}

function targetTypeFromRustTarget(target: string): string {
  if (target.startsWith("agents.")) return "agent";
  if (target.startsWith("harnesses.")) return "harness";
  if (target.startsWith("workflows.")) return "workflow";
  return "workflow";
}

function isAgentWorkflowSpec(value: unknown): value is AgentWorkflowSpec {
  const record = asRecord(value);
  return Boolean(
    record &&
      typeof record.id === "string" &&
      typeof record.name === "string" &&
      Array.isArray(record.agents) &&
      Array.isArray(record.edges) &&
      isRecord(record.loop_policy)
  );
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return isRecord(value) ? value : null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
