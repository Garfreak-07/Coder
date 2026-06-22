# Legacy Deletion Plan

The goal is one ordinary AgentGraph product path. Legacy runtime pieces are
removed only after product references are gone and tests protect the boundary.

## Order

1. Add architecture boundary tests.
2. Stop new product calls to legacy runtime paths.
3. Keep `WorkflowSpec` / `WorkflowRunner` only for compatibility tests and old
   saved workflow support.
4. Migrate product UI away from runtime JSON editing.
5. Migrate patch/check/repair/context code behind `ActionGateway`.
6. Move Planner/Tester/FinalReview/Synthesizer execution behind
   `AgentEngineRegistry`.
7. Move or delete legacy modules once no product tests or endpoints depend on
   them.

## Current v0.9.7 Boundary

`compile_agent_workflow_legacy_preview()` and `compile_agent_workflow()` remain
legacy compatibility helpers, but they are no longer exposed through the product
server API.

Product live Agent runs use:

```text
AgentWorkflowSpec
-> PlannerOrder.plan_graph
-> RunController
-> RunGuard
-> BudgetBroker round preflight
-> GraphRunCache
-> AgentGraphRunner
-> ActionGateway
-> ContextService
-> AgentRun
-> PlannerStrategy
-> AgentEngineRegistry
-> Engines
-> PlannerInputBundle
-> PlannerDecision
-> RunController
```

They must not compile into `WorkflowSpec`, run through `WorkflowRunner`, or
construct `AgentGraphExecutor` from the product runner path.

The product default AgentWorkflow response no longer includes legacy preview
fields:

```text
workflow
runtime_boundary
runtime_type
deprecated
```

`/api/v2/agent-workflows/compile` returns `410 Gone`.

`/api/v2/live-runs` remains as a compatibility endpoint and is marked
`deprecated=true`. Product AgentGraph runs use `/api/v2/live-agent-runs`, and
creation responses return `live-agent-runs` event/result URLs. Legacy
`/api/v2/live-runs/{run_id}` and `/events` return `410 Gone` for AgentGraph run
ids with migration URLs.

## Legacy Artifacts

`plan_artifact`, `patch_artifact`, and `review_artifact` are compatibility
artifacts for old saved workflows. New product AgentGraph runs use:

- `planner_order`
- `execution_result`
- `test_result`
- `planner_decision`
- `round_summary`
- coding diagnostics such as `patch_preview`, `check_result`, and
  `debug_finding`

The next deletion pass should migrate tests that still intentionally exercise
legacy artifacts, then remove legacy artifact production from non-preview
paths.

## v0.9.7 Boundary Rules

- Ordinary user workflows remain AgentGraph-first.
- `RunController` replaces inline PlannerDecision loop handling.
- `BudgetBroker` replaces ad hoc pre-execution resource checks.
- `ActionGateway` replaces direct product calls to context, patch, command,
  sandbox, plugin, MCP, repo intelligence, artifact validation, and repair
  services.
- Declared `ActionSpec` action types must be implemented by `ActionGateway`;
  direct `load_skill` is not a product runtime action.
- Plugin and MCP execution must read registry `ToolCapability` before runtime
  dispatch; unknown or approval-gated operations must not execute silently.
- Unknown worker-requested runtime actions must create failed `runtime_action`
  artifacts, not disappear from Planner-visible effects.
- Blocked plugin/MCP runtime actions must preserve `approval_key`, policy,
  original `ActionSpec`, and `work_item_id`; approved replay must use
  `ActionGateway` without rerunning worker model output generation.
- `AgentRun` and `AgentEngineRegistry` own product Agent execution;
  `AgentGraphExecutor` is a compatibility adapter only.
- Coding auto-loop effects keep patch preview, sandbox apply, check result,
  requested runtime action, and DebugFinding artifact refs in
  `PlannerInputBundle.effects`.
- Partitioned stores are the explicit write path for metadata, results, events,
  artifacts, blobs, ledgers, contexts, tool results, live runs, extensions, and
  cache data.
- Legacy preview is quarantined; new product behavior must not depend on
  `WorkflowSpec`, `WorkflowRunner`, `run_workflow`, or `legacy_compile`.
