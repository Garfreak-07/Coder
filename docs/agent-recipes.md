# Agent Recipes

`AgentRecipe` is the ordinary user-facing Agent definition:

```text
id
name
role
purpose
behavior_notes
preferred_extension_ids
```

Supported recipe roles:

- `planner`
- `do_work`
- `check_result`
- `organize`
- `research`
- `write_draft`

`RuntimeProfileCompiler` compiles each recipe into an internal
`AgentRuntimeProfile` with engine id, context profile, token budget, artifact
policy, plugin policy, skill policy, memory policy, repair policy, and tool
policy.
Research and draft recipes currently compile to the registered
`synthesizer-engine` knowledge-worker fallback.

`RuntimeProfileCache` keys compiled profiles by workflow shape, installed
extension versions, and planner settings. The ordinary user model stays the same;
the cache only avoids repeating deterministic compilation inside the runtime.

The compatibility `AgentWorkflowAgent.capabilities` field may still exist in
saved workflows, but ordinary creation can omit it. Defaults are derived from
the Agent role or role card.

Planner remains the only Agent that can ask the user or decide global
`continue`, `ask_human`, `finish`, and `stop` outcomes. `RunController` enforces
that loop boundary after each `PlannerDecision`.

## v0.9.5 Boundary

- Ordinary users define Agent intent; runtime profiles remain internal.
- `RunController` owns round continuation after Planner decisions.
- `BudgetBroker` controls resource reservations implied by compiled profiles.
- `ActionGateway` is where profile tool/context policies become runtime action
  requests.
- `AgentRun` and `AgentEngineRegistry` own Planner, Worker, Tester,
  FinalReview, Synthesizer, and PlannerDecision execution behind compiled
  profile engine ids.
- Partitioned stores keep profile diagnostics, metadata, results, ledgers,
  artifacts, contexts, tool results, live runs, and cache data separated.
- Legacy `WorkflowSpec` compilation is limited to
  `compile_agent_workflow_legacy_preview()` for preview/migration.
