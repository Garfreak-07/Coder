# Coding Kernel

The Coding Kernel owns repository intelligence and controlled local effects.

Current services:

- `ContextService`: builds `ContextPacketV2`, selected skill context,
  coding context packets, and token ledger entries.
- `PatchService`: validates proposed changes, guards risk paths, creates patch
  previews, applies approved patches, and rolls back snapshots.
- `CommandService`: validates cwd/scope, enforces approval for product checks,
  runs sandbox/local checks, and captures output.
- `ArtifactRepairService`: central one-shot JSON artifact repair for
  Planner/Worker/Tester paths.

In v0.9.1 these services sit behind `ActionGateway`:

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
`BudgetBroker` is the pre-execution control path.

## v0.9.1 Boundary

- Ordinary users see coding capabilities through Agents and workflow edges, not
  kernel services.
- `RunController` owns loop control when coding work asks for another round.
- `BudgetBroker` reserves context, command, patch, and model resources before
  execution.
- `ActionGateway` fronts all kernel service access from product runtime paths.
- Partitioned stores separate patch previews, command output blobs, token
  ledgers, and run events.
- Legacy `plan_artifact`, `patch_artifact`, and `review_artifact` remain only
  for old `WorkflowSpec` flows.
