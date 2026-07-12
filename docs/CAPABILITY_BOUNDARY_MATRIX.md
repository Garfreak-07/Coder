# Capability Boundary Matrix

This matrix defines which Coder surface owns each capability, which permission
controls it, and what evidence should be produced.

| Capability | Owner | Permission | Boundary | Evidence |
| --- | --- | --- | --- | --- |
| Planner Chat | `planner-conversation` harness | `read_files` | Bound repository plus frozen file-list/search/range-read/git-status snapshot; 2 tool turns, 4 calls, 6,000 bytes per result, 12,000 bytes aggregate; confirmation turns reuse the typed plan without tools; no writes, model commands, network, secrets, subagents, commits, pushes, or deploys; remains available during Start Work | session records, revision, readiness state, provider token/tool trace |
| Active-run conversation control | Planner API local router | none | Status, cancel, pure confirmation, and queued guidance do not call the Planner model | local-control session record, run control/guidance event |
| Workflow control | `workflow-planner` harness | read-only | Reads Executor action/check evidence; chooses finish, continue, or external-state blocked | workflow decision events, final report inputs |
| Repo search/read | native tools | `read_files` | Path normalization, bounded reads, max result counts; model receives the authorized read while durable evidence is recursively redacted | repo evidence refs |
| Git status/diff | native tools | `read_files` | Read-only git inspection | repo evidence refs |
| Command preview | native tools | `run_commands` | Command shape can be inspected without execution | tool event |
| Command run | native tools | `run_commands` | Host-owned permission and approval checks; 10-second initial model yield, 5-second default poll, 300-second maximum poll, global 64-process limit, independent process UUIDs, and 1 MiB retained output. CLI, hooks, deterministic workflow tools, and foreground/background model calls share one lifecycle implementation. Model commands do not inherit provider API-key environment variables. Current `sandbox` input never lowers approval and does not prove network denial. | command evidence, stdout/stderr refs, exit code |
| Background command | native tools | `run_commands` | Long command returns a task id; reads support monotonic output cursors and gap reporting; `interactive=true` enables `write_stdin`; cancellation and timeout terminate the process tree | background-task refs and output refs |
| Atomic model patch | native model tool | `write_files` | Canonical provider-visible write tool; Codex Begin/Add/Delete/Update/Move grammar in one bounded `{ patch }` argument; all paths and hunks are prepared before mutation; add/update/delete/move commit as one transaction and prior contents are restored on commit I/O failure; repo-relative and sensitive-path checks; host approval cannot be supplied by the model | one `patch.applied` event, one repo evidence record with every affected path, and Review Changes diff |
| Exact text edit | native model tool | `write_files` | Existing UTF-8 file only; exact `old_string`; unique match unless `replace_all=true`; repo-relative and sensitive-path checks | file-write event and repo evidence ref |
| Tool hook | shared model-tool pipeline | underlying `run_commands`/`network` policy; prompt and agent hooks use provider settings | Command, webhook, prompt, and isolated read-only agent hooks run from the immutable run config snapshot; async rewake is agent-scoped | hook phase events, async hook status, rewake notification/delivery events |
| Patch-file preview | native API/CLI tools | `write_files` | A repository patch file is parsed and bounded before apply | patch preview evidence |
| Patch-file apply | native API/CLI tools | `write_files` | Separate user/API patch-file boundary; approval before mutation when policy is `ask` | patch apply evidence and changeset |
| Subagent | native subagent runtime | `child_harness_permissions` | Child inherits only allowed tools and scoped permissions | sidechain transcript, task status |
| Skill | native skill tool | selected tool permissions | Skill modifiers can grant scoped next-turn read/command/model/effort context, never overriding deny | skill invocation event and modifier attachment |
| MCP tool | persistent stdio MCP runtime plus shared model-tool pipeline | Start Work approval | Explicit registration starts and initializes the child process, discovers tools once, freezes a normal Executor run snapshot, preserves raw server/tool identities behind stable `mcp__server__tool` names, and never accepts model-supplied approval or routing identity. Typed read-only tasks expose no MCP schema. | server/tool discovery, approval, started/completed/failed events, bounded output or blob evidence |
| Provider call | provider runtime | `secrets`, provider settings | Provider-owned retry/idle parameters, API key redaction, shared direct/proxy/`NO_PROXY` route, custom CA policy, response caps | provider status/test trace |
| Run pause/resume/cancel | active run watch channel | run ownership | Signals only active in-process runs; inactive pause/resume conflicts, cancellation drops the active future | run control event and terminal report |
| Memory write | Planner-owned memory APIs | planner/write scope | Long-term writes are proposed/confirmed, not executor-owned | memory proposal/confirmation records |
| Publish/commit/push/deploy | none in default executor | `publish_external`, `git_commit`, `git_push`, `deploy` | Denied by default | blocked permission event |

## Permission Rules

Permission decisions are `allow`, `ask`, or `deny`. Deny always wins. Temporary
session grants from skill modifiers, hooks, or approvals are scoped and must not
expand beyond the active harness tool pool.

`Agent(type)` rules are content-aware. Denying one subagent type does not deny
the whole subagent tool unless the broader tool rule also denies it.

## Evidence Rules

An executor summary is valid only when it points to evidence:

- command id, exit code, and bounded output
- patch preview/apply evidence
- repo evidence ref
- blob/artifact/checkpoint ref
- background task status
- subagent sidechain status
- executor check result

Raw provider or tool payloads must be redacted before persistence. Large values
should be stored as blobs with bounded previews.

## Timeline Projection

The public timeline should show:

- planner readiness and Start Work boundary
- executor tool calls and outcomes
- command progress and background handoff
- file changes and changesets
- executor checks and repair signals
- final report status

Planner provider traces and Executor provider events retain numeric token usage
and cache-read metrics. Secret-like string values remain redacted; numeric
metrics must not be removed by secret redaction.

It should not expose secrets, raw provider request bodies, or unbounded
transcripts.

## Default Harness Posture

`native-code-edit` defaults to:

- read files: allow
- write files: ask
- run commands: ask
- child harness permissions: ask
- network: ask
- secrets: ask
- publish external: deny
- git commit: deny
- git push: deny
- deploy: deny

This keeps the executor useful while requiring an explicit boundary crossing for
side effects.
