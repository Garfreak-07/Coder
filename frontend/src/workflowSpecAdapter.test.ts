import { defaultPlannerLedAgentWorkflow } from "./examples";
import { readFileSync } from "node:fs";
import { getLiveAgentRun, normalizeRunChangeSetsResponse, normalizeRunTimelineResponse } from "./api";
import { AppSidebar } from "./components/AppSidebar";
import { PlannerChatPage } from "./features/planner-chat/PlannerChatPage";
import { ReviewChangesCard } from "./features/review-changes/ReviewChangesCard";
import { WorkTimeline } from "./features/work-timeline/WorkTimeline";
import type { AgentWorkflowSpec, PlannerChatSession, RustProjectConfig, TimelineItem } from "./types";
import {
  agentWorkflowToRustLibrarySaveRequest,
  rustDefaultWorkflowToAgentWorkflow,
  rustLibraryWorkflowToAgentWorkflow,
  rustRunEventsToRunEventsPage
} from "./rustApiAdapter";
import {
  canvasToWorkflowExport,
  canvasToWorkflowSpec,
  parseWorkflowImport,
  validateWorkflowSpec,
  workflowExportToProjectConfig,
  workflowSpecToCanvas
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

const tests: Array<{ name: string; run: () => void | Promise<void> }> = [];

function test(name: string, run: () => void | Promise<void>) {
  tests.push({ name, run });
}

async function runTests() {
  for (const { name, run } of tests) {
    try {
      await run();
      console.log(`ok - ${name}`);
    } catch (error) {
      console.error(`not ok - ${name}`);
      throw error;
    }
  }
}

test("exports planner/executor canvas to Rust workflow config", () => {
  const config = canvasToWorkflowSpec(defaultPlannerLedAgentWorkflow);
  const workflow = config.workflows["default-planner-led"];

  assert.equal(config.version, 1);
  assert.equal(workflow.nodes.length, 3);
  assert.equal(workflow.nodes[0].id, "executor");
  assert.equal(workflow.nodes[1].id, "verifier");
  assert.equal(workflow.nodes[2].id, "workflow-planner");
  assert.equal(workflow.max_rounds, defaultPlannerLedAgentWorkflow.loop_policy.max_auto_rounds);
  assert.equal(workflow.stop.final_report_agent, "verifier");
});

test("roundtrips Rust workflow export back to an equivalent canvas", () => {
  const exported = canvasToWorkflowExport(defaultPlannerLedAgentWorkflow);
  const imported = workflowSpecToCanvas(exported);

  assert.equal(imported.id, defaultPlannerLedAgentWorkflow.id);
  assert.equal(imported.primary_planner_id, "planner");
  assert.deepEqual(imported.ui?.layout, defaultPlannerLedAgentWorkflow.ui?.layout);
  assert.equal(imported.loop_policy.max_auto_rounds, defaultPlannerLedAgentWorkflow.loop_policy.max_auto_rounds);
  assert.deepEqual(imported.edges, defaultPlannerLedAgentWorkflow.edges);
});

test("roundtrips an optional shared workflow token budget", () => {
  const workflow: AgentWorkflowSpec = {
    ...defaultPlannerLedAgentWorkflow,
    loop_policy: {
      ...defaultPlannerLedAgentWorkflow.loop_policy,
      token_budget: 100_000
    }
  };
  const exported = canvasToWorkflowExport(workflow);
  const rustWorkflow = exported.workflows[workflow.id];
  const imported = workflowSpecToCanvas(exported);

  assert.equal(rustWorkflow.token_budget, 100_000);
  assert.equal(imported.loop_policy.token_budget, 100_000);
});

test("maps default task execution harness to native backend", () => {
  const config = canvasToWorkflowSpec(defaultPlannerLedAgentWorkflow);
  const plannerHarness = config.harnesses["planner-conversation"];
  const taskHarness = config.harnesses["native-code-edit"];

  assert.equal(plannerHarness.backend, "planner-model");
  assert.equal(config.workflows["default-planner-led"].nodes[0].harness, "native-code-edit");
  assert.equal(taskHarness.backend, "native-rust");
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
  const config = canvasToWorkflowSpec(workflow);

  assert.equal(config.harnesses["review-only-chat"].backend, "planner-model");
  assert.equal(config.harnesses["review-only-supervisor"].backend, "planner-model");
  assert.equal(config.harnesses["review-only-task"].backend, "native-rust");
  assert.deepEqual(config.harnesses["review-only-task"].memory.read, ["workflow", "run"]);
  assert.equal(config.workflows["default-planner-led"].nodes[0].harness, "review-only-task");
});

test("validates invalid Rust specs with user-facing errors", () => {
  const config = canvasToWorkflowSpec(defaultPlannerLedAgentWorkflow);
  delete config.harnesses["native-code-edit"];

  const validation = validateWorkflowSpec(config, "default-planner-led");

  assert.equal(validation.status, "error");
  assert.ok(validation.issues.some((issue) => issue.code === "workflow_node_harness_not_found"));
  assert.ok(validation.issues.every((issue) => !issue.message.includes("HarnessRuntimeManager")));
});

test("imports future Rust fields without crashing", () => {
  const exported = canvasToWorkflowExport(defaultPlannerLedAgentWorkflow) as ReturnType<
    typeof canvasToWorkflowExport
  > & { future_field: { keep: true } };
  exported.future_field = { keep: true };

  const imported = parseWorkflowImport(exported);

  assert.equal(imported.id, defaultPlannerLedAgentWorkflow.id);
});

test("preserves Rust hook settings in workflow exports", () => {
  const exported = canvasToWorkflowExport(defaultPlannerLedAgentWorkflow);
  exported.disable_all_hooks = true;
  exported.hooks = {
    PreToolUse: [
      {
        matcher: "Bash",
        hooks: [{ type: "command", command: "echo check" }]
      }
    ]
  };

  const config = workflowExportToProjectConfig(exported);

  assert.equal(config.disable_all_hooks, true);
  assert.equal(config.hooks?.PreToolUse?.[0]?.matcher, "Bash");
  assert.equal(config.hooks?.PreToolUse?.[0]?.hooks[0]?.command, "echo check");
});

test("imports plain Rust ProjectConfig while preserving max rounds and planner", () => {
  const config: RustProjectConfig = canvasToWorkflowSpec(defaultPlannerLedAgentWorkflow);
  const imported = workflowSpecToCanvas(config, "default-planner-led");

  assert.equal(imported.primary_planner_id, "planner");
  assert.equal(imported.loop_policy.max_auto_rounds, defaultPlannerLedAgentWorkflow.loop_policy.max_auto_rounds);
});

test("maps Rust default workflow response into the canvas model", () => {
  const config = canvasToWorkflowSpec(defaultPlannerLedAgentWorkflow);
  const imported = rustDefaultWorkflowToAgentWorkflow({
    workflow_id: "default-planner-led",
    config,
    workflow: config.workflows["default-planner-led"]
  });

  assert.equal(imported.id, "default-planner-led");
  assert.equal(imported.name, defaultPlannerLedAgentWorkflow.name);
  assert.equal(imported.agents.length, 4);
  assert.equal(imported.edges.at(-1)?.loop, true);
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

test("uses Rust run event pagination metadata without local slicing", () => {
  const page = rustRunEventsToRunEventsPage(
    {
      run_id: "run-1",
      event_count: 5,
      returned_count: 2,
      truncated: true,
      next_after_sequence: 4,
      events: [
        {
          event_id: "evt-3",
          run_id: "run-1",
          sequence: 3,
          timestamp: "2026-06-28T00:00:02Z",
          kind: "node.started",
          payload: { node_id: "executor", status: "running" },
          refs: []
        },
        {
          event_id: "evt-4",
          run_id: "run-1",
          sequence: 4,
          timestamp: "2026-06-28T00:00:03Z",
          kind: "node.completed",
          payload: { node_id: "executor", status: "completed" },
          refs: []
        }
      ]
    },
    2,
    2
  );

  assert.equal(page.events.length, 2);
  assert.equal(page.events[0].node_id, "executor");
  assert.equal(page.next_cursor, 4);
  assert.equal(page.has_more, true);
});

test("live run detail uses bounded run metadata and paged events", async () => {
  const calls: string[] = [];
  const originalFetch = globalThis.fetch;
  globalThis.fetch = (async (url: string | URL | Request) => {
    const requestUrl = String(url);
    calls.push(requestUrl);
    if (requestUrl.includes("/api/v3/runs/run-live/events")) {
      return jsonResponse({
        run_id: "run-live",
        event_count: 3,
        returned_count: 1,
        truncated: true,
        next_after_sequence: 1,
        events: [
          {
            event_id: "evt-live-1",
            run_id: "run-live",
            sequence: 1,
            timestamp: "2026-06-28T00:00:01Z",
            kind: "node.started",
            payload: { node_id: "executor", status: "running" },
            refs: []
          }
        ]
      });
    }
    if (requestUrl.includes("/api/v3/runs/run-live?include_events=false")) {
      return jsonResponse({
        run_id: "run-live",
        metadata: {
          run_id: "run-live",
          workflow_id: "planner-led",
          status: "running",
          created_at: "2026-06-28T00:00:00Z",
          updated_at: "2026-06-28T00:00:01Z"
        },
        events: [],
        event_count: 3,
        returned_count: 0,
        report: null,
        repo_evidence_count: 0
      });
    }
    return jsonResponse({ error: `unexpected URL ${requestUrl}` }, 404);
  }) as typeof fetch;

  try {
    const detail = await getLiveAgentRun("run-live");
    assert.equal(detail.id, "run-live");
    assert.equal(detail.status, "running");
    assert.equal(detail.events.length, 1);
    assert.equal(detail.events[0].type, "node.started");
    assert.ok(calls.some((url) => url.includes("/api/v3/runs/run-live?include_events=false")));
    assert.ok(calls.some((url) => url.includes("/api/v3/runs/run-live/events?limit=200")));
    assert.ok(!calls.some((url) => url.endsWith("/api/v3/runs/run-live")));
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("frontend API client uses the Rust v3 planner and run surfaces", () => {
  const apiSource = readFileSync("src/api.ts", "utf8");

  assert.ok(apiSource.includes("/api/v3/planner-chat/sessions"));
  assert.ok(apiSource.includes("canvasToWorkflowSpec(input.agent_workflow)"));
  assert.ok(apiSource.includes("include_events=false"));
  assert.ok(apiSource.includes('params.set("tail", "true")'));
});

test("Planner Chat API keeps execution behind Start Work", () => {
  const apiSource = readFileSync("src/api.ts", "utf8");
  const appSource = readFileSync("src/App.tsx", "utf8");
  const chatTurnSource = sourceBetween(
    apiSource,
    "async function sendRustPlannerChatTurn",
    "async function startRustAgentRun"
  );
  const startWorkSource = sourceBetween(
    apiSource,
    "export async function startPlannerSessionWork",
    "export function getPlannerChatSession"
  );

  assert.ok(chatTurnSource.includes("/api/v3/planner-chat/sessions/${encodeURIComponent(input.session_id)}/turn"));
  assert.ok(chatTurnSource.includes("confirmed: false"));
  assert.ok(chatTurnSource.includes('mode: "discuss"'));
  assert.ok(chatTurnSource.includes("run_id: mappedSession.run_id"));
  assert.ok(!chatTurnSource.includes("run_id: null"));
  assert.ok(!chatTurnSource.includes("/api/v3/runs"));
  assert.ok(startWorkSource.includes("/start-work"));
  assert.ok(!startWorkSource.includes('"/api/v3/runs"'));
  assert.ok(appSource.includes("const response = await sendPlannerChatTurn({"));
  assert.ok(appSource.includes("const response = await startPlannerSessionWork({"));
  assert.ok(!appSource.includes("startLiveAgentRun"));
});

test("desktop skeleton keeps API fallback and desktop scripts opt-in", () => {
  const apiSource = readFileSync("src/api.ts", "utf8");
  const rootPackage = readFileSync("../package.json", "utf8");
  const tauriConfig = readFileSync("../src-tauri/tauri.conf.json", "utf8");
  const rootCargo = readFileSync("../Cargo.toml", "utf8");
  const serverSource = readFileSync("../crates/coder-server/src/lib.rs", "utf8");
  const docs = readFileSync("../README.md", "utf8");

  assert.ok(apiSource.includes("VITE_CODER_API_BASE_URL"));
  assert.ok(apiSource.includes("window.CODER_API_BASE_URL"));
  assert.ok(apiSource.includes("http://127.0.0.1:8876"));
  assert.ok(apiSource.includes("resolveApiUrl"));
  assert.ok(rootPackage.includes("desktop:dev"));
  assert.ok(rootPackage.includes("desktop:build"));
  assert.ok(rootPackage.includes("@tauri-apps/cli@2"));
  assert.ok(tauriConfig.includes("\"devUrl\": \"http://127.0.0.1:5173\""));
  assert.ok(tauriConfig.includes("\"frontendDist\": \"../frontend/dist\""));
  assert.ok(rootCargo.includes("exclude = [\"src-tauri\"]"));
  assert.ok(serverSource.includes("CorsLayer::permissive()"));
  assert.ok(docs.includes("npm run desktop:dev"));
  assert.ok(docs.includes("npm run desktop:build"));
});

test("run summary recognizes backend approval request events", () => {
  const appSource = readFileSync("src/App.tsx", "utf8");

  assert.ok(appSource.includes("approval.requested"));
  assert.ok(appSource.includes("isApprovalRequestEvent"));
});

test("Planner Chat page uses Start Work timeline and hides draft controls", () => {
  const source = readFileSync("src/features/planner-chat/PlannerChatPage.tsx", "utf8");
  const css = readFileSync("src/styles.css", "utf8");
  const legacyDraftLabel = ["Draft", "Plan"].join(" ");
  const legacyDraftLowerLabel = ["Draft", "plan"].join(" ");

  assert.ok(source.includes("Start Work"));
  assert.ok(source.includes("WorkTimeline"));
  assert.ok(source.includes("ReviewChangesCard"));
  assert.ok(!source.includes("message-status"));
  assert.ok(!source.includes("formatRunStatus"));
  assert.ok(!source.includes("runStatus"));
  assert.ok(!source.includes(legacyDraftLowerLabel));
  assert.ok(!source.includes(legacyDraftLabel));
  assert.ok(!source.includes("Discuss"));
  assert.ok(!source.includes("plannerInteractionMode"));
  assert.ok(!css.includes("message-status"));
  assert.ok(!css.includes("planner-state-card"));
  assert.ok(!css.includes("planner-state-strip"));
  assert.ok(!css.includes("memory-proposal-list"));
  const appSource = readFileSync("src/App.tsx", "utf8");
  assert.ok(!appSource.includes(["start", "if", "ready"].join("_")));
  assert.ok(!appSource.includes(["interaction_mode:", '"discuss"'].join(" ")));
});

test("Planner Chat page renders two turns without synthetic status cards", () => {
  const tree = renderPlannerChat(plannerSessionFixture({
    messages: [
      { role: "user", content: "First question" },
      { role: "assistant", content: "First answer" },
      { role: "user", content: "Second question" },
      { role: "assistant", content: "Second answer" }
    ],
    generation: 4
  }));
  const text = collectReactTreeText(tree);
  const classNames = collectReactTreeClassNames(tree);

  assert.ok(text.includes("First question"));
  assert.ok(text.includes("First answer"));
  assert.ok(text.includes("Second question"));
  assert.ok(text.includes("Second answer"));
  assert.ok(!classNames.includes("message-status"));
  assert.ok(!text.includes("Ready"));
  assert.ok(!text.includes(["Draft", "Plan"].join(" ")));
  assert.ok(!text.includes("Discuss"));
  assert.ok(!text.includes(["Work", "mode"].join(" ")));
});

test("Planner Chat renders structured table artifacts as compact cards", () => {
  const tree = renderPlannerChat(plannerSessionFixture({
    messages: [
      { role: "user", content: "Compare files" },
      {
        role: "assistant",
        content: "I summarized the structured details below.",
        artifacts: [
          {
            type: "table",
            title: "Planner table 1",
            columns: ["File", "Change"],
            rows: [["index.html", "add shell"], ["main.js", "add game loop"]]
          }
        ]
      }
    ]
  }));
  const text = collectReactTreeText(tree);
  const classNames = collectReactTreeClassNames(tree);

  assert.ok(text.includes("Planner table 1"));
  assert.ok(text.includes("index.html"));
  assert.ok(text.includes("add game loop"));
  assert.ok(classNames.includes("planner-artifact-card"));
  assert.ok(classNames.includes("planner-artifact-table-wrap"));
  assert.ok(!text.includes("| File | Change |"));
});

test("Planner Chat shell exposes polished empty, loading, and Start Work states", () => {
  const emptyTree = renderPlannerChat(null);
  const readyTree = renderPlannerChat(plannerSessionFixture(), { request: "Implement the accepted plan." });
  const loadingTree = renderPlannerChat(plannerSessionFixture({ run_id: "run-1" }), { plannerLoading: true });
  const emptyClasses = collectReactTreeClassNames(emptyTree);
  const readyClasses = collectReactTreeClassNames(readyTree);
  const loadingText = collectReactTreeText(loadingTree);
  const loadingClasses = collectReactTreeClassNames(loadingTree);

  assert.ok(emptyClasses.includes("chat-empty-panel"));
  assert.ok(readyClasses.includes("message-bubble"));
  assert.ok(readyClasses.includes("composer-actions"));
  assert.ok(readyClasses.includes("start-work-action primary-action"));
  assert.ok(loadingText.includes("Planner is responding..."));
  assert.ok(loadingClasses.includes("chat-loading-row"));
});

test("Planner Chat renders stored sessions without task state", () => {
  const legacySession = {
    ...plannerSessionFixture(),
    task_state: undefined
  } as unknown as PlannerChatSession;
  const tree = renderPlannerChat(legacySession);
  const text = collectReactTreeText(tree);
  const classNames = collectReactTreeClassNames(tree);

  assert.ok(text.includes("Please plan the change."));
  assert.ok(text.includes("The plan is ready."));
  assert.ok(!classNames.includes("start-work-action primary-action"));
});

test("Planner Chat composer disables input only while a request is in flight", () => {
  const idleTree = renderPlannerChat(null, { request: "hello", plannerLoading: false });
  const busyTree = renderPlannerChat(null, { request: "hello", plannerLoading: true });
  const idleComposer = findElementByPlaceholder(idleTree, "Message the Planner...");
  const busyComposer = findElementByPlaceholder(busyTree, "Message the Planner...");

  assert.equal(Boolean(idleComposer?.props?.disabled), false);
  assert.equal(Boolean(busyComposer?.props?.disabled), true);
});

test("Planner Chat remains available while a workflow runs", () => {
  const tree = renderPlannerChat(plannerSessionFixture({ status: "running" }), {
    request: "Plan the next task.",
    workflowRunning: true
  });
  const composer = findElementByPlaceholder(tree, "Message the Planner...");
  const text = collectReactTreeText(tree);
  const classNames = collectReactTreeClassNames(tree);

  assert.equal(Boolean(composer?.props?.disabled), false);
  assert.ok(text.includes("Workflow is running. Planner Chat remains available."));
  assert.ok(text.includes("Stop work"));
  assert.ok(classNames.includes("stop-work-action"));
  assert.ok(!classNames.includes("start-work-action primary-action"));
});

test("App navigation hides Plugins & Skills outside debug UI", () => {
  const defaultTree = AppSidebar({
    activeSection: "chat",
    status: "Ready",
    onSectionChange: () => undefined
  });
  const debugTree = AppSidebar({
    activeSection: "extensions",
    status: "Ready",
    onSectionChange: () => undefined,
    showExtensions: true
  });
  const appSource = readFileSync("src/App.tsx", "utf8");
  const architectureDocs = readFileSync("../docs/ARCHITECTURE.md", "utf8");

  assert.ok(collectReactTreeText(defaultTree).includes("Planner Chat"));
  assert.ok(collectReactTreeText(defaultTree).includes("Settings"));
  assert.ok(collectReactTreeText(defaultTree).includes("Advanced"));
  assert.ok(collectReactTreeText(defaultTree).includes("Developer"));
  assert.ok(collectReactTreeText(defaultTree).includes("Workflow editor"));
  assert.ok(!collectReactTreeText(defaultTree).includes("Agent Workflow"));
  assert.ok(!collectReactTreeText(defaultTree).includes("Plugins & Skills"));
  assert.ok(collectReactTreeText(debugTree).includes("Plugins & Skills"));
  assert.ok(appSource.includes('useState<AppSection>("chat")'));
  assert.ok(appSource.includes("showExtensions={debugUiEnabled}"));
  assert.ok(appSource.includes('activeSection === "extensions" && debugUiEnabled'));
  assert.ok(appSource.includes('get("debug") === "1"'));
  assert.ok(appSource.includes('window.localStorage.getItem("coder_debug_ui") === "1"'));
  assert.ok(architectureDocs.includes("ordinary product sidebar starts at Planner Chat"));
  assert.ok(architectureDocs.includes("Plugins & Skills"));
  assert.ok(architectureDocs.includes("explicit debug UI"));
});

test("Work timeline renders public ReAct items without raw backend details", () => {
  const tree = WorkTimeline({
    runId: "run-1",
    items: [
      {
        type: "reasoning_summary",
        id: "reason-1",
        agent_id: "executor",
        summary_text: ["Need inspect repo state."],
        created_at: "2026-01-01T00:00:00Z"
      },
      {
        type: "executor_step",
        id: "action-1",
        agent_id: "executor",
        title: "Action selected",
        status: "selected",
        summary: "Selected repo_find_files.",
        created_at: "2026-01-01T00:00:01Z"
      },
      {
        type: "tool_call",
        id: "tool-1",
        agent_id: "executor",
        tool_name: "repo_find_files",
        status: "completed",
        summary: "Found README.md",
        created_at: "2026-01-01T00:00:02Z"
      }
    ]
  });
  const text = collectReactTreeText(tree);

  assert.ok(text.includes("Work timeline"));
  assert.ok(text.includes("Need inspect repo state."));
  assert.ok(text.includes("Action selected"));
  assert.ok(text.includes("repo_find_files"));
  assert.ok(!text.includes("raw_ref"));
});

test("Work timeline explains a complete run with compact command output", () => {
  const items: TimelineItem[] = [
    {
      type: "planner_message",
      id: "planner-1",
      agent_id: "planner",
      content: "Planner prepared the run.",
      created_at: "2026-01-01T00:00:00Z"
    },
    {
      type: "reasoning_summary",
      id: "reason-1",
      agent_id: "executor",
      summary_text: ["Need inspect repo state before editing."],
      created_at: "2026-01-01T00:00:01Z"
    },
    {
      type: "executor_step",
      id: "action-1",
      agent_id: "executor",
      title: "Action selected",
      status: "selected",
      summary: "Selected repo_find_files.",
      created_at: "2026-01-01T00:00:02Z"
    },
    {
      type: "tool_call",
      id: "tool-1",
      agent_id: "executor",
      tool_name: "repo_find_files",
      status: "started",
      summary: "Scanning repository files.",
      evidence_ref: "blob://sha256/raw-tool-start",
      created_at: "2026-01-01T00:00:03Z"
    },
    {
      type: "tool_call",
      id: "tool-2",
      agent_id: "executor",
      tool_name: "repo_find_files",
      status: "completed",
      summary: "Found README.md.",
      evidence_ref: "blob://sha256/raw-tool-complete",
      created_at: "2026-01-01T00:00:04Z"
    },
    {
      type: "executor_step",
      id: "observation-1",
      agent_id: "executor",
      title: "Observation recorded",
      status: "completed",
      summary: "README.md needs the requested update.",
      created_at: "2026-01-01T00:00:05Z"
    },
    {
      type: "command_execution",
      id: "command-1",
      agent_id: "executor",
      command: ["cargo", "test"],
      cwd: ".",
      status: "completed",
      stdout_preview: "test result: ok",
      stderr_preview: null,
      exit_code: 0,
      duration_ms: 1530,
      evidence_ref: "blob://sha256/raw-command",
      created_at: "2026-01-01T00:00:06Z"
    },
    {
      type: "file_change",
      id: "file-1",
      agent_id: "executor",
      path: "README.md",
      change_type: "modified",
      diff_ref: "blob://sha256/raw-diff",
      created_at: "2026-01-01T00:00:07Z"
    },
    {
      type: "approval",
      id: "approval-1",
      agent_id: "executor",
      risk_level: "medium",
      action_type: "command",
      summary: "Command required confirmation.",
      status: "blocked",
      created_at: "2026-01-01T00:00:08Z"
    },
    {
      type: "verification",
      id: "verification-1",
      agent_id: "executor",
      status: "completed",
      summary: "Tests passed.",
      evidence_ref: "blob://sha256/raw-verification",
      created_at: "2026-01-01T00:00:09Z"
    },
    {
      type: "final_summary",
      id: "summary-1",
      agent_id: "planner",
      status: "completed",
      summary:
        "Status: completed\nRequested: Update README.md\nDone: Updated README.md\nChanged files: README.md\nVerification: cargo test: completed exit 0\nEvidence: 1 evidence ref(s) recorded: event_log.\nRemaining risks: No remaining blocker or risk was recorded.\nNext steps: No next step was recorded.",
      changed_files: ["README.md"],
      checks: ["cargo test: completed exit 0"],
      evidence_refs: [{ kind: "native_raw_event", reference: "blob://sha256/raw-final" }],
      blockers: [],
      next_steps: [],
      created_at: "2026-01-01T00:00:10Z"
    }
  ];
  const tree = WorkTimeline({ runId: "run-1", items });
  const text = collectReactTreeText(tree);
  const classNames = collectReactTreeClassNames(tree);

  assert.ok(text.includes("11 public steps"));
  assert.ok(text.includes("Planner message"));
  assert.ok(text.includes("Planner prepared the run."));
  assert.ok(text.includes("Executor reasoning"));
  assert.ok(text.includes("Need inspect repo state before editing."));
  assert.ok(text.includes("Action selected"));
  assert.ok(text.includes("Tool running: repo_find_files"));
  assert.ok(text.includes("Tool completed: repo_find_files"));
  assert.ok(text.includes("Observation recorded"));
  assert.ok(text.includes("Command execution"));
  assert.ok(text.includes("cargo test"));
  assert.ok(text.includes("cwd ."));
  assert.ok(text.includes("exit 0"));
  assert.ok(text.includes("1.5 s"));
  assert.ok(text.includes("Command output"));
  assert.ok(text.includes("test result: ok"));
  assert.ok(text.includes("File change"));
  assert.ok(text.includes("README.md"));
  assert.ok(text.includes("Approval"));
  assert.ok(text.includes("Blocked"));
  assert.ok(text.includes("Verification"));
  assert.ok(text.includes("Final summary"));
  assert.ok(text.includes("Requested: Update README.md"));
  assert.ok(text.includes("Remaining risks: No remaining blocker or risk was recorded."));
  assert.ok(text.includes("Next steps: No next step was recorded."));
  assert.ok(text.includes("cargo test: completed exit 0"));
  assert.ok(text.includes("1 evidence refs"));
  assert.ok(classNames.includes("timeline-command-output"));
  assert.ok(classNames.includes("timeline-tone-success"));
  assert.ok(classNames.includes("timeline-tone-warning"));
  assert.ok(!text.includes("raw_ref"));
  assert.ok(!text.includes("blob://sha256"));
});

test("Work timeline shows a clear empty progress state", () => {
  const tree = WorkTimeline({ runId: "run-1", items: [] });
  const text = collectReactTreeText(tree);
  const classNames = collectReactTreeClassNames(tree);

  assert.ok(text.includes("Work has started"));
  assert.ok(text.includes("Timeline events will appear here"));
  assert.ok(classNames.includes("timeline-empty"));
});

test("Work timeline renders safely with undefined null or malformed items", () => {
  const undefinedTree = WorkTimeline({ runId: "run-1", items: undefined });
  const nullTree = WorkTimeline({ runId: "run-1", items: null });
  const malformedTree = WorkTimeline({ runId: "run-1", items: { items: [] } as unknown as TimelineItem[] });
  const malformedArrayTree = WorkTimeline({ runId: "run-1", items: [null] as unknown as TimelineItem[] });

  assert.ok(collectReactTreeText(undefinedTree).includes("Work has started"));
  assert.ok(collectReactTreeText(nullTree).includes("Work has started"));
  assert.ok(collectReactTreeText(malformedTree).includes("Work has started"));
  assert.ok(collectReactTreeText(malformedArrayTree).includes("Work has started"));
});

test("Review Changes stays hidden without changes and shows undo conflicts", () => {
  const empty = ReviewChangesCard({
    changeSets: [],
    diffByChangeSetId: {},
    loadingChangeSetId: null,
    onAccept: () => undefined,
    onLoadDiff: () => undefined,
    onUndo: () => undefined
  });
  assert.equal(empty, null);

  const conflicted = ReviewChangesCard({
    changeSets: [
      {
        change_set_id: "changeset-current",
        run_id: "run-1",
        repo_root: ".",
        status: "failed_to_undo",
        created_at: "2026-01-01T00:00:00Z",
        base_git_head: null,
        before_checkpoint_ref: null,
        after_diff_ref: "artifact://runs/run-1/artifacts/changeset-current.json",
        reverse_patch_ref: "artifact://runs/run-1/artifacts/changeset-current.json#reverse-git-apply",
        changed_files: [{ path: "tracked.txt", change_type: "modified" }],
        command_checks: [],
        evidence_refs: [],
        after_diff: "diff --git a/tracked.txt b/tracked.txt",
        diff_truncated: false,
        undo_conflict: "Undo refused because diff content changed for: tracked.txt."
      }
    ],
    diffByChangeSetId: {},
    loadingChangeSetId: null,
    onAccept: () => undefined,
    onLoadDiff: () => undefined,
    onUndo: () => undefined
  });
  const text = collectReactTreeText(conflicted);
  const classNames = collectReactTreeClassNames(conflicted);

  assert.ok(text.includes("Review changes"));
  assert.ok(text.includes("Diff is not loaded yet."));
  assert.ok(text.includes("diff content changed for: tracked.txt"));
  assert.ok(text.includes("tracked.txt"));
  assert.ok(classNames.includes("review-diff-state"));
  assert.ok(classNames.includes("change-set-failed_to_undo"));
});

test("Review Changes renders safely with undefined null or malformed change sets", () => {
  const baseProps = {
    diffByChangeSetId: {},
    loadingChangeSetId: null,
    onAccept: () => undefined,
    onLoadDiff: () => undefined,
    onUndo: () => undefined
  };

  assert.equal(ReviewChangesCard({ ...baseProps, changeSets: undefined }), null);
  assert.equal(ReviewChangesCard({ ...baseProps, changeSets: null }), null);
  assert.equal(ReviewChangesCard({ ...baseProps, changeSets: { changes: [] } as unknown as [] }), null);
  assert.equal(ReviewChangesCard({ ...baseProps, changeSets: [null] as unknown as [] }), null);
});

test("Review Changes renders loaded diffs with readable diff class", () => {
  const tree = ReviewChangesCard({
    changeSets: [
      {
        change_set_id: "changeset-current",
        run_id: "run-1",
        repo_root: ".",
        status: "pending_review",
        created_at: "2026-01-01T00:00:00Z",
        base_git_head: null,
        before_checkpoint_ref: null,
        after_diff_ref: "artifact://runs/run-1/artifacts/changeset-current.json",
        reverse_patch_ref: null,
        changed_files: [{ path: "tracked.txt", change_type: "modified" }],
        command_checks: [{ command: "npm run test", status: "passed", exit_code: 0 }],
        evidence_refs: [],
        after_diff: "diff --git a/tracked.txt b/tracked.txt",
        diff_truncated: false,
        undo_conflict: null
      }
    ],
    diffByChangeSetId: {
      "changeset-current": "diff --git a/tracked.txt b/tracked.txt\n-old\n+new"
    },
    loadingChangeSetId: null,
    onAccept: () => undefined,
    onLoadDiff: () => undefined,
    onUndo: () => undefined
  });
  const text = collectReactTreeText(tree);
  const classNames = collectReactTreeClassNames(tree);

  assert.ok(text.includes("diff --git a/tracked.txt b/tracked.txt"));
  assert.ok(text.includes("npm run test"));
  assert.ok(classNames.includes("review-diff"));
  assert.ok(classNames.includes("change-set-pending_review"));
});

test("Start Work review loading failure keeps chat visible", () => {
  const tree = renderPlannerChat(plannerSessionFixture({ run_id: "run-1" }), {
    activeRunId: "run-1",
    reviewStateError: "Work results failed to load; chat remains available. timeline: connection refused",
    timelineItems: [],
    changeSets: []
  });
  const text = collectReactTreeText(tree);
  const classNames = collectReactTreeClassNames(tree);
  const appSource = readFileSync("src/App.tsx", "utf8");

  assert.ok(text.includes("Please plan the change."));
  assert.ok(text.includes("Work results could not be loaded."));
  assert.ok(text.includes("Work has started"));
  assert.ok(classNames.includes("work-state-error"));
  assert.ok(appSource.includes("Promise.allSettled"));
  assert.ok(appSource.includes("setTimelineItems([])"));
  assert.ok(appSource.includes("setChangeSets([])"));
  assert.ok(appSource.includes("setReviewStateError(message)"));
});

test("Timeline and changes API responses normalize malformed payloads", () => {
  assert.deepEqual(normalizeRunTimelineResponse("run-1", null), { run_id: "run-1", items: [] });
  assert.deepEqual(normalizeRunTimelineResponse("run-1", { run_id: "run-x" }), { run_id: "run-x", items: [] });
  assert.deepEqual(normalizeRunChangeSetsResponse("run-1", undefined), { run_id: "run-1", changes: [] });
  assert.deepEqual(normalizeRunChangeSetsResponse("run-1", { changes: "bad" } as never), { run_id: "run-1", changes: [] });
});

test("Core UI styles include responsive chat and review polish hooks", () => {
  const css = readFileSync("src/styles.css", "utf8");

  assert.ok(css.includes(".message-bubble"));
  assert.ok(css.includes(".composer-actions"));
  assert.ok(css.includes(".review-diff-state"));
  assert.ok(css.includes(".work-state-error"));
  assert.ok(css.includes(".render-error-panel"));
  assert.ok(css.includes(".timeline-empty"));
  assert.ok(css.includes("@media (max-width: 640px)"));
  assert.ok(css.includes("white-space: pre;"));
});

test("Review and Undo docs cover binary and untracked file handling", () => {
  const docs = readFileSync("../docs/REVIEW_AND_UNDO.md", "utf8");

  assert.ok(docs.includes("Binary changes"));
  assert.ok(docs.includes("Untracked files"));
  assert.ok(docs.includes("git reset --hard"));
});

test("Rust v3 smoke exercises Planner to Review end-to-end path", () => {
  const smokeScript = readFileSync("../scripts/smoke-rust-v3.ps1", "utf8");

  for (const needle of [
    "/api/v3/providers/settings",
    "mock_mode = $mockMode",
    "product_validation",
    "plumbing",
    "/api/v3/planner-chat/sessions",
    "Send-PlannerTurn",
    "should_start_workflow",
    "/start-work",
    "timeline_url",
    "/report/preview",
    "/changes",
    "/diff",
    "/undo",
    "New-SmokeRepo",
    "README.md",
    "Review Changes returned no change sets",
    "Undo did not restore README.md"
  ]) {
    if (!smokeScript.includes(needle)) {
      throw new Error(`smoke-rust-v3.ps1 missing ${needle}`);
    }
  }
});

test("Provider Settings exposes DeepSeek preset and exact test result UI", () => {
  const panelSource = readFileSync("src/components/ProviderSettingsPanel.tsx", "utf8");
  const hookSource = readFileSync("src/hooks/useProviderSettings.ts", "utf8");
  const apiSource = readFileSync("src/api.ts", "utf8");
  const appSource = readFileSync("src/App.tsx", "utf8");
  const liveSmokeScript = readFileSync("../scripts/live-llm-smoke.ps1", "utf8");
  const providerDocs = readFileSync("../docs/PROVIDER_SETUP.md", "utf8");

  assert.ok(panelSource.includes("DeepSeek preset"));
  assert.ok(panelSource.includes("Test Provider"));
  assert.ok(panelSource.includes("Provider Network"));
  assert.ok(panelSource.includes("Direct"));
  assert.ok(panelSource.includes("Current environment"));
  assert.ok(panelSource.includes("Explicit proxy"));
  assert.ok(panelSource.includes("Provider Proxy URL"));
  assert.ok(panelSource.includes("explicit proxy"));
  assert.ok(panelSource.includes("Clear API Key"));
  assert.ok(panelSource.includes("showMockMode"));
  assert.ok(panelSource.includes("Test succeeded"));
  assert.ok(panelSource.includes("Test failed"));
  assert.ok(panelSource.includes('type="password"'));
  assert.ok(panelSource.includes('autoComplete="off"'));
  assert.ok(panelSource.includes("testResult.model"));
  assert.ok(panelSource.includes("testResult.endpoint"));
  assert.ok(panelSource.includes("deepseek"));
  assert.ok(panelSource.includes("openai-compatible"));
  assert.ok(panelSource.includes("custom"));
  assert.ok(hookSource.includes('default_provider: "deepseek"'));
  assert.ok(hookSource.includes("deepseek-v4-flash"));
  assert.ok(hookSource.includes("https://api.deepseek.com"));
  assert.ok(hookSource.includes('proxy_mode: "direct"'));
  assert.ok(hookSource.includes("defaultProxyModeForProvider"));
  assert.ok(hookSource.includes("proxy_urls: proxyUrls"));
  assert.ok(hookSource.includes("proxy_modes: proxyModes"));
  assert.ok(hookSource.includes("api_keys: { [provider]: null }"));
  assert.ok(hookSource.includes("mock_mode: false"));
  assert.ok(hookSource.includes("buildProviderSettingsPayload(providerForm, providerSettings)"));
  assert.ok(hookSource.includes("Saving provider ${provider} before test"));
  assert.ok(appSource.includes("showMockMode={debugUiEnabled}"));
  assert.ok(liveSmokeScript.includes("CODER_LIVE_LLM_SMOKE"));
  assert.ok(liveSmokeScript.includes("should_start_workflow"));
  assert.ok(liveSmokeScript.includes("Start Work returned neither a run_id nor a Planner clarification."));
  assert.ok(providerDocs.includes("$env:CODER_LIVE_LLM_SMOKE=\"1\""));
  assert.ok(providerDocs.includes("does not write plaintext"));
  assert.ok(providerDocs.includes("or print them"));
});

test("Planner Chat shows provider setup before chat when credentials are missing", () => {
  const tree = renderPlannerChat(null, {
    providerSetupRequired: true,
    providerSetupMessage: "Configure a provider in Settings before I can plan or execute work."
  });
  const text = collectReactTreeText(tree);
  const composer = findElementByPlaceholder(tree, "Message the Planner...");

  assert.ok(text.includes("Provider setup required"));
  assert.ok(text.includes("Configure a provider in Settings before I can plan or execute work."));
  assert.ok(text.includes("Open Settings"));
  assert.equal(Boolean(composer?.props?.disabled), true);
});

test("live selftest covers generic runtime boundary probes", () => {
  const liveSelftestScript = readFileSync("../scripts/live-coder-selftest-suite.ps1", "utf8");

  assert.ok(liveSelftestScript.includes("Invoke-RuntimeBoundaryProbe"));
  assert.ok(liveSelftestScript.includes("pre_tool_use_hooks"));
  assert.ok(liveSelftestScript.includes("command_run"));
  assert.ok(liveSelftestScript.includes("BashOutputTool"));
  assert.ok(liveSelftestScript.includes("TaskStop"));
  assert.ok(liveSelftestScript.includes("agent_subagent"));
  assert.ok(liveSelftestScript.includes("TaskOutput"));
  assert.ok(liveSelftestScript.includes("ToolName \"TaskStop\""));
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
    plannerLoading: false,
    scopesText: "",
    submittedRequest: "",
    timelineItems: [],
    workflowRunning: false,
    plannerSession,
    plannerStrength: "balanced",
    providerSetupRequired: false,
    providerSetupMessage: "",
    reviewStateError: null,
    onAcceptChangeSet: () => undefined,
    onLoadChangeSetDiff: () => undefined,
    onOpenProviderSettings: () => undefined,
    onRepoChange: () => undefined,
    onRequestChange: () => undefined,
    onScopesTextChange: () => undefined,
    onPlannerStrengthChange: () => undefined,
    onCancelWork: () => undefined,
    onStartWork: () => undefined,
    onSubmitRequest: () => undefined,
    onUndoChangeSet: () => undefined,
    ...overrides
  });
}

function plannerSessionFixture(overrides: Partial<PlannerChatSession> = {}): PlannerChatSession {
  return {
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
      { role: "user", content: "Please plan the change." },
      { role: "assistant", content: "The plan is ready." }
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
      readiness: "ready_to_execute"
    },
    generation: 2,
    last_turn: null,
    run_id: null,
    work_in_progress: false,
    status: "chatting",
    ...overrides
  };
}

function collectReactTreeText(node: unknown): string {
  if (node === null || typeof node === "undefined" || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(collectReactTreeText).join("");
  if (typeof node !== "object") return "";
  const element = node as { type?: unknown; props?: { children?: unknown } };
  if (typeof element.type === "function") {
    return collectReactTreeText(element.type(element.props ?? {}));
  }
  const props = element.props;
  return collectReactTreeText(props?.children);
}

function collectReactTreeClassNames(node: unknown): string {
  if (node === null || typeof node === "undefined" || typeof node === "boolean") return "";
  if (Array.isArray(node)) return node.map(collectReactTreeClassNames).join(" ");
  if (typeof node !== "object") return "";
  const element = node as { type?: unknown; props?: { children?: unknown; className?: unknown } };
  if (typeof element.type === "function") {
    return collectReactTreeClassNames(element.type(element.props ?? {}));
  }
  const props = element.props;
  return [
    typeof props?.className === "string" ? props.className : "",
    collectReactTreeClassNames(props?.children)
  ].filter(Boolean).join(" ");
}

function findElementByPlaceholder(
  node: unknown,
  placeholder: string
): { props?: { children?: unknown; placeholder?: unknown; disabled?: unknown } } | null {
  if (node === null || typeof node === "undefined" || typeof node === "boolean") return null;
  if (Array.isArray(node)) {
    for (const item of node) {
      const found = findElementByPlaceholder(item, placeholder);
      if (found) return found;
    }
    return null;
  }
  if (typeof node !== "object") return null;
  const element = node as { type?: unknown; props?: { children?: unknown; placeholder?: unknown; disabled?: unknown } };
  if (typeof element.type === "function") {
    return findElementByPlaceholder(element.type(element.props ?? {}), placeholder);
  }
  if (element.props?.placeholder === placeholder) return element;
  return findElementByPlaceholder(element.props?.children, placeholder);
}

function jsonResponse(payload: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    statusText: status >= 200 && status < 300 ? "OK" : "Error",
    json: async () => payload,
    text: async () => JSON.stringify(payload)
  } as Response;
}

function sourceBetween(source: string, startNeedle: string, endNeedle: string): string {
  const start = source.indexOf(startNeedle);
  const end = source.indexOf(endNeedle, start + startNeedle.length);
  assert.ok(start >= 0);
  assert.ok(end > start);
  return source.slice(start, end);
}

await runTests();
