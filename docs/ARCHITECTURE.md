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
  -> Workflow Planner decision
       -> finish
       or bounded executor improvement / repair
       or blocked
  -> Review Changes and final report
```

Planner Chat is side-effect free. A session stores its selected repository and
receives only the `planner-conversation` harness's frozen read-only tool
snapshot. The default snapshot exposes bounded file listing, text search, line
range reads, and git status from the shared `coder-tools::catalog`; tool schemas
are sent structurally and are not repeated in prompt prose. The loop allows at
most two tool turns, four calls, 6,000 bytes per result, and 12,000 bytes of
aggregate observations before tools are removed and a final response is
required. Confirmation-only turns reuse the typed plan and do not re-expose
repository tools. `TurnContext` keeps Start Work unauthorized and preserves deny policy
for writes, commands, network, secrets, subagents, and publishing even when a
model invents approval fields or undeclared tool calls. Compact provider traces
record tool turns, calls, and returned bytes without persisting raw observations.
Planner Chat remains available while Start Work runs. Session revisions ensure
workflow completion cannot overwrite
newer chat turns or a newly prepared plan. Its structured plan requires every
material requested or inferred goal/scope behavior to map to an observable
acceptance criterion. Normal and prompt-overflow recovery use the same output
contract. Paths, acceptance criteria, and risks stated explicitly in the
current user request, including English and Chinese marker forms, remain
authoritative; model output may add detail but cannot silently replace them.
Provider transport, authentication, quota, or rate-limit failures are stored as
a visible blocked Planner turn with the redacted provider reason. They never
fall back to a locally marked-ready plan or start execution.

Planner Chat is not inferred from agent or harness names. The config entry
`surface_bindings.planner_chat` explicitly selects its agent and harness. The
Workflow Planner remains a separate node in the workflow graph and uses the
`workflow_decision` output contract. Active-run control uses typed `chat`,
`user_input`, `status`, and `interrupt` operations rather than phrase matching.

Start Work authorization is structural, not prompt prose:
`plan_context.start_work_authorized=true` is the execution gate. The Executor
task is the single sanitized domain objective. `plan_context.plan_draft` carries
only supplemental execution constraints such as scope, steps, affected paths,
acceptance criteria, assumptions, non-goals, and risks; it does not copy the
task or Planner Chat history.

The Workflow Planner is a separate read-only internal agent. It receives the
original goal, selected plan criteria, compact Executor evidence, the current
round budget, whether the Executor produced evidence, a current-round Executor
evidence summary capped at 1,000 characters, and at most three prior
 improvement directions. The evidence summary contains compact checks, blockers,
and changed-file names; it resets at each round boundary and never copies the
Executor transcript. Closed objective tasks take a deterministic `finish` fast
path after successful verification. Failure/blocking and plans explicitly marked
with `review_mode: qualitative` use the provider-backed Planner. Its default
output budget is 900 tokens, and `agent.runtime.max_output_tokens` is the
effective configured value within the shared 256..64,000 validation bounds.
`continue` is accepted only for one to three concrete improvements with medium
or high expected gain, and only when the expected quality gain clearly
outweighs another execution and verification round. Meeting the acceptance
criteria ends the loop even when optional enhancements remain.

Rust owns the hard stop policy: default maximum three rounds, no continuation
on the final round, no repeated direction, and no second refinement after a
round without Executor evidence. The native Executor stops on completion or
cancelation. An explicit `max_turns` deployment override is accepted; otherwise
the host derives 24 turns for normal-output models or 16 for high-output
reasoning models. A workflow may override the shared `token_budget`; otherwise
the host derives `(context_window + 2 * max_output) * max_rounds`, clamped to
64,000..2,000,000. Provider output plus non-cached input is charged across
Executor, Workflow Planner, subagents, model hooks, and transcript compaction.
Cache reads are not charged again. A completed Executor
result with check evidence becomes `finish` when a stop gate fires; an unresolved
Executor failure becomes
`blocked` rather than being falsely reported as complete. Provider failure is
not a successful stop gate: when a qualitative or failed-verification route
requires the live Workflow Planner, an unavailable or malformed decision
returns `blocked` with the provider reason even if basic smoke checks passed.

Execution happens through the `native-code-edit` harness by default. That
harness uses the `native-rust` backend and Coder-owned tool implementations.
The executor never talks directly to the user and must cite tool evidence for
claims.

When Start Work reaches the Rust API path, the server injects the model-driven
`native-model-tool-loop` behind the `native-rust` backend contract. It offers an
OpenAI-compatible tool-call loop for repo file listing,
text search, file reads, git status/diff, shared command/background output
tools, skill/subagent dispatch, repo-scoped atomic multi-file patches, and
finish signals. Registered stdio MCP tools are discovered once before the first
provider turn and added to the same frozen model-tool snapshot. The
provider-visible write surface advertises `apply_patch`;
legacy exact-edit and whole-file-write responses remain accepted only for
compatibility with already-running sessions.
Rust executes every tool call and returns observations to the model. If the
provider returns no tool calls, its text is summary-only. Read-only tasks may
finish without changed files. Missing credentials produce an explicit blocked
result. `DeterministicNativeBackend` is available only to explicit mock mode and
tests; it is not a production fallback.

Typed `execution_mode: read_only` is a host boundary, not prompt advice. It
reduces the Executor snapshot to repo listing/search/read, git status/diff, and
`finish`, and caps the loop at eight provider turns. Command, write, Skill,
subagent, and MCP schemas are absent. Tool evidence is recursively redacted before
durable storage, while the authorized in-memory read result still reaches the
model; documentation that names variables such as `LLM_API_KEY` therefore does
not fail merely because the evidence store rejects secret-like text.

Provider requests follow Codex's structural separation: available tools are
sent once as tool schemas, not repeated as JSON in user prose. Harness context
keeps one runtime copy of selected tools, raw child-inheritance policy, and
memory access; derived permission context contains only the decision contract
consumed by tool events.

`coder-tools::catalog` is the single built-in tool definition source for
canonical names, aliases, permissions, concurrency classes, and provider-visible
schemas. Planner plans carry typed `execution_mode` and `review_mode` fields;
workflow control never derives those decisions from task-language keywords.

`coder-extensions::StdioMcpRuntime` owns persistent local MCP connections. A
registration clears the child environment, forwards only the core process
environment plus explicitly named variables, uses 30 seconds for startup and
tool discovery by default, and uses 300 seconds for a tool call. Model-visible
names follow the Codex `mcp__server__tool` convention, permit only ASCII
alphanumeric characters and underscores, stay within 64 bytes, and use a stable
12-hex hash suffix for sanitization collisions or long names. Raw server and
tool names remain separate for protocol routing. The shared model-tool executor
supplies the Start Work approval and immutable route; model arguments supply
only the MCP input schema. Read-only annotations permit concurrent calls, while
unannotated or side-effecting tools remain exclusive. Results use the same hook,
event, redaction, large-output, and evidence pipeline as built-in tools. The
current boundary is stdio only; remote HTTP/OAuth MCP transport requires a
separate demonstrated product need.

## Crates

- `coder-cli`: `coder-rust` commands for doctor, config validation, workflow
  preview/run, run inspection, repo tools, and server startup.
- `coder-config`: project config model, Claude-style permission rules,
  runtime parameter validation, agent tool resolution, and workflow validation.
- `coder-core`: core identifiers and run/report types.
- `coder-events`: event records, large-payload refs, and secret redaction.
- `coder-extensions`: local plugin/skill discovery plus persistent stdio MCP
  connections and tool discovery.
- `coder-harness`: harness-facing contracts.
- `coder-memory`: project memory and knowledge retrieval baselines.
- `coder-server`: Axum API v3, Planner Chat, provider settings, run surfaces,
  native tool endpoints, hooks, background commands, subagents, skills, cache,
  and product UI API projection.
- `coder-store`: append-only local store, blobs, artifacts, checkpoints,
  changesets, repo evidence, cache accounting, compaction state, and goal
  state. Durable DTOs and errors live in `models.rs`.
- `coder-tools`: repo/file/git/command/patch tool implementations. The shared
  command lifecycle is isolated in `command_process.rs`; tool schemas live in
  `catalog.rs`, the atomic model patch parser and transaction live in
  `inline_patch.rs`, and request/evidence DTOs live in `models.rs`.
- `coder-workflow`: workflow graph runner, native backend, context budgeting,
  context compaction, provider streaming, subagent runtime, model-tool loop, and
  final reports.

`coder-server/src/lib.rs` and `coder-workflow/src/lib.rs` are module wiring
layers. Behavior should live in focused modules and be re-exported only when it
is part of the crate contract.

## Config Model

The default config lives in `examples/coder.yaml`.

Important defaults:

- `planner`: user-facing Planner Chat.
- `workflow-planner`: internal Start Work control loop.
- `executor`: native coding executor.
- `planner-conversation`: read-only planner harness.
- `workflow-planner`: read-only control harness.
- `native-code-edit`: native executor harness.

Model capability and Agent policy have separate ownership:

- `ModelCapabilities` owns context window, maximum output, effective-context
  percentage, auto-compact limit, streaming, tool-call, and parallel-tool-call
  support. DeepSeek chat resolves to 128,000 context and 8,000 output;
  DeepSeek reasoner resolves to 128,000 and 64,000; the generic fallback is
  200,000 and 64,000.
- Effective context defaults to Codex's 95%. Auto-compaction is capped at 90%
  of the full context window, so DeepSeek compacts at 115,200 tokens.
- `max_output_tokens`: optional Agent strategy override, clamped to the model
  capability.
- `max_turns`: optional positive deployment override. When omitted, the
  model-aware cost policy supplies 24 or 16 host-enforced provider turns.
- `effort`: `low`, `medium`, `high`, `xhigh`, or `max`. Planner Chat,
  Workflow Planner, Executor, and configured subagents carry it into provider
  requests; DeepSeek maps presence to thinking enabled, while generic
  OpenAI-compatible requests use `reasoning_effort` (`max` becomes `xhigh`).
- `compact_output_reserve_tokens`: optional Agent strategy override, clamped to
  the resolved model output bound.
- `max_output_recovery_attempts`: default 3.
- `max_consecutive_compaction_failures`: default 3.

Provider network policy owns transport parameters independently of Agent
strategy:

- `request_max_retries`: default 4, capped at 100.
- `stream_max_retries`: default 5, capped at 100. It is retained as provider
  capability state; Chat Completions streams are not replayed until continuation
  state can prevent duplicate generation.
- `stream_idle_timeout_ms`: default 300,000 and reset between Planner SSE chunks.
- `websocket_connect_timeout_ms`: default 15,000. It is inactive until a provider
  wire protocol actually selects WebSocket.
- `supports_websockets`: provider capability, not permission to force a transport.

`outbound_http::HttpClientFactory` resolves environment/explicit/direct routes,
`NO_PROXY`, and custom trust roots once for provider, SSE, and webhook clients.
`CODER_CA_CERTIFICATE` takes precedence over `SSL_CERT_FILE`. Route diagnostics
never include proxy credentials. The local frontend API accepts only loopback
binds and local/Tauri origins; remote binding requires a future authenticated
transport.

Workflow policy has two independent bounds:

- `max_rounds`: default 3, range 1 to 20.
- `token_budget`: optional explicit override. Otherwise the model-aware formula
  above supplies the run threshold; default DeepSeek chat with three rounds is
  432,000. `run.started.cost_policy` records the value and `budget_source`.

These runtime limits are independent of review topology and keep provider turns,
recovery, compaction, and transcript reads bounded.

Command `sandbox` input is not an operating-system sandbox in the current
runtime, so it never lowers approval requirements. `TurnContext` replaces
model-supplied `approved` and `sandbox` values with host state before policy
evaluation. Commands are bounded and permission/approval checked, but direct
socket access is not yet isolated. Platform sandbox and managed network enforcement
must be added together before Coder can claim Codex-equivalent command network
isolation. Model-generated foreground and background commands do not inherit
Coder's provider API-key environment variables, matching the credential
ownership side of Codex's broker without pretending to provide its WFP/socket
enforcement.

All command surfaces use `coder-tools::CommandProcessHandle` for spawn, output,
timeout, stdin, cancellation, and process-tree termination. The CLI,
deterministic workflow backend, and command hooks call the blocking
`run_command` adapter; the server holds the same handle while it projects
durable task records, evidence, and events. Running processes use independent
UUIDs and share one atomic 64-process product limit. Codex-aligned model bounds
are a 10-second initial yield, a blocking 5-second default status poll, a
300-second maximum poll, and a 1 MiB retained-output cap. Output reads accept a
monotonic byte cursor and report dropped-output gaps. Commands started with
`interactive=true` accept `write_stdin` and explicit stdin closure.
Cancellation and timeout target the Unix process group or the Windows process
tree before falling back to the direct child.

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
- `workflow_verification.rs`: completion evidence gate and repair signal handling.

Native turns, the model-tool HTTP surface, and subagents all call the same
`ServerModelToolExecutor`. That `ToolRuntime` owns schema routing, immutable tool
selection, permissions, hooks, safe/exclusive concurrency, ordered results,
evidence, and asynchronous attachments. `TurnContext` freezes run/repo identity,
agent/model capability, tools, permission policy, Start Work authorization, and
budget for one turn. Cancellation remains live workflow/task state so a signal
can interrupt an in-flight provider or tool future instead of being frozen into
the context snapshot.

The API runner wraps the workflow registry with
`coder-server/src/native_model_backend.rs` for `native-code-edit` executor
nodes and `coder-server/src/workflow_planner_backend.rs` for bounded quality
decisions. The executor wrapper enforces:

- `plan_context.start_work_authorized == true` before writes.
- configured provider credentials and base URL before model-driven edits.
- provider tool-call turns use the resolved Agent/model cost policy; `finish`
  terminates immediately, explicit `max_turns` wins, and the default host bound
  is visible in runtime metadata.
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
  permissions, metadata refs, transcript refs, explicit
  `read_subagent_status` polling, and `cancel_subagent_background`. Native
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
  with transcript evidence instead of being reported as still running.
  Cancellation is idempotent across races: if a task reaches a terminal state
  before the cancel request executes, Coder returns a completed no-op with
  `cancelled=false` and the terminal status. A successful explicit cancel is a
  non-error tool result even though its task status is `cancelled`.
- a configured tool-turn limit that blocks empty/no-op loops, but completes
  with an explicit check when useful repo-scoped writes already exist.
- changed-file evidence is carried across workflow repair rounds so a later
  verification-only Executor pass is not misreported as a no-op.
- one structured tool protocol for every side effect; plain model text cannot
  bypass tool permissions or evidence capture.
- repo-relative file paths only. The canonical model write tool accepts the
  Codex Begin/Add/Delete/Update/Move patch grammar in a `{ patch }` function
  argument, validates every target and hunk before mutation, then commits or
  rolls back the complete multi-file transaction.
- one `patch.applied` event and one repo evidence record containing every path
  changed by an atomic model patch. Review Changes derives the resulting Git
  diff independently from the run baseline.
- change evidence is produced by the tool that applied the change. Coder does
  not reread or persist the repository's whole pre-existing working-tree diff.
  This follows Codex's `TurnDiffTracker`, which tracks exact committed
  `apply_patch` deltas for the current turn without rereading the workspace.

The model-tool loop supports ordered result posting, duplicate tool-call
protection, synthetic errors for missing results, and aggregate result budget
storage. Tools are permission checked before execution and record evidence after
execution.

OpenAI-compatible `finish_reason=length|max_tokens` responses are treated as
provider output truncation rather than generic malformed JSON. The Executor
uses `max_output_recovery_attempts` (default 3), asks the model to resume with
smaller atomic `apply_patch` calls, records each recovery, and blocks when
recovery is exhausted. Coder does not generically escalate the output cap
because providers such as DeepSeek do not share one output-limit contract.

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

Provider `environment` routing follows a Codex-style three-state decision:
explicit proxy URL, explicit direct bypass from `NO_PROXY`, or transport default
when neither applies. The last state preserves Windows/PAC/system discovery;
it is not collapsed into direct access. DeepSeek's default `direct` mode always
uses `no_proxy` and remains isolated from those ambient settings.

The Planner provider path supports OpenAI-compatible streaming, fallback to JSON
when streaming fails, 2MiB response/pending-line caps, and bounded max-output
recovery. Each provider turn records
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

`ModelCapabilities` owns model context and output limits;
`coder-workflow/src/context_budget.rs` consumes the resolved values. DeepSeek's
128k context has 121,600 effective context and compacts at 115,200 tokens. A
configured lower limit is honored; a higher value is clamped to the same 90%
ceiling. The blocking limit remains the full model context window.

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
tokens versus a 180,000-token default autocompact threshold. Coder therefore
preserves cacheable history at the current measured scale and adds pruning only
when profiling demonstrates a real need.

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

Coder does not ship domain-specific verifier backends. Following Codex, the
Executor chooses repository-appropriate verification through structured command,
Skill, subagent, or MCP tools and records the resulting evidence. The Workflow
Planner consumes that evidence without embedding task-specific test logic.

## API Surface

The API is mounted under `/api/v3`. Main groups:

- health, capabilities, role cards, and built-in tool endpoints
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
default and exposes only controls supported by the current runtime.

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
