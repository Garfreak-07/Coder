# Coder Architecture

Coder is an ordinary-user-first, Planner-led AgentGraph system for coding work.
The product surface is Agents, workflows, plugins, skills, and Planner
conversation. Runtime internals are compiled from those choices.

## Layers

1. Runtime Kernel: `RunController`, `RunGuard`, AgentGraph scheduling,
   dependency waves, cache, artifacts, context packets, budget reservations,
   trace spans, permissions, replay, and diagnostics.
2. Agent Layer: Agent identity, ordinary role card, workflow position, purpose,
   performance history, and compiled runtime profile.
3. Extensions: plugins, skills, and AgentEngine packages routed per work item.

## Human Channel

Only Planner can talk to the user. Workers, Testers, Final Testers, and other
non-Planner Agents return structured artifacts or blockers to Planner.

## Runtime Flow

```text
User goal -> Planner -> RunContract / PlannerOrder.plan_graph
RunController -> AgentGraphRunner -> AgentGraphScheduler
ActionGateway -> BudgetBroker -> ContextService
AgentRun -> AgentEngineRegistry -> AgentEngine -> artifact
PlannerInputBundle -> PlannerDecision -> RunController
```

Legacy `WorkflowSpec` remains only as a compatibility and advanced preview
boundary. Product live Agent workflows use `AgentGraphRunner`.

## v0.9.1 Control Plane

`RunController` owns global continuation decisions after each
`PlannerDecision`. It enforces max rounds and plan fingerprint loop guards
before another Planner round can start.

`ActionGateway` is the entry point for low-level runtime actions:

- context construction
- patch preview
- sandbox/local command checks
- artifact validation and repair

`BudgetBroker` reserves model, tool, and context budgets before those actions
run. `TokenLedger` remains the diagnostic/audit record after context is built.

Run events carry trace fields in their payloads:

```text
trace_id
span_id
parent_span_id
```

The first partitioned store façade keeps the existing `.coder` layout but gives
the code explicit event, artifact, blob, ledger, extension, and cache stores.
