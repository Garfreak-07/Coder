# Architecture

Coder is a native Rust control plane with a React product UI. The core product
decision is simple: Planner owns user conversation, Start Work owns execution,
and Coder-owned harnesses enforce tools, permissions, cache policy, evidence,
and verification.

## Product Loop

```text
Planner Chat
  -> readiness state
  -> Start Work
  -> native-code-edit executor
  -> verifier
  -> Workflow Planner decision
       -> finish
       or bounded executor improvement / repair
       or blocked
  -> Review Changes and final report
```

Planner Chat is side-effect free. It can read bounded context and produce
readiness, but it must not mutate the repo. Planner Chat remains available while
Start Work runs. Session revisions ensure workflow completion cannot overwrite
newer chat turns or a newly prepared plan. Its structured plan requires every
material requested or inferred goal/scope behavior to map to an observable
acceptance criterion. Normal and prompt-overflow recovery use the same output
contract. Paths, acceptance criteria, and risks stated explicitly in the
current user request, including English and Chinese marker forms, remain
authoritative; model output may add detail but cannot silently replace them.
Provider transport, authentication, quota, or rate-limit failures are stored as
a visible blocked Planner turn with the redacted provider reason. They never
fall back to a locally marked-ready plan or start execution.

Start Work authorization is structural, not prompt prose:
`plan_context.start_work_authorized=true` is the execution gate. The Executor
task and plan goal contain the sanitized domain objective without duplicated
"Start Work was clicked" text.

The Workflow Planner is a separate read-only internal agent. It receives the
original goal, selected plan criteria, compact verifier evidence, the current
round budget, whether the Executor produced evidence, a current-round Executor
evidence summary capped at 1,000 characters, and at most three prior
improvement directions. The evidence summary contains compact checks, blockers,
and changed-file names; it resets at each round boundary and never copies the
Executor transcript. Closed objective tasks take a deterministic `finish` fast
path after successful verification. Failure/blocking and explicit open-ended
quality goals use a provider-backed Planner with a 900-token output cap.
`continue` is accepted only for one to three concrete improvements with medium
or high expected gain, and only when the expected quality gain clearly
outweighs another execution and verification round. Meeting the acceptance
criteria ends the loop even when optional enhancements remain.

Rust owns the hard stop policy: default maximum three rounds, no continuation
on the final round, no repeated direction, and no second refinement after a
round without Executor evidence. The native Executor also has a 24-turn
fallback when configuration omits `max_turns`, so a custom non-interactive
workflow cannot silently become unbounded. A workflow may additionally set an
optional shared `token_budget`; provider output plus non-cached input is
charged across Executor, Workflow Planner, subagents, model hooks, and
transcript compaction. Cache reads are not charged again. A completed verifier result becomes
`finish` when a stop gate fires; an unresolved verifier failure becomes
`blocked` rather than being falsely reported as complete. Provider failure is
not a successful stop gate: when a qualitative or failed-verification route
requires the live Workflow Planner, an unavailable or malformed decision
returns `blocked` with the provider reason even if basic smoke checks passed.

Execution happens through the `native-code-edit` harness by default. That
harness uses the `native-rust` backend and Coder-owned tool implementations.
The executor never talks directly to the user and must cite tool evidence for
claims.

When Start Work reaches the Rust API path with provider credentials configured,
the server injects a model-driven native executor implementation named
`native-model-file-write` behind the same `native-rust` backend contract. It
first offers a bounded OpenAI-compatible tool-call loop for repo file listing,
text search, file reads, git status/diff, shared command/background output
tools, skill/subagent dispatch, repo-scoped exact text edits/full writes, and
finish signals.
Rust executes every tool call and returns observations to the model. If the
provider returns no tool calls, Coder falls back to a strict JSON file plan and
still applies only repo-scoped writes through `coder-tools`. The deterministic
`NativeRustBackend` remains the read-only, mock-mode, and missing-credential
fallback.

## Crates

- `coder-cli`: `coder-rust` commands for doctor, config validation, workflow
  preview/run, run inspection, repo tools, and server startup.
- `coder-config`: project config model, Claude-style permission rules,
  runtime parameter validation, agent tool resolution, and workflow validation.
- `coder-core`: core identifiers and run/report types.
- `coder-events`: event records, large-payload refs, and secret redaction.
- `coder-extensions`: local plugin/skill discovery and registry helpers.
- `coder-harness`: harness-facing contracts.
- `coder-memory`: project memory and knowledge retrieval baselines.
- `coder-server`: Axum API v3, Planner Chat, provider settings, run surfaces,
  native tool endpoints, hooks, background commands, subagents, skills, cache,
  and product UI API projection.
- `coder-store`: append-only local store, blobs, artifacts, checkpoints,
  changesets, repo evidence, cache accounting, compaction state, and goal
  state.
- `coder-tools`: repo/file/git/command/patch tool implementations.
- `coder-workflow`: workflow graph runner, native backend, browser verifier,
  context budgeting, context compaction, provider streaming, subagent runtime,
  model-tool loop, and final reports.

`coder-server/src/lib.rs` and `coder-workflow/src/lib.rs` are module wiring
layers. Behavior should live in focused modules and be re-exported only when it
is part of the crate contract.

## Config Model

The default config lives in `examples/coder.yaml`.

Important defaults:

- `planner`: user-facing Planner Chat.
- `workflow-planner`: internal Start Work control loop.
- `executor`: native coding executor.
- `verifier`: read-only verification agent.
- `planner-conversation`: read-only planner harness.
- `workflow-planner`: read-only control harness.
- `native-code-edit`: native executor harness.
- `browser-verification`: browser/gameplay verifier harness.

Agent runtime policy is explicit and validated:

- `max_output_tokens`: 256 to 64,000.
- `max_turns`: optional positive configuration value, matching Claude Code's
  optional `maxTurns` loop bound. Coder's non-interactive native runtime uses
  24 when the value is omitted.
- `effort`: `low`, `medium`, `high`, `xhigh`, or `max`. Planner Chat,
  Workflow Planner, Executor, and configured subagents carry it into provider
  requests; DeepSeek maps presence to thinking enabled, while generic
  OpenAI-compatible requests use `reasoning_effort` (`max` becomes `xhigh`).
- `context_window_tokens`: default 200,000, range 32,000 to 1,000,000.
- `compact_output_reserve_tokens`: default 20,000.
- `autocompact_buffer_tokens`: default 13,000, with larger dynamic buffers for
  400k and 800k context windows.
- `max_output_recovery_attempts`: default 3.
- `max_consecutive_compaction_failures`: default 3.
- `stream_idle_timeout_ms`: default 90,000. Planner Chat resets this timeout
  for each SSE chunk; non-streaming agent requests use it as their response
  deadline.

Workflow policy has two independent bounds:

- `max_rounds`: default 3, range 1 to 20.
- `token_budget`: optional positive integer with no invented default. It is a
  stop threshold at provider-turn boundaries, not a target to consume.

These values intentionally mirror the useful Claude Code behavior where it is
measurable: bounded recovery attempts, explicit effort levels, large-context
buffering, and bounded transcript/cache reads.

## Native Executor

Native execution is split into small modules:

- `native_backend.rs`: backend selection, public truncation, and executor
  summaries.
- `tool_execution.rs`: tool concurrency classes and execution shape.
- `model_tool_loop.rs`: assistant tool-use blocks to ordered tool results.
- `subagent_context.rs` and `subagent_runtime.rs`: child harness context,
  sidechain state, and background subagent output.
- `workflow_runner_core.rs`: round transitions, status decisions, and final
  report flow.
- `workflow_backend_execution.rs`: backend invocation and evidence plumbing.
- `workflow_verification.rs`: verifier handoff and repair signal handling.

The API runner wraps the workflow registry with
`coder-server/src/native_model_backend.rs` for `native-code-edit` executor
nodes and `coder-server/src/workflow_planner_backend.rs` for bounded quality
decisions. The executor wrapper enforces:

- `plan_context.start_work_authorized == true` before writes.
- configured provider credentials and base URL before model-driven edits.
- provider tool-call turns use the agent's optional `max_turns`; `finish`
  terminates immediately, and the default Executor is explicitly capped at 24.
- repo/git read-only provider tool calls routed through the shared model-tool
  pipeline, preserving permission checks, hooks, result caps, evidence refs, and
  post-compact restore candidates.
- command/background tool calls routed through the shared model-tool pipeline,
  preserving permission checks, hooks, defaults, evidence, and background task
  persistence.
- Prompt hooks and isolated agent hooks use the configured provider through the
  same run snapshot. Agent hooks receive a read-only minimal tool set and must
  return through `StructuredOutput`. Async rewake command hooks persist an
  exit-code-2 notification and attach it to the next matching provider turn.
- Webhook hooks are native Coder tool hooks. External hook URLs must use
  `https://`; `http://` is accepted only for loopback local development such as
  `localhost`, `127.0.0.1`, or `::1`. Config accepts the clearer
  `type: webhook`, `allowedWebhookUrls`, and `webhookAllowedEnvVars` names.
- Skill and subagent provider tool calls routed through the shared model-tool
  pipeline, preserving skill context, Agent(type) rules, child harness
  permissions, metadata refs, transcript refs, and Claude-style `TaskOutput`
  polling plus `TaskStop` cancellation for background subagents. Native
  subagents inherit backend context for model/runtime/tool selection, keep
  separate child ids/transcripts, recover the parent run's Start Work
  `plan_context` when a direct model-tool request omits `backend_context`, and
  can complete no-file review tasks without inheriting the parent executor's
  file-write requirement. When a provider tool call returns a structured
  subagent final report, the parent native outcome promotes only
  `changed_files` and `evidence_refs` into the parent final report and Review
  Changes surface; child events remain isolated in the child transcript.
- Background subagent task records are durable. After a server restart,
  completed tasks recover status/report/event previews from metadata and
  transcript sidecars. Running tasks with no live registry are marked `lost`
  with transcript evidence instead of being reported as still running. TaskStop
  is idempotent across races: if a task reaches a terminal state before the
  stop executes, Coder returns a completed no-op with `cancelled=false` and the
  terminal status.
- a configured tool-turn limit that blocks empty/no-op loops, but completes
  with an explicit check when useful repo-scoped writes already exist.
- changed-file evidence is carried across workflow repair rounds so a later
  verification-only Executor pass is not misreported as a no-op.
- strict JSON fallback output with bounded file count and file size.
- repo-relative file paths only, using `coder-tools::edit_text_file` for
  localized exact replacements and `coder-tools::write_text_file` for new or
  deliberate whole-file writes.
- `file.written` and repo evidence refs for every applied file.
- git diff evidence when the target repo can provide one.

The model-tool loop supports ordered result posting, duplicate tool-call
protection, synthetic errors for missing results, and aggregate result budget
storage. Tools are permission checked before execution and record evidence after
execution.

OpenAI-compatible `finish_reason=length|max_tokens` responses are treated as
provider output truncation rather than generic malformed JSON. The Executor
uses `max_output_recovery_attempts` (default 3), asks the model to resume with
a small exact edit or smaller single-file writes, records each recovery, and
blocks when recovery is exhausted. Coder does not generically escalate to
Claude's 64k output cap
because providers such as DeepSeek do not share Anthropic's output limits.

The core provider observe/act loop has real DeepSeek pressure for
repo/git/write/command tools, background commands, prompt and isolated agent
hooks, async rewake next-turn delivery, model-chosen subagents, and an
open-ended browser game. That game run predates the bounded success-path
Workflow Planner and remains a baseline rather than evidence for the new loop.

## Permissions

Permissions are configured at harness level:

- `read_files`
- `write_files`
- `run_commands`
- `child_harness_permissions`
- `network`
- `secrets`
- `publish_external`
- `git_commit`
- `git_push`
- `deploy`

`coder-config/src/permissions.rs` implements rule evaluation and persistence
updates. `coder-server/src/model_tool_permissions.rs` applies the active
runtime policy before a model-facing tool call executes. Deny rules override
temporary grants. Agent-specific rules such as `Agent(type)` are preserved so a
single subagent type can be denied without disabling the entire subagent tool.

## Provider Runtime

Provider Settings are the normal user path. Environment variables are fallback
for headless development and tests.

The provider runtime resolves:

- default provider and model
- provider-specific API keys
- base URLs
- proxy mode: `direct`, `explicit`, or `environment`
- provider test request and redacted error reporting

DeepSeek and Ollama default to direct networking. Other providers default to
environment proxy mode because they are more likely to require a developer
proxy. Provider Settings resolve Coder's symbolic model aliases (`best`,
`standard`, `economy`) to the selected provider/model, but preserve explicit
role-specific model ids for hook or specialist agents. Provider errors are
redacted before they reach API responses.

The Planner provider path supports OpenAI-compatible streaming, fallback to JSON
when streaming fails, 2MiB response/pending-line caps, and max-output recovery
using the same default retry count as Claude Code. Each provider turn records
transport, fallback, estimated input/output tokens, reported input/output/total
tokens, cache-read tokens, and provider request count. Pure confirmations and
active-run status/cancel/guidance messages have no provider trace because they
are handled locally.

The Workflow Planner uses one short OpenAI-compatible JSON request when the
quality router selects it. Its output is capped at 900 tokens and persisted as
`planner.workflow_decision`, including improvements, expected gain, stop reason,
round budget, and provider token usage.

For explicit qualitative goals, the Planner prompt does not treat a generic
smoke-test pass as sufficient quality evidence. It may request one focused,
task-specific review or playtest when that evidence is absent. Rust still owns
the stop decision when the final round, repetition, no-progress, or per-agent
turn gate fires. Refinement tasks carry only the Planner feedback and directly
related checks; they explicitly stop broad replanning and unrelated rewrites.

## Context And Memory

`coder-workflow/src/context_budget.rs` owns context thresholds. The default
200k context with a 20k summary reserve and 13k autocompact buffer yields an
effective 180k working window and a 167k autocompact threshold.

`coder-workflow/src/context_compaction.rs` performs deterministic JSON
compaction for plan context. It records:

- compaction contract
- thresholds
- original and compacted token estimates
- projected max turn growth
- circuit breaker state

`coder-server/src/run_transcript_compaction.rs` handles run transcript
compaction and post-compact restoration of bounded file/skill context.

Prompt-cache-aware cache edits, microcompaction of individual tool results, and
additional collapse strategies should be added only when profiling proves the
native loop needs them. The current DeepSeek peak was about 32,000 request
tokens versus a 167,000-token default autocompact threshold. Claude's
time-based old-result clearing is disabled by default and assumes an expired
cache; OpenCode protects 40,000 recent tool-output tokens and requires more
than 20,000 tokens of actual savings before pruning. Coder therefore preserves
the cacheable history at the current measured scale.

## Store And Cache

The local store defaults to `.coder` under the workspace. Durable state includes
sessions, runs, artifacts, blobs, settings, checkpoints, changesets, background
tasks, and logs. Disposable cache includes repo indexes, plugin cache, skill
cache, and `tmp`.

Important bounds:

- Durable file reads are capped at 50MiB.
- JSONL page/tail reads are capped at 1000 records.
- Cache usage scans are capped at 1000 filesystem entries per bucket.
- Provider responses and pending streaming lines are capped at 2MiB.

The browser verifier resolves Node/Playwright from Coder-owned runtime paths or
explicit overrides. It must not install large verification dependencies into a
target user project just because verification is enabled.

Browser intent uses word-boundary matching so terms such as `package.json` and
`functions` do not accidentally select page/game checks. Static verification
reads both external scripts and inline single-file applications. Dynamic
verification starts common button or overlay entry controls, exercises input,
checks visible progress, and reports console/page errors before a generic
progress failure so Planner repair receives the actionable source location.

## API Surface

The API is mounted under `/api/v3`. Main groups:

- health, capabilities, role cards, and harness tools
- workflows and workflow library
- planner sessions and Start Work
- provider settings, provider status, and provider test
- run detail, events, timeline, artifacts, blobs, evidence, checkpoints, and
  reports
- repo, command, patch, model-tool, skill, subagent, memory, extension, cache,
  and review/undo endpoints

Active Start Work runs and generic workflow runs with a caller-supplied
`run_id` register a real watch control channel. Pause/resume reject inactive
runs; cancellation drops the active provider/tool future, while a stale durable
running record may still be terminalized as cancelled.

Run details should be fetched with bounded pages/tails. Full transcript hydration
is not the default UI path.

## Frontend

The React app uses API v3 directly. The ordinary first screen is Planner Chat.
The ordinary product sidebar starts at Planner Chat. Workflow graph editing,
cache status, and developer surfaces are advanced tools, not the main user path.
Plugins & Skills belongs behind the explicit debug UI switch
`?debug=1` or `localStorage.coder_debug_ui = "1"`.

Frontend workflow generation must emit `native-code-edit` for execution by
default and should not expose removed runtime knobs.

## Verification

Minimum verification for architecture changes:

```sh
cargo fmt --all --check
cargo check --workspace
cargo test -p coder-config
cargo test -p coder-workflow
cargo test -p coder-server
```

Frontend changes should run:

```sh
cd frontend
npm run test
npm run build
```

Live model validation is required before claiming provider/runtime parity.
