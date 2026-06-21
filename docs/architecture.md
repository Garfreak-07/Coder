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

## v0.9.2 Control Plane

`RunController` owns global continuation decisions after each
`PlannerDecision`. It enforces max rounds and plan fingerprint loop guards
before another Planner round can start, and writes explicit diagnostics into
run result data.

`ActionGateway` is the entry point for low-level runtime actions:

- context construction
- patch preview
- sandbox patch apply
- sandbox/local command checks
- artifact validation and repair

`BudgetBroker` reserves model, tool, and context budgets before those actions
run. Reservation diagnostics are written alongside `TokenLedger`, which remains
the audit record after context is built.

`AgentEngineRegistry` owns agent execution. It registers Planner, Code Worker,
Tester, Final Review, and Synthesizer engines. `AgentGraphExecutor` remains as
a compatibility adapter for older call sites and must not own prompt, repair, or
artifact-specific execution logic.

Coding work follows the controlled auto-loop path:

```text
proposed_changes -> patch preview -> sandbox apply/check -> DebugFinding
-> PlannerInputBundle -> PlannerDecision -> next RunController decision
```

Run events carry trace fields in their payloads:

```text
trace_id
span_id
parent_span_id
```

Partitioned run stores keep the existing `.coder` layout while providing the
explicit event, artifact, blob, ledger, extension, and cache write path.

Legacy `WorkflowSpec` endpoints remain compatibility-only. Preview compilers
return `runtime_type=legacy_preview`, and `/api/v2/live-runs` is deprecated in
favor of `/api/v2/live-agent-runs` for product AgentGraph execution.
