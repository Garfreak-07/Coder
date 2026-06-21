# Coding Kernel

The Coding Kernel owns repository intelligence and controlled local effects.

Current services:

- `ContextService`: builds `ContextPacketV2`, selected skill context, coding
  context packets, and token ledger entries.
- `PatchService`: validates proposed changes, guards risk paths, creates patch
  previews, applies approved patches, and rolls back snapshots.
- `CommandService`: validates cwd/scope, enforces approval for product checks,
  runs sandbox/local checks, and captures output.
- `ArtifactRepairService`: central one-shot JSON artifact repair for
  Planner/Worker/Tester paths.

In v0.9.2 these services sit behind `ActionGateway`:

```text
AgentGraphRunner / AgentEngine
-> ActionSpec
-> ActionGateway
-> BudgetBroker reservation
-> ContextService / PatchService / CommandService / ArtifactRepairService
```

Agent Engines receive prepared context and return artifacts. Patch/check effects
remain behind runtime services and are no longer direct Runner calls.

`TokenLedger` is still the audit record after context construction.
`BudgetBroker` is the pre-execution control path and writes reservation
diagnostics into run data.

## Coding Auto-Loop

The controlled coding loop is:

```text
execution_result.proposed_changes
-> ActionGateway propose_patch
-> patch_preview hidden effect
-> ActionGateway apply_patch_sandbox when sandbox_root is available
-> ActionGateway run_command_sandbox
-> DebugFinding on failed check
-> PlannerInputBundle.effects
-> PlannerDecision continue/replan or ask_human
```

Risk paths such as `.env`, `.git`, and `.coder` are blocked at patch preview
time and converted into Planner interrupts. Sandbox actions use `sandbox_root`
when provided; otherwise they are marked `sandbox_unavailable` and fall back to
the repo-root compatibility path with normal approval behavior.

## v0.9.2 Boundary

- Ordinary users see coding capabilities through Agents and workflow edges, not
  kernel services.
- `RunController` owns loop control when coding work asks for another round.
- `BudgetBroker` reserves context, command, patch, and model resources before
  execution.
- `ActionGateway` fronts all kernel service access from product runtime paths.
- `AgentEngineRegistry` owns Planner, Worker, Tester, FinalReview, and
  Synthesizer execution.
- Partitioned stores separate patch previews, command output blobs, token
  ledgers, trace spans, artifacts, and run events.
- Legacy `plan_artifact`, `patch_artifact`, and `review_artifact` remain only
  for old `WorkflowSpec` flows.
