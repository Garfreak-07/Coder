# Legacy Deletion Plan

The goal is one ordinary AgentGraph product path. Legacy runtime pieces are
removed only after product references are gone and tests protect the boundary.

## Order

1. Add architecture boundary tests.
2. Stop new product calls to legacy runtime paths.
3. Keep `WorkflowSpec` / `WorkflowRunner` only for compatibility preview.
4. Migrate product UI away from runtime JSON editing.
5. Migrate patch/check/repair/context code behind `ActionGateway`.
6. Move or delete legacy modules once no product tests or endpoints depend on
   them.

## Current v0.9.1 Boundary

`compile_agent_workflow_legacy_preview()` is the explicit compiler for advanced
preview and migration/debug only. `compile_agent_workflow()` remains a
compatibility alias until callers have moved.

Product live Agent runs use:

```text
AgentWorkflowSpec
-> RunController
-> AgentGraphRunner
-> ActionGateway
-> PlannerDecision
```

They must not compile into `WorkflowSpec` or run through `WorkflowRunner`.

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

## v0.9.1 Boundary

- Ordinary user workflows remain AgentGraph-first.
- `RunController` replaces inline PlannerDecision loop handling.
- `BudgetBroker` replaces ad hoc pre-execution resource checks.
- `ActionGateway` replaces direct product calls to context, patch, command, and
  repair services.
- Partitioned stores make it possible to delete old mixed run layouts in staged
  passes.
- Legacy preview is explicit; new product behavior must not depend on
  `WorkflowRunner`.
