# Agent Engines

Agents do not call tools directly. Runtime compiles each Agent into a runtime
profile and dispatches work through `AgentRun` and `AgentEngineRegistry`.

Current default execution path:

```text
AgentGraphRunner
-> AgentRun
-> AgentEngineRegistry
-> PlannerEngine / CodeWorkerEngine / TesterEngine / FinalReviewEngine / SynthesizerEngine
-> structured artifact
```

`AgentGraphExecutor` remains only to preserve compatibility call sites and
tests. The product runner does not construct it. Do not add prompt building,
repair logic, mock payload construction, or artifact-specific execution there.

Registered default engines:

- `planner-engine`: creates `PlannerOrder` and `PlannerDecision`.
- `code-worker-engine`: runs bounded coding work through `CodeWorkerHarness`.
- `tester-engine`: creates per-work-item `test_result` artifacts.
- `final-review-engine`: aggregates round evidence into a final `test_result`.
- `synthesizer-engine`: creates `synthesis_artifact` output for organizer-style
  Agents.

Research and draft roles currently compile to the knowledge-worker
`synthesizer-engine` fallback. Dedicated research or draft engines may be added
later as installable AgentEngine packages, but compiled default profiles must
always resolve to a registered engine.

Agent engines receive prepared envelopes. They should not call `ContextService`,
`PatchService`, `CommandService`, artifact validation, or repair services
directly. New low-level work enters through `ActionGateway`, which reserves
budget through `BudgetBroker` first.

`AgentEngineSpec` and `HarnessGraph` define installable engine structure without
exposing it in ordinary UI.

`HarnessValidator` enforces core boundaries:

- context builder, artifact validator, and output artifact are required
- loops require max steps
- worker and tester engines cannot ask the human
- tester engines cannot write files
- external effects require preview metadata
- plugin operations require permission metadata

Model calls inside the default AgentGraph engines reserve model budget before
invocation when a real model is configured. Mock-mode execution does not consume
model-call budget.

## v0.9.5 Boundary

- Ordinary users choose Agents; engine graphs remain hidden runtime internals.
- `RunController` controls whether engine output can lead to another round.
- `BudgetBroker` gates model calls and low-level engine actions.
- `ActionGateway` is the approved bridge from engine/runtime intent to context,
  patch, sandbox apply/check, command, artifact validation, and repair services.
- `AgentRun` is the only product facade for Planner, Worker, Tester,
  FinalReview, Synthesizer, and PlannerDecision execution.
- Partitioned stores write engine metadata, results, events, artifacts, blobs,
  ledgers, contexts, tool results, live runs, and cache data.
- Legacy engines based on `WorkflowRunner` remain compatibility-only.
