# Capability Boundary Matrix

This matrix defines which Coder surface owns each capability, which permission
controls it, and what evidence should be produced.

| Capability | Owner | Permission | Boundary | Evidence |
| --- | --- | --- | --- | --- |
| Planner Chat | `planner-conversation` harness | read-only | No file writes, commands, network, secrets, commits, pushes, or deploys; remains available during Start Work | session records, revision, readiness state, provider token trace |
| Active-run conversation control | Planner API local router | none | Status, cancel, pure confirmation, and queued guidance do not call the Planner model | local-control session record, run control/guidance event |
| Workflow control | `workflow-planner` harness | read-only | Invoked only after verifier failure/blocking; chooses continue or external-state blocked from existing evidence | workflow decision events, final report inputs |
| Repo search/read | native tools | `read_files` | Path normalization, bounded reads, max result counts | repo evidence refs |
| Git status/diff | native tools | `read_files` | Read-only git inspection | repo evidence refs |
| Command preview | native tools | `run_commands` | Command shape can be inspected without execution | tool event |
| Command run | native tools | `run_commands` | Approval and sandbox policy before execution | command evidence, stdout/stderr refs, exit code |
| Background command | native tools | `run_commands` | Long command returns task id, output is tailed and cancellable | background-task refs and output refs |
| Exact text edit | native model tool | `write_files` | Existing UTF-8 file only; exact `old_string`; unique match unless `replace_all=true`; repo-relative and sensitive-path checks | file-write event and repo evidence ref |
| Tool hook | shared model-tool pipeline | underlying `run_commands`/`network` policy; prompt and agent hooks use provider settings | Command, webhook, prompt, and isolated read-only agent hooks run from the immutable run config snapshot; async rewake is agent-scoped | hook phase events, async hook status, rewake notification/delivery events |
| Patch preview | native tools | `write_files` | Patch is parsed and bounded before apply | patch preview evidence |
| Patch apply | native tools | `write_files` | Approval before mutation when policy is `ask` | patch apply evidence and changeset |
| Subagent | native subagent runtime | `child_harness_permissions` | Child inherits only allowed tools and scoped permissions | sidechain transcript, task status |
| Skill | native skill tool | selected tool permissions | Skill modifiers can grant scoped next-turn read/command/model/effort context, never overriding deny | skill invocation event and modifier attachment |
| Browser verification | `browser-verification` harness | read-only plus verifier command runtime | Uses Coder-owned verifier runtime paths | verifier result and browser evidence |
| Provider call | provider runtime | `secrets`, provider settings | API key redaction, proxy isolation, response caps | provider status/test trace |
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
- verifier result

Raw provider or tool payloads must be redacted before persistence. Large values
should be stored as blobs with bounded previews.

## Timeline Projection

The public timeline should show:

- planner readiness and Start Work boundary
- executor tool calls and outcomes
- command progress and background handoff
- file changes and changesets
- verifier checks and repair signals
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
