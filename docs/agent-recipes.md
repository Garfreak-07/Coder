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

`RuntimeProfileCache` keys compiled profiles by workflow shape, installed
extension versions, and planner settings. The ordinary user model stays the same;
the cache only avoids repeating deterministic compilation inside the runtime.

The compatibility `AgentWorkflowAgent.capabilities` field may still exist in
saved workflows, but ordinary creation can omit it. Defaults are derived from
the Agent role or role card.

Planner remains the only Agent that can ask the user or decide global
`continue`, `ask_human`, `finish`, and `stop` outcomes. `RunController` enforces
that loop boundary after each `PlannerDecision`.

## v0.9.2 Boundary

- Ordinary users define Agent intent; runtime profiles remain internal.
- `RunController` owns round continuation after Planner decisions.
- `BudgetBroker` controls resource reservations implied by compiled profiles.
- `ActionGateway` is where profile tool/context policies become runtime action
  requests.
- `AgentEngineRegistry` owns Planner, Worker, Tester, FinalReview, and
  Synthesizer execution behind compiled profile engine ids.
- Partitioned stores keep profile diagnostics, ledgers, artifacts, and cache
  data separated.
- Legacy `WorkflowSpec` compilation is limited to
  `compile_agent_workflow_legacy_preview()` for preview/migration.
