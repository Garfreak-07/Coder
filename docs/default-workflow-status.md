# Default Workflow Status

The default workflow is moving to a three-agent default that hides low-level settings from regular users.

## Target product workflow

```text
Codex Planner
  -> DeepSeek CC Executor
  -> DeepSeek CC Tester
  -> Codex Planner
       -> done when checks pass
       -> retry executor when failures are actionable
       -> block when risk/scope is unacceptable
```

Regular users should only provide:

- project folder
- natural-language request

The system infers:

- target scope
- allowed paths
- check command
- max iterations
- local A2A routing

## What works now

The default visible workflow spec and A2A events now use:

- `codex_planner`
- `cc_executor`
- `cc_tester`

The Web UI no longer exposes scope/check/approval/max-iteration fields. `/api/run` infers those settings automatically and includes the inferred settings in the run output for debugging.

The current concrete runtime is still:

```text
intake -> scan_repo -> module_map -> codex_planner -> approval(auto-approved for dry-run) -> execute(dry-run) -> check -> codex_planner
```

This means the workflow can complete a full safe run from the UI without asking users to understand internal settings.

## Current limitation

`execute_node` is still deliberately dry-run. It records that execution was approved but does not modify source files.

The DeepSeek-backed Claude Code executor/tester are represented as Agent Cards and A2A events, but they are not yet separate provider-routed runtime sessions.

Patch application should wait until these pieces exist:

1. Snapshot before mutation.
2. Exact allowed-path enforcement.
3. Patch preview and human confirmation.
4. Deterministic patch application.
5. Check command execution after patch.
6. Reviewer gate before final status.
7. Rollback support.

## Next implementation target

The next useful step is provider-routed scoped execution:

```text
Codex Planner produces instructions and target files
  -> DeepSeek CC Executor proposes/applies a scoped diff
  -> DeepSeek CC Tester runs inferred checks
  -> Codex Planner approves, retries, or blocks
```

Do not expose low-level scope/check/MCP/A2A configuration to regular users. Keep those as inferred runtime details and advanced debug output.
