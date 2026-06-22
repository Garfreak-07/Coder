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

## v0.9.6 Control Plane

`RunController` owns global continuation decisions after each
`PlannerDecision`. It enforces max rounds and plan fingerprint loop guards
before another Planner round can start, and writes explicit diagnostics into
run result data.

`ActionGateway` is the entry point for low-level runtime actions:

- context construction
- plugin operation dispatch
- MCP operation dispatch
- repo intelligence construction
- patch preview
- sandbox patch apply
- sandbox/local command checks
- artifact validation and repair

v0.9.6 closes declared runtime actions and capability enforcement.
`ActionGateway` handlers cover every
`ActionSpec` action type in the product runtime. Skills continue to load through
`ContextService` / `build_context`; direct `load_skill` is intentionally not a
public runtime action. Plugin and MCP operations merge caller-provided risk with
registry `ToolCapability`; approval-gated or unknown operations are blocked
before runtime execution unless explicitly approved.

`BudgetBroker` reserves model, tool, and context budgets before those actions
run. Reservation diagnostics are written alongside `TokenLedger`, which remains
the audit record after context is built.

`AgentRun` is the product Agent execution facade. It dispatches PlannerOrder,
Worker, Tester, Final Review, Synthesizer, and PlannerDecision work through
`AgentEngineRegistry`. `AgentGraphExecutor` remains only as a compatibility
adapter for older call sites and must not be constructed by the product
`AgentGraphRunner`.

Coding work follows the controlled auto-loop path:

```text
proposed_changes -> patch_preview -> sandbox_apply/check_result -> DebugFinding
-> PlannerInputBundle -> PlannerDecision -> next RunController decision
```

Patch preview, sandbox apply, sandbox check, requested runtime actions, and
DebugFinding records carry structured artifact refs in
`PlannerInputBundle.effects`, so Planner replan prompts can cite raw
failed-check or tool output instead of summaries only.

`WaveExecutor` owns worker wave concurrency. `AgentGraphRunner` prepares task
contexts and handles outcomes, but does not own `ThreadPoolExecutor` details.

Action events use helper-built envelopes for `action.started` and
`action.completed` / `action.blocked` / `action.failed`; hidden effects also
store the same completion payload in each effect record.

Run events carry trace fields in their payloads:

```text
trace_id
span_id
parent_span_id
```

Partitioned run stores keep the existing `.coder` layout while providing the
explicit metadata, result, event, artifact, blob, ledger, context, tool-result,
live-run, extension, and cache write path. `RunStore.save()` orchestrates these
stores instead of owning primary object file writes directly.

Legacy `WorkflowSpec` code remains compatibility-only and is not exposed by the
product AgentWorkflow API. `/api/v2/agent-workflows/default` returns only the
AgentWorkflow contract, and `/api/v2/agent-workflows/compile` returns
`410 Gone`. `/api/v2/live-runs` is deprecated in favor of
`/api/v2/live-agent-runs` for product AgentGraph execution. Live AgentGraph
creation returns `live-agent-runs` event and result URLs. Legacy
`/api/v2/live-runs/{run_id}` and `/events` return `410 Gone` for AgentGraph run
ids with migration URLs instead of returning AgentGraph payloads.
