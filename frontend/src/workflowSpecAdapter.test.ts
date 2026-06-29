import { defaultPlannerLedAgentWorkflow } from "./examples";
import { readFileSync } from "node:fs";
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

test("Planner Chat page renders memory proposal review surface", () => {
  const source = readFileSync("src/features/planner-chat/PlannerChatPage.tsx", "utf8");

  assert.ok(source.includes("Memory proposals"));
  assert.ok(source.includes("memory_proposals"));
});
