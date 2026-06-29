# CodeWorker Harness Tool Loop

The CodeWorker tool loop is an optional provider-neutral execution harness for
bounded executor work. It is enabled with:

```powershell
CODER_ENABLE_CODE_WORKER_TOOL_LOOP=1
```

When the flag is off, `CodeWorkerHarness` keeps the single-shot
`execution_result` path.

## Authority Model

Coder remains Planner-led. The Planner owns global continue, finish, and human
interaction decisions. CodeWorker receives one `WorkItem`, performs scoped
runtime actions, and returns only an `execution_result`.

The model proposes actions. Runtime validates and executes actions. Runtime
records observations, evidence refs, changed files, patch refs, command checks,
and lifecycle records. Final artifacts are enriched from those runtime facts;
model-provided claims are not trusted.

## Loop Shape

Each turn accepts exactly one JSON object:

- `harness_action`
- `harness_action_batch`
- `execution_result`

`planner_decision`, `final_report`, `ask_human`, and human-message fields are
permission-boundary violations for CodeWorker.

The high-level loop is:

```text
prepare bounded prompt context
call model
parse JSON
validate action or candidate final
gate action through ToolGate
execute allowed side effects through ActionGateway
budget and externalize large results
record HarnessObservation and action lifecycle
continue until StopGate accepts execution_result or returns blocked
```

## Allowed Actions

The CodeWorker tool surface is deny-by-default:

- `read_file`
- `search_files`
- `inspect_git_diff`
- `propose_patch`
- `apply_patch_sandbox`
- `run_command_sandbox`
- `read_tool_output`
- `return_execution_result`

`run_command`, publishing, deployment, plugin installation, MCP enablement,
secret reads, direct memory writes, and human prompts are denied.

Tool metadata defines each action's phase, read-only status, concurrency safety,
interrupt behavior, and risk level. Read/search/diff/read-output actions may be
batched concurrently. Patch, command, and finalization actions are exclusive.

## Context And Result Budgeting

`CodeWorkerContextPreprocessor` creates a prompt projection without mutating the
durable `HarnessSession`.

Prompt context is split into:

- hot context: current work item, constraints, transition, recent observations,
  changed files, patch refs, latest command check
- warm context: opened files, search patterns, summarized older observations,
  recent recovery attempts, compact coding context
- cold context: evidence refs, output refs, omitted counts

Large tool payloads are converted to previews plus `sha256:<digest>` refs before
they are shown to the model. Full content is stored through the pending blob/tool
result replacement path and can be read explicitly through `read_tool_output`.

## Stop Gate And Recovery

An `execution_result` from the model is a candidate final artifact. `StopGate`
accepts, retries, or blocks it.

Recoverable examples:

- completed result without runtime evidence or no-op rationale
- missing verification
- completed result that ignores a failed command or patch
- unsupported changed file or patch ref claims
- completed result after a patch without runtime-backed changed files or patch
  refs

Unrecoverable examples:

- wrong work item, merge index, or agent id
- planner-only artifact output
- human prompt attempt
- permission boundary or risk path violation

`RecoveryPolicy` limits retries:

- invalid JSON or action schema: one retry
- command, patch, and stop-gate failures: two retries
- permission boundary: no retry

## Patch And Command Workflows

Patch workflow:

- patch actions are scoped and risk checked before execution
- a failed patch forces a `read_file` or `search_files` before another patch
- successful `apply_patch_sandbox` automatically records an `inspect_git_diff`
  observation
- changed files and patch refs come from runtime session evidence

Command workflow:

- only sandboxed commands are available to CodeWorker
- high-risk and interactive commands are blocked
- command failures produce failed verification checks
- a later passing command can recover the final verification status
- command output is preview/ref only

## Lifecycle Records

Each model action records lifecycle states such as:

```text
requested -> allowed -> executing -> ok|failed|blocked -> recorded
```

Cancelled actions record `cancelled -> recorded`. Actions skipped after a failed
exclusive sibling record `skipped -> recorded`.

Lifecycle summaries are surfaced on `requested_actions[*].lifecycle_statuses`
without embedding large payloads.

## Streaming Preparation

`StreamingActionExecutor` is an internal provider-neutral abstraction for future
streaming adapters. It can start concurrency-safe actions early, hold exclusive
actions until prior work is drained, emit observations in stable order, and
discard pending work with synthetic observations. The default tool loop remains
non-streaming.

## Validation Commands

Run from the repository root:

```powershell
cargo test --workspace
cd frontend
npm.cmd run test
npm.cmd run build
cd ..
git diff --check
```
