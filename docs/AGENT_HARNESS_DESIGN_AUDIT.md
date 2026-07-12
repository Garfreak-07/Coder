# Agent Harness Design Audit

This audit records the current design and its evidence. It is not a change log.

## First-Principles Product Rules

1. User conversation and execution are different contexts. Planner Chat owns
   conversation and the approved plan; a workflow run receives a compact plan
   snapshot, tools, evidence, and execution feedback.
2. The normal path is `Planner -> Executor -> Workflow Planner`. The Executor
   owns implementation and checks; review is not a permanent workflow agent.
   Closed objective success finishes locally; failures and explicit qualitative
   goals use a bounded provider-backed finish-or-improve decision.
3. Status, cancellation, pure confirmation, and requirements attached to an
   active run are local control operations. They must not spend a Planner model
   turn.
4. Rust owns side effects, permissions, evidence, cancellation, storage, and
   resource limits. Provider output is a request to use tools, not authority to
   mutate the host.
5. Optional integrations stay outside the core until a real workflow requires
   them. Coder owns the ordinary execution runtime.

## Concrete Codex Runtime References

The local Codex Rust source at
`C:/Users/aixdl/Downloads/codex-main/codex-main` was inspected directly:

| Behavior | Codex source/value | Coder policy |
| --- | --- | --- |
| Stream idle watchdog | `model-provider-info/src/lib.rs`: provider-owned 300,000 ms; SSE applies timeout between events | Coder now owns this in provider network settings and resets it for each Planner SSE chunk. Agent runtime no longer owns a duplicate network timeout. Native non-streaming execution has a separate operation deadline. |
| Request retries | `model-provider-info/src/lib.rs` and `codex-client/src/retry.rs`: 4 request retries, 200 ms exponential backoff, 0.9-1.1 jitter, transport/5xx retry, and no 429 retry; streaming permits 5 reconnects; retry counts are capped at 100 | Coder resolves the same provider defaults and cap, and all model call sites use the provider-specific request count. External webhooks are excluded to avoid repeating side effects. Stream replay remains inactive because Chat Completions lacks Codex's persisted Responses continuation state. |
| WebSocket connect | `model-provider-info/src/lib.rs`: provider-owned 15,000 ms plus `supports_websockets`; `core/src/client.rs` keeps HTTP fallback state at session scope | Coder records the provider capability and timeout but does not select WebSocket while its live wire protocol is Chat Completions. A configuration flag alone does not create a transport. |
| Product outbound routes | `http-client/src/outbound_proxy.rs`: one `HttpClientFactory`, route classes, explicit direct/proxy decisions, redacted proxy diagnostics | Coder provider, SSE, and webhook clients share one route-aware factory and one environment/`NO_PROXY` resolver. DeepSeek remains direct by default. |
| Custom trust roots | `http-client/src/custom_ca.rs`: `CODEX_CA_CERTIFICATE` precedes `SSL_CERT_FILE`; every active HTTP/WebSocket constructor uses the shared policy | Coder applies the same precedence with its product prefix, `CODER_CA_CERTIFICATE`, then `SSL_CERT_FILE`, to every active external client. No WebSocket client exists yet. |
| Local control transport | `app-server-transport/src/transport/websocket.rs`: loopback by default; unauthenticated non-loopback listeners are rejected; browser Origin upgrades are rejected | Coder's HTTP frontend API now rejects non-loopback binds and allows CORS only for loopback and Tauri local origins. It does not expose an unauthenticated remote listener. |
| Agent command network | `sandboxing`, `network-proxy`, and `windows-sandbox-rs`: OS sandbox plus managed proxy and credential broker; Windows uses restricted identities, Firewall/WFP, proxy allowlists, and offline environment rewrites | Coder strips provider API keys from model command environments, forces model-supplied `sandbox` false, and keeps approval host-owned. It still has no OS socket isolation or host-bound credential injection. |
| Auto-compaction | `protocol/src/openai_models.rs`: 95% effective context and `min(config_limit, context_window * 9 / 10)` | Coder puts these values in `ModelCapabilities`; DeepSeek's 128k context has 121,600 effective context and a 115,200 compact threshold. |
| Tool output | `core/src/unified_exec/mod.rs`: 10,000 model-visible tokens and 1 MiB raw output | Coder limits model-visible tool results more tightly and uses the same 1 MiB raw command cap. |
| Background terminal wait | `core/src/unified_exec/mod.rs`: 300,000 ms | Coder's command lifecycle differs and must be aligned semantically rather than copying a foreground kill timeout. |
| Multi-agent concurrency | `core/src/config/mod.rs`: MultiAgentV2 defaults to 4 threads | This does not control same-turn tool concurrency. Coder will not reuse `4` for a different parameter. |
| Agent wait | `core/src/tools/handlers/multi_agents_spec.rs`: minimum 10s, default 30s, maximum 3600s | Retained as a reference for the subagent wait path; it is not a model request timeout. |
| Tool routing | `core/src/session/turn.rs::build_prompt` takes `ToolRouter::model_visible_specs()`; `core/src/client.rs::build_responses_request` serializes those specs separately from prompt input. Codex core contains no task-specific browser/game verifier. | Coder exposes general command, Skill, subagent, and MCP tools structurally. It does not maintain a domain verifier backend or repeat tool names in user prose. |
| MCP runtime | `codex-mcp/src/tools.rs` preserves raw identities while sanitizing, collision-hashing, and limiting model names to 64 bytes; `codex-mcp/src/rmcp_client.rs` and `rmcp-client/src/stdio_server_launcher.rs` keep initialized child-process clients; config defaults are 30 seconds for startup and 300 seconds per tool. | Coder uses `rmcp` 1.8 with persistent stdio children, the same 30/300-second defaults, isolated explicit environment forwarding, frozen Executor tool snapshots, stable `mcp__server__tool` names, and host-owned approval/routing through `ServerModelToolExecutor`. |
| Turn/tool ownership | `core/src/session/turn_context.rs`, `core/src/tools/router.rs`, and `core/src/tools/parallel.rs`: immutable turn configuration plus one router/runtime for permission, dispatch, concurrency, and results; cancellation remains live task state | Native Executor, model-tool API, and subagents use one `ServerModelToolExecutor`. `TurnContext` freezes identity, tools, permissions, model capability, approval, and budget while workflow control remains live. |
| Change attribution | `core/src/turn_diff_tracker.rs` tracks exact committed `apply_patch` deltas for the current turn without rereading the workspace; `core/src/tools/events.rs` updates it only from known deltas. | Coder persists evidence from the tool that applied each change. It does not collect the repository's whole pre-existing dirty diff after a read-only or write run. |
| Secret isolation and errors | `protocol/src/shell_environment.rs` excludes environment variable names matching `*KEY*`, `*SECRET*`, and `*TOKEN*`; `protocol/src/error.rs` retains bounded provider error text. | Coder redacts structured secret keys, explicit credential assignments, and key-shaped values. Ordinary diagnostic words such as `token` and `secret` remain visible so blocked runs explain their cause. |

Coder adopts a value only when the state and timeout boundary match. Similar
names are not sufficient evidence.

## Research Decision Matrix

| Source | Concrete evidence | Coder decision |
| --- | --- | --- |
| [Plan-and-Solve](https://arxiv.org/abs/2305.04091) | Planning before solving reduces missing-step and semantic errors. | Planner model output now populates the existing structured `PlanDraft`; no new planning agent was added. |
| [CodePlan](https://arxiv.org/abs/2309.12499) | Repository work benefits from dependency-aware, adaptive edit plans. | Planner distinguishes assumptions from repository facts and makes bounded inspection an execution step. It must not invent paths before inspection. |
| [Agentless](https://arxiv.org/abs/2407.01489) | A three-stage localization, repair, and validation pipeline outperformed more complex open-source agents at lower cost. | Keep execution and verification simple. A cheap intent gate avoids an extra model request for closed successful tasks. |
| [SWE-agent](https://arxiv.org/abs/2405.15793) | Agent-computer interface design materially changes solve rate. Its current `TemplateConfig` bounds one observation at 100,000 characters. | Improve tool result shape and references before adding reasoning layers. Coder keeps a smaller 24,000-character per-result model bound. |
| [Aider repo map](https://github.com/Aider-AI/aider/blob/main/aider/website/_posts/2023-10-22-repomap.md) | Tree-sitter symbols are graph-ranked into a repository map with a default 1,000-token budget. | Treat a small symbol map as a future localization experiment, not a default dependency. Existing selective search/read tools remain the baseline. |
| [RepoCoder](https://arxiv.org/abs/2303.12570) | Iterative retrieval and generation improves repository-level completion over one-shot retrieval. | Executor may inspect incrementally; Planner handoff carries intent and quality criteria, not copied source files. |
| [ReAct](https://arxiv.org/abs/2210.03629) and [Reflexion](https://arxiv.org/abs/2303.11366) | Interleaved evidence and action help; Reflexion iterates until evaluator success or a maximum trial count. | Preserve tool and check feedback in the Executor loop; let the Workflow Planner turn that evidence into one bounded repair/refinement direction. |
| [Self-Refine](https://arxiv.org/abs/2303.17651) | Feedback and refinement alternate until `stop(feedback, t)`; experiments commonly cap the number of steps. | Planner judges marginal gain, while Rust enforces round, repetition, and no-progress stop gates. |
| [LATS](https://arxiv.org/abs/2310.04406) | Search combines external feedback, value estimates, and reflection until success or computation budget exhaustion, with bounded depth. | Keep only the useful value/budget principle; do not add a search tree. `expected_gain` must be medium/high and `max_rounds` remains the hard depth bound. |
| [Scaling LLM Test-Time Compute Optimally](https://arxiv.org/abs/2408.03314) | Test-time compute is most useful when allocated adaptively by prompt difficulty; spending the maximum budget uniformly is inefficient. | A workflow budget is permission to continue, not a target. Closed verified work stops locally, while only unresolved criteria or a concrete medium/high-gain direction can spend another round. |
| Russell and Wefald, *Do the Right Thing: Studies in Limited Rationality* (1991) | Additional computation is rational only while its expected value exceeds its cost. | The Workflow Planner estimates gain, but deterministic Rust gates reject low-gain, repeated, no-progress, and over-budget continuation. This is a control heuristic, not a claim of calibrated utility. |
| [Semantic Early-Stopping for Iterative LLM Agent Loops](https://arxiv.org/abs/2606.27009) | On its HotpotQA study, a judge-free semantic stopper reduced operational tokens by 38% at parity quality, while a per-round quality judge cost more than it saved. | Do not add another judge or embedding service. Reuse the Workflow Planner only on routed quality work, and give it a bounded current-round evidence summary plus cheap repeated-direction and no-progress gates. |
| [Adaptive Stopping for Multi-Turn LLM Reasoning](https://arxiv.org/abs/2604.01413) | Conformal error allocation can provide coverage guarantees across adaptive turns, but it requires calibrated task data and prediction sets. | Do not claim statistical stopping guarantees without a representative Coder evaluation set. Keep deterministic safety limits now and treat calibrated adaptive budgets as a later measured experiment. |
| [CP-Agent](https://arxiv.org/abs/2605.24693) | Feedback-driven solving is modeled as a calibrated stopped process with false-admission risk, evidence against bad programs, and active-state success hazard under a finite controller manifest. | Executor check evidence remains mandatory for finish, controller outcomes stay finite (`finish`, `continue`, `blocked`), and an unavailable quality decision must not admit an unsupported result. |
| [DSPy](https://arxiv.org/abs/2310.03714) | Structured signatures and metric-driven optimization outperform hand-tuned prompt strings. | Keep runtime dependency-free; use a small offline evaluation set to tune Planner instructions and reject changes that do not improve measured outcomes. |
| [Gemini CLI compression](https://github.com/google-gemini/gemini-cli/blob/main/packages/core/src/context/chatCompressionService.ts) | Current defaults trigger at 50% of the context window, preserve the latest 30%, and reserve 50,000 tokens for recent function responses. | Use these as comparison points for the next Executor-history experiment. Do not copy them until the DeepSeek baseline shows the same pressure pattern. |
| [Aider history](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/coders/base_coder.py) | `summarize_start()` summarizes only `done_messages` when `too_big`, runs the summarizer in a background thread, and leaves current-turn messages separate. | Coder Executor runs are task-scoped and currently stay well below their context limit; do not add a background summarizer without a long-run overflow case. |
| [OpenCode compaction](https://github.com/anomalyco/opencode/blob/4a1982f5c951850a1820e7eb0c9ed4b4613a2912/packages/opencode/src/session/compaction.ts) | Pruning protects 40,000 recent tool-output tokens, requires more than 20,000 tokens of actual savings, protects two recent turns, and replaces old output with at most 2,000 characters. | Coder's measured request peak is about 32,000 total tokens, below OpenCode's protected old-tool budget alone. Keep the 24,000-character per-result cap and defer old-result clearing. |
| Codex Rust `apply-patch` local source | `codex-rs/apply-patch/src/lib.rs`, `core/src/tools/handlers/apply_patch_spec.rs`, and their tests accept multiple files and hunks in one `apply_patch` call, expose the Lark grammar, parse the complete change before mutation, and reject invalid patches. | Coder exposes the same grammar as the canonical provider-visible write tool through a `{ patch }` function argument for Chat Completions providers. All target contents are prepared before mutation, and commit I/O failure restores every affected path. The separate patch-file API/CLI boundary remains available for user-supplied patch files. |

## Codex Rust Communication Reference

The local source at
`C:/Users/aixdl/Downloads/codex-main/codex-main` was inspected directly:

- `codex-rs/protocol/src/protocol.rs` defines a typed
  `InterAgentCommunication` with author, recipients, content, and a
  `trigger_turn` flag.
- `codex-rs/core/src/session/input_queue.rs` stores typed messages in a
  `VecDeque`, uses `tokio::sync::watch` only as an activity signal, and drains
  queued communication at a turn boundary.
- `codex-rs/core/src/agent/control/spawn.rs` can fork no history, all history,
  or the last N turns. Fork filtering drops reasoning, tool calls, tool
  outputs, and intermediate agent messages instead of copying an entire raw
  transcript.
- `codex-rs/core/src/context_manager/history.rs` stores typed `ResponseItem`
  values, truncates function outputs as they are recorded, and consumes a
  cloned history snapshot in `for_prompt()` for normalization. A prompt-side
  history clone is therefore not itself evidence of a leak.
- `codex-rs/core/src/config/mod.rs` defaults MultiAgentV2 to four concurrent
  threads. Its wait defaults are 10,000 ms minimum, 30,000 ms normal, and a
  3,600,000 ms hard maximum.
- `codex-rs/core/src/rollout_budget.rs` shares one budget across a root session
  tree. It charges sampling tokens plus weighted non-cached prefill tokens,
  sends remaining-budget reminders per thread, and returns
  `SessionBudgetExceeded` at the hard limit. The config test demonstrates
  `100000` weighted tokens, reminder thresholds at `50000/25000/10000`, and
  weights `1.0` for output and `0.1` for prefill; those are test inputs, not
  universal product defaults.

Coder does not need Codex's full thread manager for its task-scoped subagents.
The adopted principle is smaller: keep runtime state typed or singly owned,
send concise summaries plus durable references across agent boundaries, and
serialize full records only at API or storage boundaries.

Codex passes selected reasoning effort explicitly through client request
construction instead of treating it as descriptive context. Coder now applies
the same ownership rule across Planner Chat, Workflow Planner, Executor, and
subagents. One shared mapping validates provider effort levels; DeepSeek uses
its thinking switch and generic compatible endpoints receive
`reasoning_effort`.

Coder now copies Codex's default accounting semantics without exposing its
uncalibrated tuning surface: sampling/output tokens and non-cached input tokens
both have weight `1.0`; cache-read tokens have zero incremental charge. The
workflow limit is derived from resolved model capacity and rounds unless the
workflow explicitly overrides it. Custom weights and reminder thresholds remain
absent until cross-provider evaluations justify them.

## Current Optimization Pass

- Planner provider responses can now supply a compact structured plan. Domain
  expectations, assumptions, trade-offs, and task-specific acceptance criteria
  reach the Executor through the existing `PlanDraft` instead of being lost in
  chat text.
- Explicit paths plus English or Chinese acceptance criteria and risks in the
  current user request are retained ahead of model suggestions. Generic
  deterministic fields may still be replaced, so this protection does not
  freeze stale scope from earlier turns.
- Start Work sends one plan snapshot plus the original request and authorization
  bit. Acceptance criteria, risks, affected paths, workflow ID, and the static
  conversation summary are not duplicated at the top level.
- Subagent runtime context contains one `coder` object with only the fields
  consumed by the child runtime.
- Model-visible subagent results contain status, report, task metadata, and
  durable metadata/transcript references. Full event previews remain available
  in the API payload and store but are not copied back into the model history.
- Completed and cancelled background commands/subagents leave the in-memory
  registry after their durable record is written. Background subagents are
  capped at three live child tasks: Codex's default of four session threads
  includes the root agent.
- Background work starts only from explicit command or subagent tool calls and
  follows the configured task timeout and ownership boundaries.
- Planner harnesses expose no execution tools. Workflow Planner reuses the
  default model reference and provider runtime.
- Provider-backed Workflow Planner decisions use strict JSON output and
  code-enforced stop gates. Successful closed tasks retain a zero-provider
  fast path; qualitative goals and Executor failures receive model analysis.
- A missing, failed, or malformed live Workflow Planner decision is `blocked`,
  not converted to `finish` by an earlier smoke-test pass. Low gain,
  repetition, no progress, and final-round exhaustion remain ordinary stop
  gates that may accept genuinely verified work.
- Planner Chat provider failures are also represented as a persisted blocked
  turn rather than HTTP 500 or a ready deterministic fallback. The current
  user request remains in the session, the provider status is visible, and no
  execution authorization is produced.
- Verification is selected by the Executor from general structured tools and
  repository conventions. Task-specific checks belong in commands, Skills, or
  MCP integrations rather than fixed Rust classifiers.
- `apply_patch` is the canonical provider-visible write tool. It supports
  Add/Delete/Update/Move operations across multiple files, validates all hunks
  before mutation, records one evidence chain for every affected path, and
  rolls back partial commit I/O failures. Legacy exact-edit and whole-file
  responses remain accepted only for compatibility with existing sessions.
- Registered stdio MCP tools join the normal Executor schema and shared
  model-tool pipeline. Tool discovery is frozen once per run, typed read-only
  tasks exclude MCP, and only read-only annotations permit parallel calls.
- Subagent cleanup records are emitted only for concrete task ownership or
  cancellation actions.

## Current Agent Boundaries

| Role | Context | Tools | When invoked |
| --- | --- | --- | --- |
| Planner Chat | Bounded conversation history plus current plan | No execution tools | User conversation, including while work is active |
| Executor | Compact approved plan, run events, tool results, queued guidance | Harness-scoped native tools | Start Work and repair rounds |
| Workflow Planner | Goal, plan criteria, Executor action/check evidence, round budget, current progress, and at most three prior directions | No execution tools | Every Executor outcome; provider call only for failure/blocking or open-ended quality intent |
| Subagent | Filtered child context and allowed tool set | Child-harness tools only | Explicit Executor/skill delegation |

Planner session revisions prevent workflow completion from overwriting newer
chat turns or a newly prepared plan. Active runs expose `work_in_progress`,
`active_run_id`, and `latest_run_id`. Status and cancellation are routed
locally. Supplementary requirements are persisted as queued guidance and
delivered at the next safe Executor turn. Active-run membership and guidance
enqueue are one atomic operation. Before finishing, the Workflow Planner gives
pending guidance priority over optional refinement: it continues when a round
remains and blocks at the hard round limit. Guidance that arrives after the
last safe Executor turn is recorded as `planner.user_guidance.unapplied` and is
reported in Planner Chat instead of being silently deleted.

## Token And Runtime Controls

- Planner pure confirmations are local and preserve the existing plan.
- Closed successful tasks do not spend a provider turn. Open-ended success may
  spend one 900-token Workflow Planner turn to decide whether a concrete
  medium/high-gain refinement is justified.
- Planner provider traces persist request count, estimated input/output,
  provider input/output/total tokens, cache-read tokens, transport, and
  fallback state.
- Retained DeepSeek Planner Chat sessions report roughly 982-1,061 input
  tokens on representative first turns and 1,159-1,604 on later parallel
  turns, with 256-640 cache-read tokens. This is well below the Executor cost
  and does not justify removing the strict contract or compacting fewer than
  the current ten recent turns without a regression benchmark.
- Executor provider turns emit the same token categories as run events.
- Full Planner history is not copied into execution; Start Work creates a
  compact plan handoff.
- Start Work authorization is represented once as
  `plan_context.start_work_authorized=true`. `request.task` is the single
  sanitized objective; the plan handoff carries only supplemental execution
  constraints and does not repeat the original request or goal.
- Tool availability is transmitted once through provider tool schemas. The
  user prompt does not serialize `selected_tools`, matching Codex's separate
  `Prompt.input` and `Prompt.tools` fields.
- Native execution has one side-effect protocol. Plain assistant text may end a
  read-only task, but file changes require structured tools and their shared
  permission/evidence pipeline; the former whole-file JSON plan path was
  removed.
- Backend context omits unconsumed memory summaries, model profile aliases, and
  duplicated permission policy/tool metadata. Raw harness policy remains only
  where child inheritance needs it; permission events receive a compact
  contract plus evaluated decisions.
- Provider tool turns stop immediately on `finish`. Explicit `max_turns` wins;
  otherwise the model-aware host policy supplies 24 turns for normal-output
  models or 16 for high-output reasoning models.
- Native Executor responses ending in `length` or `max_tokens` use the existing
  three-attempt output recovery policy. The retry prompt follows Claude's
  smaller-piece recovery instruction, while provider-specific 64k escalation
  is deliberately not applied to DeepSeek/OpenAI-compatible endpoints.
- Files changed in earlier repair rounds remain part of later Executor reports;
  a verification-only round is not treated as a no-op failure.
- The Workflow Planner receives at most 1,000 characters of current-round
  Executor checks, blockers, and changed-file evidence. The summary resets at
  the round boundary and never copies the Executor transcript.
- `max_rounds` defaults to 3. The final round cannot continue; repeated Planner
  directions and a second refinement without Executor evidence are stopped.
- `workflow.token_budget` is an optional run-level override. Otherwise
  `(context_window + 2 * max_output) * max_rounds`, clamped to
  64,000..2,000,000, supplies the visible default. It is shared
  by the root Executor, Workflow Planner, synchronous/background subagents,
  Prompt/Agent hooks, and transcript compaction. Each successful provider
  response charges `output + max(0, input - cache_read)`; providers without
  usage metadata use the existing request/response token estimate. A response
  already in flight may cross the threshold, matching Claude and Codex; no
  subsequent provider request is sent. Budget state is released only after the
  root run and its background subagents are inactive.
- Continuation uses a two-layer policy. Rust enforces the hard round/turn/output
  ceilings plus novelty and progress gates. Inside those limits, the Workflow
  Planner may continue only for an unmet acceptance criterion, newly queued
  user guidance, or 1-3 observable changes with medium/high expected gain.
  Optional ideas and generic polish never justify consuming the remaining
  budget. This makes every run an anytime process: the latest verified artifact
  is usable, and stopping does not depend on reaching a subjective ideal.
- A token budget is a ceiling, not a target to consume. The inspected Claude
  implementation can auto-continue below 90% of a requested turn budget and
  detects diminishing output after three continuations with two sub-500-token
  deltas. Coder deliberately does not copy that continuation rule: acceptance
  and marginal gain decide continuation, while round/turn/output limits remain
  hard backstops.
- Missing command runtime, credentials, permission, or network state is an
  external blocker and is not misreported as a code repair.

## Implemented Native Capabilities

- Provider-backed repo/search/read/git/write/command/background tool loop.
- Shared permission, hook, evidence, result-bounding, and redaction pipeline.
- Prompt, isolated agent, command, async rewake, and webhook hooks.
- Foreground-to-background command handoff, bounded output, polling, and
  explicit `read_command_output` / `cancel_command_background` control.
- Skills with scoped context modifiers and forked execution.
- Synchronous/background subagents with filtered tools, sidechain transcripts,
  durable status, explicit `read_subagent_status` /
  `cancel_subagent_background` control, and lost-handle recovery.
- Deterministic plan compaction, model-backed transcript compaction, bounded
  restoration attachments, and persistent failure circuits.
- Append-only run/session evidence, Review Changes, conservative undo, cache
  accounting, and bounded page/tail reads.
- Active model/tool cancellation using `tokio::sync::watch`; pause is observed
  at workflow boundaries.

## Completion Evidence

| Requirement | Evidence | Status |
| --- | --- | --- |
| Native lightweight core | Cargo workspace contains 11 native crates and the default execution path stays inside Coder-owned Rust harnesses. | Complete |
| Maintainable module layout | `coder-server/src/lib.rs` and `coder-workflow/src/lib.rs` are wiring layers; behavior lives in focused modules. | Complete |
| Parallel Planner and workflow | Deterministic concurrency/cancellation tests prove Planner Chat remains available while Start Work runs and completion does not overwrite newer turns. | Complete |
| Local status/supplement/stop control | Targeted server tests prove zero-provider routing and in-flight cancellation. | Complete |
| Low-token successful path | Pure confirmation is local; closed verified tasks use a zero-provider Planner fast path; open-ended quality review is capped at 900 output tokens. | Complete |
| Real provider path | Provider transport, native tools, hooks, compaction, subagent, Review Changes, and secret boundaries have deterministic coverage. A minimal live DeepSeek replay completed through Planner Chat, Start Work, native Executor tools, Workflow Planner, final report, and Review Changes; streaming stayed on the event-stream path and the secret scan passed. | Complete |
| Documentation | Maintained docs describe the current native path, architecture, provider setup, persistence, and resource policy. | Complete |

## Residual Risk

These are release-depth improvements, not missing core architecture:

- Measure prompt-cache behavior with providers beyond DeepSeek before adding
  cache-edit complexity.
- The native provider loop still clones and serializes its accumulated message
  vector on every turn, but this is a transient copy rather than a demonstrated
  leak. The measured request peak was about 32,000 tokens, and DeepSeek reused
  most of the prefix from cache. Claude's ordinary old-result clearing is
  disabled by default, Codex also clones a normalized prompt snapshot, and
  OpenCode protects 40,000 recent tool-output tokens before pruning. Do not
  trade cache reuse for compaction until a run approaches Coder's 180,000-token
  threshold or memory profiling attributes material retained bytes here.
- Planner Chat has no repository tools. This is intentional for the current
  side-effect-free boundary; large-repository planning should be evaluated
  before deciding whether a bounded read-only exploration step is justified.

## Maintenance Rule

Prefer deleting duplicated agents, model calls, adapters, scripts, and docs.
Add a component only when it owns a distinct state boundary or removes measured
complexity. Every retained runtime parameter must have a source, a validation
rule, and an observable event or test.
