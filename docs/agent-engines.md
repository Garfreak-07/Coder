# Agent Engines

Agents do not call tools directly. Runtime compiles each Agent into a runtime
profile and dispatches work through `AgentRun` and `AgentEngineRegistry`.

Current default worker path:

```text
AgentGraphExecutor.create_execution_result
-> AgentRun
-> RuntimeProfileCache / RuntimeProfileCompiler
-> AgentEngineRegistry
-> CodeWorkerEngine
-> CodeWorkerHarness
```

Agent engines receive prepared envelopes. They should not call
`ContextService`, `PatchService`, `CommandService`, or repair services directly.
New low-level work enters through `ActionGateway`, which reserves budget through
`BudgetBroker` first.

`AgentEngineSpec` and `HarnessGraph` define installable engine structure without
exposing it in ordinary UI.

`HarnessValidator` enforces core boundaries:

- context builder, artifact validator, and output artifact are required
- loops require max steps
- worker and tester engines cannot ask the human
- tester engines cannot write files
- external effects require preview metadata
- plugin operations require permission metadata

Model calls inside the default AgentGraph executor reserve model budget before
invocation when a real model is configured. Mock-mode execution does not consume
model-call budget.

## v0.9.1 Boundary

- Ordinary users choose Agents; engine graphs remain hidden runtime internals.
- `RunController` controls whether engine output can lead to another round.
- `BudgetBroker` gates model calls and low-level engine actions.
- `ActionGateway` is the approved bridge from engine/runtime intent to context,
  patch, command, and repair services.
- Partitioned stores separate engine events, artifacts, blobs, ledgers, and
  cache data.
- Legacy engines based on `WorkflowRunner` remain compatibility-only.
