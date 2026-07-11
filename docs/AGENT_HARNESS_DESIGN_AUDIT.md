# Agent Harness Design Audit

This audit records the current design and its evidence. It is not a change log.

## First-Principles Product Rules

1. User conversation and execution are different contexts. Planner Chat owns
   conversation and the approved plan; a workflow run receives a compact plan
   snapshot, tools, evidence, and verification feedback.
2. The normal path is `Planner -> Executor -> Verifier -> Workflow Planner`.
   Closed objective success finishes locally; failures and explicit qualitative
   goals use a bounded provider-backed finish-or-improve decision.
3. Status, cancellation, pure confirmation, and requirements attached to an
   active run are local control operations. They must not spend a Planner model
   turn.
4. Rust owns side effects, permissions, evidence, cancellation, storage, and
   resource limits. Provider output is a request to use tools, not authority to
   mutate the host.
5. Optional integrations stay outside the core until a real workflow requires
   them. The default product has no separate executor runtime.

## Concrete Claude Code References

The local Claude Code 2.8.1 source was inspected rather than inferred from
product behavior:

| Behavior | Claude Code source/value | Coder policy |
| --- | --- | --- |
| Auto-compaction buffer | `src/services/compact/autoCompact.ts`: 13,000 tokens | `autocompact_buffer_tokens: 13000` |
| Manual compaction buffer | `src/services/compact/autoCompact.ts`: 3,000 tokens | Manual compaction may use a smaller explicit override; it is not reused as the automatic threshold. |
| Compaction output reserve | `src/utils/context.ts`: 20,000 tokens | `compact_output_reserve_tokens: 20000` |
| Normal output cap | `src/query.ts`: 8,000 tokens | Executor default `max_output_tokens: 8000` |
| Escalated output cap | `src/query.ts`: 64,000 tokens | Validation maximum is 64,000; Coder does not select it by default. |
| Output recovery attempts | `src/query.ts`: 3 | `max_output_recovery_attempts: 3` |
| Tool-result message bound | `src/constants/toolLimits.ts`: 200,000 characters | Coder uses smaller per-result and aggregate limits, with blob refs for retained large values. |
| Agent turns | `src/query.ts`: optional `maxTurns`; the loop stops only when it is supplied and exceeded | `runtime.max_turns` remains optional configuration, but the non-interactive native runtime falls back to 24; `finish` stops immediately. |
| Stream idle watchdog | `src/services/api/claude.ts`: `CLAUDE_STREAM_IDLE_TIMEOUT_MS`, default 90,000 ms, reset after stream activity | Planner Chat applies `stream_idle_timeout_ms` to the initial response and every SSE chunk. Non-streaming Executor and Workflow Planner requests use it as the response deadline. Codex Rust independently uses `tokio::time::timeout(idle_timeout, stream.next())` in `codex-client/src/sse.rs`. |
| Workflow token budget | `packages/workflow-engine/src/engine/budget.ts`: optional `budgetTotal`; `assertCanSpend()` runs before each agent call and `addOutputTokens()` records completed output | `workflow.token_budget` is optional. One run-scoped counter is shared by Executor, Workflow Planner, subagents, model hooks, and transcript compaction. |
| Session lookup cache | `src/utils/sessionStorage.ts`: 200 entries | Planner session cache is capped at 200 entries. |
| Transcript file read | `src/utils/sessionStorage.ts`: 50 MiB | Durable file reads reject values above 50 MiB. |

Coder intentionally does not copy every Claude feature. It copies a behavior
only when it improves resource bounds, context quality, recovery, permissions,
or maintainability.

## Research Decision Matrix

| Source | Concrete evidence | Coder decision |
| --- | --- | --- |
| [Plan-and-Solve](https://arxiv.org/abs/2305.04091) | Planning before solving reduces missing-step and semantic errors. | Planner model output now populates the existing structured `PlanDraft`; no new planning agent was added. |
| [CodePlan](https://arxiv.org/abs/2309.12499) | Repository work benefits from dependency-aware, adaptive edit plans. | Planner distinguishes assumptions from repository facts and makes bounded inspection an execution step. It must not invent paths before inspection. |
| [Agentless](https://arxiv.org/abs/2407.01489) | A three-stage localization, repair, and validation pipeline outperformed more complex open-source agents at lower cost. | Keep execution and verification simple. A cheap intent gate avoids an extra model request for closed successful tasks. |
| [SWE-agent](https://arxiv.org/abs/2405.15793) | Agent-computer interface design materially changes solve rate. Its current `TemplateConfig` bounds one observation at 100,000 characters. | Improve tool result shape and references before adding reasoning layers. Coder keeps a smaller 24,000-character per-result model bound. |
| [Aider repo map](https://github.com/Aider-AI/aider/blob/main/aider/website/_posts/2023-10-22-repomap.md) | Tree-sitter symbols are graph-ranked into a repository map with a default 1,000-token budget. | Treat a small symbol map as a future localization experiment, not a default dependency. Existing selective search/read tools remain the baseline. |
| [RepoCoder](https://arxiv.org/abs/2303.12570) | Iterative retrieval and generation improves repository-level completion over one-shot retrieval. | Executor may inspect incrementally; Planner handoff carries intent and quality criteria, not copied source files. |
| [ReAct](https://arxiv.org/abs/2210.03629) and [Reflexion](https://arxiv.org/abs/2303.11366) | Interleaved evidence and action help; Reflexion iterates until evaluator success or a maximum trial count. | Preserve tool feedback in the Executor loop; let the Workflow Planner turn verifier evidence into one bounded repair/refinement direction. |
| [Self-Refine](https://arxiv.org/abs/2303.17651) | Feedback and refinement alternate until `stop(feedback, t)`; experiments commonly cap the number of steps. | Planner judges marginal gain, while Rust enforces round, repetition, and no-progress stop gates. |
| [LATS](https://arxiv.org/abs/2310.04406) | Search combines external feedback, value estimates, and reflection until success or computation budget exhaustion, with bounded depth. | Keep only the useful value/budget principle; do not add a search tree. `expected_gain` must be medium/high and `max_rounds` remains the hard depth bound. |
| [Scaling LLM Test-Time Compute Optimally](https://arxiv.org/abs/2408.03314) | Test-time compute is most useful when allocated adaptively by prompt difficulty; spending the maximum budget uniformly is inefficient. | A workflow budget is permission to continue, not a target. Closed verified work stops locally, while only unresolved criteria or a concrete medium/high-gain direction can spend another round. |
| Russell and Wefald, *Do the Right Thing: Studies in Limited Rationality* (1991) | Additional computation is rational only while its expected value exceeds its cost. | The Workflow Planner estimates gain, but deterministic Rust gates reject low-gain, repeated, no-progress, and over-budget continuation. This is a control heuristic, not a claim of calibrated utility. |
| [Semantic Early-Stopping for Iterative LLM Agent Loops](https://arxiv.org/abs/2606.27009) | On its HotpotQA study, a judge-free semantic stopper reduced operational tokens by 38% at parity quality, while a per-round quality judge cost more than it saved. | Do not add another judge or embedding service. Reuse the Workflow Planner only on routed quality work, and give it a bounded current-round evidence summary plus cheap repeated-direction and no-progress gates. |
| [Adaptive Stopping for Multi-Turn LLM Reasoning](https://arxiv.org/abs/2604.01413) | Conformal error allocation can provide coverage guarantees across adaptive turns, but it requires calibrated task data and prediction sets. | Do not claim statistical stopping guarantees without a representative Coder evaluation set. Keep deterministic safety limits now and treat calibrated adaptive budgets as a later measured experiment. |
| [CP-Agent](https://arxiv.org/abs/2605.24693) | Feedback-driven solving is modeled as a calibrated stopped process with false-admission risk, evidence against bad programs, and active-state success hazard under a finite controller manifest. | Verifier evidence remains mandatory for finish, controller outcomes stay finite (`finish`, `continue`, `blocked`), and an unavailable quality decision must not admit a smoke-test-only result. |
| [DSPy](https://arxiv.org/abs/2310.03714) | Structured signatures and metric-driven optimization outperform hand-tuned prompt strings. | Keep runtime dependency-free; use a small offline evaluation set to tune Planner instructions and reject changes that do not improve measured outcomes. |
| [Gemini CLI compression](https://github.com/google-gemini/gemini-cli/blob/main/packages/core/src/context/chatCompressionService.ts) | Current defaults trigger at 50% of the context window, preserve the latest 30%, and reserve 50,000 tokens for recent function responses. | Use these as comparison points for the next Executor-history experiment. Do not copy them until the DeepSeek baseline shows the same pressure pattern. |
| [Aider history](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/coders/base_coder.py) | `summarize_start()` summarizes only `done_messages` when `too_big`, runs the summarizer in a background thread, and leaves current-turn messages separate. | Coder Executor runs are task-scoped and currently stay well below their context limit; do not add a background summarizer without a long-run overflow case. |
| [OpenCode compaction](https://github.com/anomalyco/opencode/blob/4a1982f5c951850a1820e7eb0c9ed4b4613a2912/packages/opencode/src/session/compaction.ts) | Pruning protects 40,000 recent tool-output tokens, requires more than 20,000 tokens of actual savings, protects two recent turns, and replaces old output with at most 2,000 characters. | Coder's measured request peak is about 32,000 total tokens, below OpenCode's protected old-tool budget alone. Keep the 24,000-character per-result cap and defer old-result clearing. |
| Claude `FileEditTool` local source | `packages/builtin-tools/src/tools/FileEditTool/utils.ts` applies an internal `edits[]` sequentially, while the public Edit contract remains one exact `old_string`/`new_string` replacement and the prompt permits multiple Edit calls in one model message. | Preserve the exact-replacement contract and add an optional same-file `edits[]` shape to the existing tool. Do not import Claude's larger edit stack or add another public tool. |
| Codex Rust `apply-patch` local source | `codex-rs/apply-patch/src/lib.rs` and its tool tests accept multiple files and hunks in one `apply_patch` call, parse the complete change before mutation, and reject invalid patches. | Keep Coder's patch-file path for user/API patch workflows. Provider repair turns use the smaller exact-edit tool, but batch validation must be atomic before its one file write. |

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
optional workflow limit has no invented default. Custom weights and reminder
thresholds remain absent until cross-provider evaluations justify them.

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
  bit. Removed top-level copies of acceptance criteria, risks, affected paths,
  workflow ID, and the static conversation summary.
- Subagent runtime context now contains one `coder` object. Removed duplicate
  `parent_backend_context`, `coder_subagent`, context-factory copies, source
  citations, and descriptive policy JSON that no runtime path read.
- Model-visible subagent results contain status, report, task metadata, and
  durable metadata/transcript references. Full event previews remain available
  in the API payload and store but are not copied back into the model history.
- Completed and cancelled background commands/subagents leave the in-memory
  registry after their durable record is written. Background subagents are
  capped at three live child tasks: Codex's default of four session threads
  includes the root agent.
- Removed the unused `CODER_AUTO_BACKGROUND_TASKS` /
  `CLAUDE_AUTO_BACKGROUND_TASKS` compatibility branch and its 120,000 ms
  constant.
- Removed Planner harness tool declarations that the tool-disabled adapter never
  exposed. Workflow Planner now reuses the default model reference and provider
  runtime instead of carrying a separate client or model alias.
- Added provider-backed Workflow Planner decisions with strict JSON output and
  code-enforced stop gates. Successful closed tasks retain a zero-provider
  fast path; qualitative goals and verifier failures receive model analysis.
- A missing, failed, or malformed live Workflow Planner decision is `blocked`,
  not converted to `finish` by an earlier smoke-test pass. Low gain,
  repetition, no progress, and final-round exhaustion remain ordinary stop
  gates that may accept genuinely verified work.
- Planner Chat provider failures are also represented as a persisted blocked
  turn rather than HTTP 500 or a ready deterministic fallback. The current
  user request remains in the session, the provider status is visible, and no
  execution authorization is produced.
- Browser/game verification now uses word-boundary intent routing, reads inline
  scripts, starts common button/overlay entry surfaces, and prioritizes console
  errors over a generic no-progress failure.
- `edit_text_file` now accepts either its legacy single replacement or at most
  32 ordered same-file replacements in `edits[]`. All replacements are applied
  and validated in memory before one write; a later ambiguous or missing match
  leaves the file unchanged. This removes repeated provider turns without a new
  patch parser, tool, agent, or file-write path.
- Removed synthetic subagent cleanup records that listed only
  `not_configured`/`not_applicable` actions. Rust task ownership and explicit
  cancellation remain the real cleanup boundary.

## Current Agent Boundaries

| Role | Context | Tools | When invoked |
| --- | --- | --- | --- |
| Planner Chat | Bounded conversation history plus current plan | No execution tools | User conversation, including while work is active |
| Executor | Compact approved plan, run events, tool results, queued guidance | Harness-scoped native tools | Start Work and repair rounds |
| Verifier | Task, repo, and evidence snapshot | Read-only verifier checks | After each Executor outcome |
| Workflow Planner | Goal, plan criteria, verifier evidence, round budget, current progress, and at most three prior directions | No execution tools | Every verifier outcome; provider call only for failure/blocking or open-ended quality intent |
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
  `plan_context.start_work_authorized=true`. The task and plan goal retain only
  the sanitized domain objective; the old repeated textual authorization was
  removed from both fields.
- Provider tool turns stop immediately on `finish`. The example Executor sets
  24 turns explicitly, and the native runtime uses the same fallback if a
  custom configuration omits the value.
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
- `workflow.token_budget` is an optional run-level stop threshold. It is shared
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
- Retained DeepSeek logs demonstrate why the shared bound is distinct from
  rounds and turns. The from-zero game run used 110 provider turns and 341,376
  weighted tokens (`163,414` output + `177,962` non-cached input); its repair
  run used 40 turns and 108,312 weighted tokens. These are reproducible sums of
  `model.provider_turn.completed` records across the root and subagent JSONL
  files. They motivated the budget implementation; they are not default limits.
- Browser checks are selected from task intent. A Node.js utility is not a
  browser task merely because it uses JavaScript.
- Missing verifier runtime, credentials, permission, or network state is an
  external blocker and is not routed back as a code repair.

## Implemented Native Capabilities

- Provider-backed repo/search/read/git/write/command/background tool loop.
- Shared permission, hook, evidence, result-bounding, and redaction pipeline.
- Prompt, isolated agent, command, async rewake, and webhook hooks.
- Foreground-to-background command handoff, bounded output, polling, and
  cancellation.
- Skills with scoped context modifiers and forked execution.
- Synchronous/background subagents with filtered tools, sidechain transcripts,
  durable status, polling, cancellation, and lost-handle recovery.
- Deterministic plan compaction, model-backed transcript compaction, bounded
  restoration attachments, and persistent failure circuits.
- Browser/game static and dynamic checks using a Coder-owned Playwright runtime.
- Append-only run/session evidence, Review Changes, conservative undo, cache
  accounting, and bounded page/tail reads.
- Active model/tool cancellation using `tokio::sync::watch`; pause is observed
  at workflow boundaries.

## Completion Evidence

| Requirement | Evidence | Status |
| --- | --- | --- |
| Native lightweight core | Cargo workspace contains 11 native crates; source/config/script search has no removed-runtime references. | Complete |
| Maintainable module layout | `coder-server/src/lib.rs` and `coder-workflow/src/lib.rs` are wiring layers; behavior lives in focused modules. | Complete |
| Parallel Planner and workflow | Deterministic concurrency/cancellation tests pass; the live game session retained a parallel Planner turn after workflow completion. | Complete |
| Local status/supplement/stop control | Targeted server tests prove zero-provider routing and in-flight cancellation. | Complete |
| Low-token successful path | Pure confirmation is local; closed verified tasks use a zero-provider Planner fast path; open-ended quality review is capped at 900 output tokens. | Complete |
| Real provider path | DeepSeek `deepseek-v4-flash`, direct proxy mode, SSE Planner transport, native tools, hooks, compaction, subagent, Review Changes, and secret checks executed successfully. | Complete |
| Autonomous open-ended task | Run `e444d194-5a31-4417-b3f8-c3a3fe8dd30e` created and verified a browser garden-defense game from a short prompt; independent QA records the remaining quality gap below. | Complete |
| Documentation | Maintained docs describe the current native path and resource policy; superseded planning/history docs are deleted. | Complete |

## Bounded Planner Live Evidence

The 2026-07-11 DeepSeek evaluation used a sparse open-ended prompt. Coder built
the game from an empty directory; no game source was edited manually:

- Run `e444d194-5a31-4417-b3f8-c3a3fe8dd30e` completed with 21 Executor
  provider turns, 355,817 input tokens, 337,792 cache-read tokens, and 10,851
  output tokens. Its weighted non-cache cost was 28,876 tokens. This is a major
  reduction from the retained 110-turn, 341,376-weighted-token baseline.
- The run used exact text edits, a child review agent, browser verification,
  Review Changes, and one provider-backed Workflow Planner decision. The
  Workflow Planner finished after one round because all generic browser checks
  passed.
- Independent Playwright QA at 1440x900 and 375x667 proved that desktop start,
  placement, passive sun, lose, win, and restart states work without console
  errors. It also found evidence the generic verifier missed: the 564px board
  spans `-94.5..469.5` in a 375px viewport while body overflow is hidden; the
  start panel spans `-147..869` in a 667px viewport; the fifth-row zombie is
  about 85px below its row; and Wave 1 displays `Wave 2 incoming`.
- Quality score: **5.2/10**. The desktop surface is recognizable and playable,
  with three plant and zombie roles, sun economy, waves, combat, and restart.
  Mobile is materially unusable, the wave announcement is wrong, and the art
  and strategic depth remain modest.
- A normal-user repair request produced run
  `8c69c147-2a59-4876-b1d3-7271518add64`. It used 24 provider turns, 443,889
  input tokens, 429,312 cache-read tokens, and 10,039 output tokens, for a
  weighted cost of 24,616. It made 21 successful exact edits and added zombie
  count/health feedback, but independent QA showed all four defects unchanged.
- That repair exposed a control bug: Executor reported
  `stopped_after_turn_limit_with_file_writes`, while Workflow Planner still
  claimed the UI was responsive and finished from generic smoke evidence.
  Coder now forces one bounded completion/self-review round for this exact
  qualitative interruption, then reports blocked if it repeats or reaches the
  final round. Closed deterministic tasks keep their zero-provider fast path.
- Planned acceptance criteria are no longer copied into final-report `checks`
  as if they were evidence. Chat Planner now makes qualitative criteria
  falsifiable and deterministically adds a representative primary-flow review;
  rendered experiences also require one desktop and one mobile viewport with
  no clipping, overlap, overflow, or unreachable controls.
- Two real Planner-only probes with `deepseek-v4-flash` confirmed the gap and
  fix. The first returned only generic functional/desktop criteria. The second
  retained the model's domain plan and added the bounded flow and desktop/mobile
  evidence criteria. The second probe used 1,121 input, 256 cache-read, and 508
  output tokens and did not start a workflow or edit files.
- A post-fix full Executor replay was requested with explicit user approval,
  but the host execution policy rejected exporting the target game's source to
  the external provider. No post-batch DeepSeek turn-count or quality
  improvement is claimed. Local schema, compatibility, sequential-application,
  atomic-failure, and provider-loop tests pass.

## Previous Live Baseline

The 2026-07-10 DeepSeek run predates the bounded success-path Workflow Planner.
It used one workflow round and no Workflow Planner provider call:

- Planner initial turn: 1 provider request; pure confirmation: 0 requests.
- Parallel Planner conversation remained available during Start Work and was
  preserved after completion.
- Executor: 16 provider turns, 281,829 input tokens, 18,296 output tokens,
  265,728 cache-read tokens, and 300,125 total reported tokens.
- Only 16,101 Executor input tokens were not cache reads (94.3% of input was
  cached); the largest single request was 32,002 input tokens. This does not
  justify aggressive history compaction yet because rewriting the prefix could
  trade a small context reduction for lower cache reuse.
- Tools included file writes, commands, repo reads, `agent_subagent`,
  `read_subagent_status`, and `finish`.
- Browser verification passed page load, static structure, gameplay input/loop,
  visible progress, Edge launch, and console checks.
- The run produced `index.html`, `main.js`, and `style.css`; Review Changes and
  the final report contained all three.
- Independent Playwright QA placed a plant, observed resource reduction,
  started a wave, and observed an enemy. The result scored 7.8/10: fully
  playable, with modest emoji-driven art and a clipped mobile Wave indicator.

## Residual Risk

These are release-depth improvements, not missing core architecture:

- Decide whether generic viewport geometry belongs in the optional browser
  verifier after measuring false positives. Planner criteria now require the
  evidence, but the current verifier still does not collect it automatically.
- Measure prompt-cache behavior with providers beyond DeepSeek before adding
  cache-edit complexity.
- Add live multi-turn skill-modifier pressure when a real skill workflow needs
  it.
- Re-run a full qualitative repair after the new interrupted-Executor gate and
  batch edit contract in an environment whose data-export policy permits the
  explicitly approved provider request. Unit tests and Planner-only DeepSeek
  evidence pass, but no second paid Executor repair is claimed for these gates.
- Measure whether same-file batches reduce the latest repair's 24 turns for 21
  successful edits and one failed call. The implementation removes the known
  one-edit-per-turn interface pressure, but only a real replay can establish
  model adoption, token savings, and earlier `finish` behavior.
- Add task-specific behavioral evidence without baking game rules into the
  fixed architecture. The latest mobile and wave defects show that generic
  browser progress is necessary but not sufficient for qualitative completion.
- The native provider loop still clones and serializes its accumulated message
  vector on every turn, but this is a transient copy rather than a demonstrated
  leak. The measured request peak was about 32,000 tokens, and DeepSeek reused
  most of the prefix from cache. Claude's ordinary old-result clearing is
  disabled by default, Codex also clones a normalized prompt snapshot, and
  OpenCode protects 40,000 recent tool-output tokens before pruning. Do not
  trade cache reuse for compaction until a run approaches Coder's 167,000-token
  threshold or memory profiling attributes material retained bytes here.
- Planner Chat has no repository tools. This is intentional for the current
  side-effect-free boundary; large-repository planning should be evaluated
  before deciding whether a bounded read-only exploration step is justified.
- Keep browser runtime provisioning Coder-owned and optional; never install it
  into the user's target repo as a side effect of verification.

## Maintenance Rule

Prefer deleting duplicated agents, model calls, adapters, scripts, and docs.
Add a component only when it owns a distinct state boundary or removes measured
complexity. Every retained runtime parameter must have a source, a validation
rule, and an observable event or test.
