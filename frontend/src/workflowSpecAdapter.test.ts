import { defaultPlannerLedAgentWorkflow } from "./examples";
import { readFileSync } from "node:fs";
import { PlannerChatPage } from "./features/planner-chat/PlannerChatPage";
import type { AgentWorkflowSpec, RustProjectConfig } from "./types";
import {
  agentWorkflowToRustLibrarySaveRequest,
  rustDefaultWorkflowToAgentWorkflow,
  rustLibraryWorkflowToAgentWorkflow,
  rustRunEventsToRunEventsPage
} from "./rustApiAdapter";
import {
  legacyCanvasToWorkflowExport,
  legacyCanvasToWorkflowSpec,
  parseWorkflowImport,
  validateWorkflowSpec,
  workflowSpecToLegacyCanvas
} from "./workflowSpecAdapter";

const assert = {
  equal(actual: unknown, expected: unknown) {
    if (actual !== expected) {
      throw new Error(`Expected ${JSON.stringify(actual)} to equal ${JSON.stringify(expected)}`);
    }
  },
  deepEqual(actual: unknown, expected: unknown) {
    const actualJson = JSON.stringify(actual);
    const expectedJson = JSON.stringify(expected);
    if (actualJson !== expectedJson) {
      throw new Error(`Expected ${actualJson} to deep-equal ${expectedJson}`);
    }
  },
  ok(value: unknown) {
    if (!value) {
      throw new Error("Expected value to be truthy");
    }
  }
};

function test(name: string, run: () => void) {
  try {
    run();
    console.log(`ok - ${name}`);
  } catch (error) {
    console.error(`not ok - ${name}`);
    throw error;
  }
}

test("exports legacy planner/executor canvas to Rust workflow config", () => {
  const config = legacyCanvasToWorkflowSpec(defaultPlannerLedAgentWorkflow);
  const workflow = config.workflows["default-planner-led"];

  assert.equal(config.version, 1);
  assert.equal(workflow.nodes.length, 2);
  assert.equal(workflow.nodes[0].id, "planner");
  assert.equal(workflow.nodes[1].id, "executor");
  assert.equal(workflow.max_rounds, defaultPlannerLedAgentWorkflow.loop_policy.max_auto_rounds);
  assert.equal(workflow.stop.final_report_agent, defaultPlannerLedAgentWorkflow.primary_planner_id);
});

test("roundtrips Rust workflow export back to equivalent legacy canvas", () => {
  const exported = legacyCanvasToWorkflowExport(defaultPlannerLedAgentWorkflow);
  const imported = workflowSpecToLegacyCanvas(exported);

  assert.equal(imported.id, defaultPlannerLedAgentWorkflow.id);
  assert.equal(imported.primary_planner_id, "planner");
  assert.deepEqual(imported.ui?.layout, defaultPlannerLedAgentWorkflow.ui?.layout);
  assert.equal(imported.loop_policy.max_auto_rounds, defaultPlannerLedAgentWorkflow.loop_policy.max_auto_rounds);
  assert.deepEqual(imported.edges, [
    { from: "planner", to: "executor" },
    { from: "executor", to: "planner", loop: true }
  ]);
});

test("maps OpenHands harness profiles to OpenHands backend", () => {
  const config = legacyCanvasToWorkflowSpec(defaultPlannerLedAgentWorkflow);
  const plannerHarness = config.harnesses["planner-conversation"];
  const taskHarness = config.harnesses["openhands-task-executor-default"];

  assert.equal(plannerHarness.backend, "planner-model");
  assert.equal(config.workflows["default-planner-led"].nodes[0].harness, "planner-conversation");
  assert.equal(taskHarness.backend, "openhands");
  assert.equal(taskHarness.openhands?.server_url, "http://127.0.0.1:8000");
  assert.deepEqual(plannerHarness.memory.read, ["user", "project", "run", "repo_facts", "knowledge_hints"]);
  assert.deepEqual(config.agents.executor.memory.read, ["workflow", "run"]);
  assert.deepEqual(taskHarness.memory.read, ["workflow", "run"]);
});

test("maps native read-only harness profiles to native Rust backend", () => {
  const workflow: AgentWorkflowSpec = {
    ...defaultPlannerLedAgentWorkflow,
    harness_bindings: {
      planning_chat: { profile_id: "review-only-chat", provider_id: "native-rust" },
      workflow_supervisor: { profile_id: "review-only-supervisor", provider_id: "native-rust" },
      task_execution: { profile_id: "review-only-task", provider_id: "native-rust" },
      agent_overrides: {}
    }
  };
  const config = legacyCanvasToWorkflowSpec(workflow);

  assert.equal(config.harnesses["review-only-chat"].backend, "planner-model");
  assert.equal(config.harnesses["review-only-task"].backend, "native-rust");
  assert.equal(config.harnesses["review-only-task"].openhands, null);
  assert.deepEqual(config.harnesses["review-only-task"].memory.read, ["workflow", "run"]);
  assert.equal(config.workflows["default-planner-led"].nodes[0].harness, "review-only-chat");
});

test("validates invalid Rust specs with user-facing errors", () => {
  const config = legacyCanvasToWorkflowSpec(defaultPlannerLedAgentWorkflow);
  delete config.harnesses["openhands-task-executor-default"];

  const validation = validateWorkflowSpec(config, "default-planner-led");

  assert.equal(validation.status, "error");
  assert.ok(validation.issues.some((issue) => issue.code === "workflow_node_harness_not_found"));
  assert.ok(validation.issues.every((issue) => !issue.message.includes("HarnessRuntimeManager")));
});

test("imports future Rust fields without crashing", () => {
  const exported = legacyCanvasToWorkflowExport(defaultPlannerLedAgentWorkflow) as ReturnType<
    typeof legacyCanvasToWorkflowExport
  > & { future_field: { keep: true } };
  exported.future_field = { keep: true };

  const imported = parseWorkflowImport(exported);

  assert.equal(imported.id, defaultPlannerLedAgentWorkflow.id);
});

test("imports plain Rust ProjectConfig while preserving max rounds and planner", () => {
  const config: RustProjectConfig = legacyCanvasToWorkflowSpec(defaultPlannerLedAgentWorkflow);
  const imported = workflowSpecToLegacyCanvas(config, "default-planner-led");

  assert.equal(imported.primary_planner_id, "planner");
  assert.equal(imported.loop_policy.max_auto_rounds, defaultPlannerLedAgentWorkflow.loop_policy.max_auto_rounds);
});

test("maps Rust default workflow response into the legacy canvas model", () => {
  const config = legacyCanvasToWorkflowSpec(defaultPlannerLedAgentWorkflow);
  const imported = rustDefaultWorkflowToAgentWorkflow({
    workflow_id: "default-planner-led",
    config,
    workflow: config.workflows["default-planner-led"]
  });

  assert.equal(imported.id, "default-planner-led");
  assert.equal(imported.name, defaultPlannerLedAgentWorkflow.name);
  assert.equal(imported.agents.length, 2);
  assert.equal(imported.edges[1].loop, true);
});

test("roundtrips library save payloads through Rust workflow storage shape", () => {
  const request = agentWorkflowToRustLibrarySaveRequest(defaultPlannerLedAgentWorkflow);
  const imported = rustLibraryWorkflowToAgentWorkflow({
    workflow_id: request.workflow_id,
    workflow: request.workflow
  });

  assert.equal(request.workflow_id, defaultPlannerLedAgentWorkflow.id);
  assert.equal(imported.id, defaultPlannerLedAgentWorkflow.id);
  assert.deepEqual(imported.ui?.layout, defaultPlannerLedAgentWorkflow.ui?.layout);
});

test("maps Rust run events into the existing paged event model", () => {
  const page = rustRunEventsToRunEventsPage(
    {
      run_id: "run-1",
      events: [
        {
          event_id: "evt-1",
          run_id: "run-1",
          sequence: 1,
          timestamp: "2026-06-28T00:00:00Z",
          kind: "node.started",
          payload: { node_id: "planner", status: "running" },
          refs: []
        },
        {
          event_id: "evt-2",
          run_id: "run-1",
          sequence: 2,
          timestamp: "2026-06-28T00:00:01Z",
          kind: "node.completed",
          payload: { node_id: "planner", status: "completed" },
          refs: []
        }
      ]
    },
    1,
    1
  );

  assert.equal(page.events.length, 1);
  assert.equal(page.events[0].type, "node.completed");
  assert.equal(page.events[0].node_id, "planner");
  assert.equal(page.next_cursor, 2);
  assert.equal(page.has_more, false);
});

test("frontend API client stays on Rust v3 without Python switch", () => {
  const apiSource = readFileSync("src/api.ts", "utf8");
  const removedV2Route = "/api/" + "v2";
  const removedPythonServer = "fast" + "api";

  assert.ok(!apiSource.includes(removedV2Route));
  assert.ok(!apiSource.toLowerCase().includes(removedPythonServer));
  assert.ok(apiSource.includes("/api/v3/planner-chat/sessions"));
  assert.ok(apiSource.includes("legacyCanvasToWorkflowSpec(input.agent_workflow)"));
});

test("run summary recognizes backend approval request events", () => {
  const appSource = readFileSync("src/App.tsx", "utf8");

  assert.ok(appSource.includes("approval.requested"));
  assert.ok(appSource.includes("isApprovalRequestEvent"));
});

test("Planner Chat page uses Start Work timeline and hides legacy draft controls", () => {
  const source = readFileSync("src/features/planner-chat/PlannerChatPage.tsx", "utf8");

  assert.ok(source.includes("Start Work"));
  assert.ok(source.includes("WorkTimeline"));
  assert.ok(source.includes("ReviewChangesCard"));
  assert.ok(!source.includes("message-status"));
  assert.ok(!source.includes("formatRunStatus"));
  assert.ok(!source.includes("runStatus"));
  assert.ok(!source.includes("Draft plan"));
  assert.ok(!source.includes("Draft Plan"));
  assert.ok(!source.includes("Discuss"));
  assert.ok(!source.includes("plannerInteractionMode"));
});

test("Planner Chat page renders two turns without synthetic status cards", () => {
  const tree = renderPlannerChat({
    session_id: "session-1",
    workflow_id: defaultPlannerLedAgentWorkflow.id,
    planner_agent_id: "planner",
    agent_workflow: defaultPlannerLedAgentWorkflow,
    repo: ".",
    scopes: [],
    knowledge_pack_ids: [],
    skill_pack_ids: [],
    memory_pack_ids: [],
    interaction_mode: "discuss",
    messages: [
      { role: "user", content: "First question" },
      { role: "assistant", content: "First answer" },
      { role: "user", content: "Second question" },
      { role: "assistant", content: "Second answer" }
    ],
    task_state: {
      goal: null,
      user_intent: null,
      scope: [],
      constraints: [],
      success_criteria: [],
      known_context: [],
      missing_context: [],
      open_questions: [],
      assumptions: [],
      risks: [],
      memory_proposals: [],
      plan_steps: [],
      readiness: "not_ready"
    },
    generation: 4,
    last_turn: null,
    run_id: null,
    status: "chatting"
  });
  const text = collectReactTreeText(tree);
  const classNames = collectReactTreeClassNames(tree);

  assert.ok(text.includes("First question"));
  assert.ok(text.includes("First answer"));
  assert.ok(text.includes("Second question"));
  assert.ok(text.includes("Second answer"));
  assert.ok(!classNames.includes("message-status"));
  assert.ok(!text.includes("Ready"));
  assert.ok(!text.includes("Draft Plan"));
  assert.ok(!text.includes("Discuss"));
});

test("Provider Settings exposes DeepSeek preset and exact test result UI", () => {
  const panelSource = readFileSync("src/components/ProviderSettingsPanel.tsx", "utf8");
  const hookSource = readFileSync("src/hooks/useProviderSettings.ts", "utf8");

  assert.ok(panelSource.includes("DeepSeek preset"));
  assert.ok(panelSource.includes("Test Provider"));
  assert.ok(panelSource.includes("Test succeeded"));
  assert.ok(panelSource.includes("Test failed"));
  assert.ok(panelSource.includes("openai-compatible"));
  assert.ok(panelSource.includes("custom"));
  assert.ok(hookSource.includes("deepseek-v4-flash"));
  assert.ok(hookSource.includes("https://api.deepseek.com"));
  assert.ok(hookSource.includes("mock_mode: false"));
});

test("Planner Chat shows provider setup before chat when credentials are missing", () => {
  const tree = renderPlannerChat(null, {
    providerSetupRequired: true,
    providerSetupMessage: "Configure an API key for openai-compatible."
  });
  const text = collectReactTreeText(tree);

  assert.ok(text.includes("Provider setup required"));
  assert.ok(text.includes("Configure an API key for openai-compatible."));
  assert.ok(text.includes("Open Settings"));
});

function renderPlannerChat(
  plannerSession: Parameters<typeof PlannerChatPage>[0]["plannerSession"],
  overrides: Partial<Parameters<typeof PlannerChatPage>[0]> = {}
): unknown {
  return PlannerChatPage({
    activeRunId: null,
    changeSets: [],
    debugEvidence: null,
    diffByChangeSetId: {},
    loadingChangeSetId: null,
    repo: ".",
    request: "",
    runLoading: false,
    scopesText: "",
    submittedRequest: "",
    timelineItems: [],
    plannerSession,
    plannerStrength: "balanced",
    providerSetupRequired: false,
    providerSetupMessage: "",
    onAcceptChangeSet: () => undefined,
    onLoadChangeSetDiff: () => undefined,
    onOpenProviderSettings: () => undefined,
    onRepoChange: () => undefined,
    onRequestChange: () => undefined,
    onScopesTextChange: () => undefined,
    onPlannerStrengthChange: () => undefined,
    onStartWork: () => undefined,
    onSubmitRequest: () => undefined,
    onUndoChangeSet: () => undefined,
    ...overrides
  });
}

function collectReactTreeText(node: unknown): string {
  if (node === null || typeof node === "undefined" || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(collectReactTreeText).join("");
  if (typeof node !== "object") return "";
  const props = (node as { props?: { children?: unknown } }).props;
  return collectReactTreeText(props?.children);
}

function collectReactTreeClassNames(node: unknown): string {
  if (node === null || typeof node === "undefined" || typeof node === "boolean") return "";
  if (Array.isArray(node)) return node.map(collectReactTreeClassNames).join(" ");
  if (typeof node !== "object") return "";
  const props = (node as { props?: { children?: unknown; className?: unknown } }).props;
  return [
    typeof props?.className === "string" ? props.className : "",
    collectReactTreeClassNames(props?.children)
  ].filter(Boolean).join(" ");
}
