# Coder

Planner-led local AgentGraph workbench for controlled coding tasks.

## Rust Skeleton

The Rust-first rebuild has started as an additive workspace. It does not replace
the current Python/FastAPI runtime yet.

Current Rust commands:

```powershell
cargo run -p coder-cli --bin coder-rust -- doctor
cargo run -p coder-cli --bin coder-rust -- config validate --path examples\coder.yaml
cargo run -p coder-cli --bin coder-rust -- workflow run --mock planner-led "summarize this repo"
cargo run -p coder-cli --bin coder-rust -- openhands doctor --server http://127.0.0.1:8000
cargo run -p coder-cli --bin coder-rust -- openhands run --server http://127.0.0.1:8000 --conversation-id <id> "summarize this repo"
cargo run -p coder-cli --bin coder-rust -- server --host 127.0.0.1 --port 8766
```

`openhands run` writes a Rust run directory with `metadata.json`,
`events.jsonl`, raw OpenHands event blob refs, and a `final-report.json`
artifact so the spike follows the same evidence-first run shape as the mock
workflow path.

Rust checks:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Coder keeps the ordinary product path small:

```text
User request
-> Planning Chat session (Discuss or Work) or legacy draft
-> Work-mode readiness handoff or user confirmation
-> AgentGraphRunner / RunController
-> SharedRunState
-> Context packets and RoundWorkingSet
-> HarnessRuntimeManager
-> Conversation Harness / Task Execution Harness
-> OpenHandsRuntimeProvider or InternalFallbackProvider
-> NativeRuntimeStore / BlobStore
-> ArtifactProjector
-> final_report
```

The Planner owns global decisions and communicates ordinary user-facing
outcomes through the `final_report` artifact. Executors perform bounded
implementation work, run allowed checks, and return structured verification
evidence. The runtime passes structured artifacts and refs instead of
transcript-sized context.

## OpenHands Runtime Refactor

Coder is moving toward this runtime split:

```text
Coder = Agent OS / workflow orchestrator
OpenHands SDK = default agent runtime provider when enabled
```

Coder owns the product flow, Planner authority, AgentGraph loop,
RunController, context and memory planes, skill routing, safety policy,
sandbox policy, artifact validation, runtime event storage, observability, and
final reports. OpenHands owns the model/tool interaction loop and native
workspace events when `CODER_ENABLE_OPENHANDS_RUNTIME=1` and the SDK is
available. Without that flag or SDK, Coder uses the internal fallback provider
so local development and tests remain runnable.

OpenHands SDK runtime verification uses Python 3.12 or newer. The local
integration has been import-verified with `openhands-sdk` and `openhands-tools`
1.29.2, but those packages remain optional until the adapter is fully wired and
validated.

Workflow Supervisor `planner_order` calls now include a strict Coder JSON
output contract. OpenHands must return exactly one `planner_order` object with
`plan_graph.work_items`; an empty work list is accepted only when
`no_work_rationale` explains why no executor work is needed. Unstructured
planner output still fails closed with
`insufficient_structured_planner_output`.

Harness dry-run readiness is available through
`run_harness_dry_run(request)`. It reports `ready`, `warning`, or `blocked`
for a requested harness run by checking artifact targets, profile bindings,
OpenHands SDK importability, credential presence, model/base URL configuration,
prompt contract availability, sandbox readiness, permission policy readiness,
trace support, and license metadata. Dry-run is diagnostic only: it does not
call a model, execute tools, start an OpenHands conversation, prepare or copy a
workspace, or include secrets, prompts, model output, logs, or diffs in the
report.

OpenHands model configuration now resolves through additive LLM provider
profiles. The default `deepseek-default` profile preserves DeepSeek-first
behavior, including `LLM_API_KEY` or `DEEPSEEK_API_KEY`,
`https://api.deepseek.com`, and DeepSeek model alias normalization. Set
`CODER_LLM_PROVIDER_PROFILE=openai-compatible-env` to use the generic
OpenAI-compatible env profile, or leave it unset for DeepSeek. Existing
`LLM_MODEL` and `LLM_BASE_URL` overrides still take precedence.

The ordinary Planner Chat path is explicit:

```text
Discuss:
  User <-> Planner multi-turn session
  Produces PlannerTaskState / PlannerChatTurn
  Never starts workflow execution

Work:
  User <-> Planner multi-turn session
  If PlannerTaskState is ready_to_execute, starts the existing AgentGraph run

Legacy draft:
  ProjectPlanDraft / RunContractDraft
  User confirms or discards
  Live AgentGraph run starts only after confirmation
```

Planning Chat Mode cannot modify files or run commands. Discuss mode cannot
start workflow execution. Work mode is the user's execution confirmation, but
the Planner may start the workflow only when the current `PlannerTaskState`
has a goal, success criteria, no open questions, and a structured handoff.
In-run Workflow Supervisor Mode produces planner orders, planner decisions,
activity updates, and final reports. Task Execution Harnesses perform bounded
execution work and cannot talk to the user, commit, push, deploy, or write
long-term memory directly.

## Runtime Hardening

The AgentGraph runtime now includes a hardened execution layer while preserving
the Planner -> Task Execution Harness -> Planner authority model:

- `ActionGateway` can route low-level tool effects through an internal
  `ToolExecutionService` with ordered results, conservative concurrency,
  per-action timeout handling, structured execution events, and large result
  budgeting.
- Context construction supports artifact-aware projection and compaction.
  Oversized snippets, artifacts, and tool outputs are represented as previews
  plus `sha256:<digest>` BlobStore refs.
- The executor harness supports bounded self-checks, verification checks, and
  multi-stage artifact repair. Invalid model output becomes a Planner-visible
  blocked artifact instead of an unstructured runtime crash.
- The CodeWorker harness can optionally run a provider-neutral internal
  action/observation loop. When `CODER_ENABLE_CODE_WORKER_TOOL_LOOP=1`, model
  steps must return `harness_action`, `harness_action_batch`, or a final
  `execution_result`; runtime validates actions through `ToolGate`, executes
  supported effects through `ActionGateway`, budgets large outputs into
  preview/ref form, records action lifecycle, and enriches the final artifact
  from runtime facts instead of model claims.
- CodeWorker finalization now uses a stop gate and bounded recovery policy:
  failed command or patch evidence cannot be ignored, model-provided changed
  files and patch refs are checked against runtime session facts, read/search
  actions can batch, patch/command actions remain exclusive, and future
  streaming execution has a provider-neutral internal executor.
- `WaveExecutor` records per-work-item attempts, timeout/cancel evidence,
  conservative retry diagnostics, completed/blocked outcomes, and wave-level
  summaries.
- Consecutive blocked rounds are promoted to a blocked run result so the Planner
  does not loop indefinitely on the same unresolved execution blocker.
- Planner decisions are normalized to `continue` or `finish`. Blocked, failed,
  and cancelled finishes produce a structured `final_report` artifact instead
  of relying on ad hoc status text.
- `SharedRunState` records compact control, planner, work-item, artifact,
  message, memory, and final-report refs. Planner, Executor, final-report, and
  debug views are projected from that shared state instead of exposing raw
  runtime caches by default.
- Canonical harness contracts and the runtime capability resolver define the
  tool, memory, skill, and denied-capability surface for the Conversation
  Harness and Task Execution Harness, while legacy harness IDs remain supported
  during migration.
- `HarnessRuntimeManager` is the central runtime entrypoint. It applies safety
  and sandbox policy before selecting `OpenHandsRuntimeProvider` or
  `InternalFallbackProvider`.
- Live AgentGraph runs expose pause, resume, cancel, and heartbeat control for
  long/background execution.

## Long Context, Storage, And Recovery

Coder uses one durable path for large text:

- `BlobStore` stores full large strings by `sha256:<digest>` id.
- Context packets live in `ContextPacketStore`; tool results live in
  `ToolResultStore`.
- Events store summary, id/ref, status, and size, not full packets or large
  tool output.
- Normal AgentGraph events are ref-only for context packets, coding packets,
  token ledger entries, task envelopes, and tool outputs. Full payloads live in
  run partitions and are loaded only through explicit artifact, context-packet,
  tool-result, or blob endpoints.
- `ContextService` remains the public context construction entrypoint. Its
  private projection chooses hot/warm/cold context before `ContextCompactor`
  shrinks selected fields.
- `RunResult.resume_checkpoint` is the active recovery path for interrupted
  AgentGraph runs. Reloaded queued/running live runs with checkpoint data are
  marked `blocked` with `status_code="resume_available"`.
- `run_group_id`, `parent_run_id`, `continued_from_run_id`, and `turn_index`
  provide lightweight multi-run continuity without a parallel session store.

See [docs/runtime-storage.md](docs/runtime-storage.md) and
[docs/long-context.md](docs/long-context.md) for storage contributor notes.
See [docs/codeworker_harness_tool_loop.md](docs/codeworker_harness_tool_loop.md)
for CodeWorker tool-loop architecture, action policy, recovery, lifecycle, and
test commands.

## Agent-Scoped Memory Plane

Coder now separates legacy workflow memory compatibility from the Batch D
agent-scoped memory plane:

- `MemoryService`, `WorkflowMemoryStore`, `MemoryDelta`, and staged memory
  writes remain the legacy workflow-memory adapter.
- `MemoryRecord`, `KnowledgeSource`, and `KnowledgeChunk` define the RAG-ready
  memory taxonomy with ACL metadata, sensitivity, trust level, evidence refs,
  source refs, tags, content hash, status, and token estimates.
- `AgentScopedMemoryStore` and `KnowledgeStore` store append-only JSONL under
  `.coder/memory/`; they do not write memory files into the Git repo by
  default.
- `MemoryRetriever` applies deterministic role policies before returning
  compact `MemoryCard` objects. Planning Chat can read planner/user/project and
  allowed knowledge memory; Workflow Supervisor can read project/run/knowledge
  memory; Task Execution can read only scoped workflow/knowledge context for
  `execution_prompt`.
- `build_harness_context_packet(...)` keeps the existing `hot`/`warm`/
  `cold_refs` shape. Compact memory cards and knowledge hits go into `warm`;
  full memory and knowledge records stay behind cold refs.
- Run-scoped checkpoints are persisted under
  `.coder/runs/{run_id}/memory/` as `checkpoints.jsonl` and
  `latest_snapshot.json`. Snapshots include planner task state, compact
  execution and verification summaries, blockers, changed-file summaries, and
  evidence refs, but not raw logs, prompts, diffs, model outputs, or secrets.
- Long-term planner file memory uses `PlannerMemoryWriteProposal` and
  `PlannerFileMemoryCommitter`. Only Planning Chat may propose updates, and
  user/project planner files require confirmation by default.
- Text/Markdown knowledge imports are available without embeddings or a vector
  database. Imported chunks are ACL-ready and retrievable through the lexical
  `MemoryRetriever` path.
- Hybrid RAG retrieval can be enabled with optional `rag` dependencies. It
  builds `.coder/indexes/bm25/` and, when Chroma is installed,
  `.coder/indexes/chroma/`, then fuses dense and lexical candidates with
  weighted RRF while keeping Batch D ACLs authoritative.
- OpenHands receives `coder_hybrid_rag_search` as a read-only custom tool in
  Planning Chat, Workflow Supervisor, and Task Execution modes. Coder binds the
  role, requested context, store root, run identity, scope, and token budget;
  model tool arguments can only provide `query`, `top_k`, `tags`, and
  `include_content`.

See [docs/hybrid_rag_tool.md](docs/hybrid_rag_tool.md) for reindexing,
optional dependency, retrieval, ACL, and OpenHands custom tool details.

## Agentic Hybrid Context Router And Native Code Context

Coder separates current repository evidence from knowledge hints through an
agentic routing loop:

- native repo discovery/search/read, test output, and diffs are evidence for
  current code facts;
- run evidence covers execution results, verification summaries, blockers,
  diff refs, log refs, and native runtime refs;
- hybrid RAG, memory, external docs, project notes, roadmaps, and future
  Obsidian notes are knowledge hints;
- RAG results that mention code must be verified with native repo search/read
  before they are used as current code facts.

The router follows:

```text
classify -> route -> retrieve -> grade -> rewrite -> switch -> verify -> assemble
```

Planning Chat may use RAG early for planning, history, decisions, roadmaps,
external docs, and user-maintained notes. Workflow Supervisor starts with run
evidence. Task Execution starts with native repo evidence and never starts
RAG-first.

Native context services write refs under `.coder/runs/{run_id}/repo_evidence/`
and inject compact records into `warm.repo_evidence`. Run facts go into
`warm.run_evidence`. Knowledge hints remain in `warm.knowledge_hints` or the
existing memory/knowledge fields. OpenHands gets read-only
`coder_repo_find_files`, `coder_repo_search_text`, and
`coder_repo_read_file` tools in addition to `coder_hybrid_rag_search`.

See [docs/agentic_context_router.md](docs/agentic_context_router.md),
[docs/retrieval_policy.md](docs/retrieval_policy.md), and
[docs/native_code_context.md](docs/native_code_context.md).

Most low-level runtime behavior is feature-flagged during rollout:

```powershell
CODER_ENABLE_TOOL_EXECUTION_SERVICE=1
CODER_ENABLE_CONTEXT_COMPACTION=1
CODER_ENABLE_HARNESS_SELF_CHECK=1
CODER_ENABLE_CODE_WORKER_TOOL_LOOP=1
CODER_ENABLE_WAVE_RETRY=1
CODER_ENABLE_OPENHANDS_RUNTIME=1
```

## Current Product Surface

The app uses a ChatGPT-style left sidebar and keeps chat separate from workflow
editing:

- `Planner Chat`: use Discuss mode for multi-turn planning that never starts
  execution, Work mode to start the existing workflow once the task is ready,
  or the legacy draft/confirm flow for explicit review before execution. The
  page shows the structured final report, run status, evidence, patches,
  checks, and explicit debug exports.
- `Agent Workflow`: load saved Agent workflows, load the default workflow, edit
  the basic Planner -> Executor loop, save, save as a new copy, import, and
  export.
- `Extensions`: manage installed plugins and skills.
- `Settings`: configure the local model provider.

The ordinary product UI does not expose runtime JSON editors, workflow IDs,
agent inspectors, manual edge labels, engine settings, harness controls, or
legacy runtime previews. Stored run inspection and raw event data remain
available only through explicit debug/export affordances and API endpoints, not
as the default Planner Chat journey.

Normal Planner decisions are `continue` or `finish`; legacy `ask_human`,
`planner_human_prompt`, and planner-response resume flows are not part of the
ordinary product path. If the run cannot safely proceed, it finishes with a
blocked `final_report`.

## Repository Layout

```text
src/coder_workbench/
  actions/           ActionSpec and ActionGateway for controlled effects
  agent_graph/       Planner-led graph runner, scheduling, working set, merge logic
  agent_harness/     Legacy/fallback harness loops and JSON artifact repair
  agent_model/       Agent recipes and runtime profile compilation
  harness_runtime/   Canonical harness contracts, providers, safety, sandbox, native events
  budget/            BudgetBroker reservations and round preflight
  coding/            Repo intelligence, patching, command checks, diagnostics
  context/           Context packet and skill-context construction
  core/              AgentWorkflowSpec, artifacts, authority, role cards
  extensions/        Plugin and skill manifests, routing, runtime
  memory/            Legacy workflow memory plus agent-scoped memory stores,
                     retrieval, hybrid RAG indexes, run snapshots, imports
  openhands_tools/   Coder custom RAG and repo-context tools registered into OpenHands SDK
  observability/     Tracing and evaluation support
  runtime_capabilities/
                     Harness capability resolver, tool/MCP/skill registries
  runtime_kernel/    RunController, RunGuard, round state, fingerprints
  runtime_state/     SharedRunState reducers and bounded state views
  server/            FastAPI app, storage, live run managers
  skills/            Installed skill store and registry client
frontend/
  src/               React + TypeScript Workbench
tests/               Python unittest suite
examples/            Example Agent workflow payloads
```

## Install

Requires Python 3.12 or newer and Node.js for the frontend.

```powershell
git clone https://github.com/Garfreak-07/Coder.git
cd Coder
python -m venv .venv
.\.venv\Scripts\activate
pip install -e .
cd frontend
npm install
```

Install optional local RAG dependencies when you want Chroma dense retrieval and
the external BM25 implementation:

```powershell
pip install -e .[rag]
```

## Run Locally

Start the API:

```powershell
.\.venv\Scripts\coder-api.exe --host 127.0.0.1 --port 8876
```

Start the frontend dev server:

```powershell
cd frontend
npm.cmd run dev
```

Open:

```text
http://127.0.0.1:5173
```

Vite proxies `/api/*` to `http://127.0.0.1:8876`.

If `frontend/dist` exists, the API can serve the built frontend from:

```text
http://127.0.0.1:8876
```

## CLI Example

```powershell
.\.venv\Scripts\coder.exe --repo . `
  --agent-workflow examples\workflows\coding-workbench.json `
  --request "Inspect this project and propose the next safe step"
```

Use `--approve` only when you intentionally want to preapprove local effects for
a trusted run.

## API Surface

Common development endpoints:

- `GET /api/v2/health`
- `GET /api/v2/agent-role-cards`
- `GET /api/v2/agent-workflows/default`
- `POST /api/v2/agent-workflows/validate`
- `POST /api/v2/agent-workflows/runtime-profiles`
- `POST /api/v2/planner-chat/draft`
- `POST /api/v2/planner-chat/confirm`
- `POST /api/v2/planner-chat/sessions`
- `POST /api/v2/planner-chat/sessions/{session_id}/turn`
- `GET /api/v2/planner-chat/sessions/{session_id}`
- `POST /api/v2/knowledge-sources/import-text`
- `GET /api/v2/knowledge-sources`
- `GET /api/v2/knowledge-sources/{source_id}/chunks`
- `POST /api/v2/rag/reindex`
- `GET /api/v2/rag/status`
- `GET /api/v2/library`
- `POST /api/v2/library/agent-workflows`
- `GET /api/v2/library/agent-workflows/{workflow_id}`
- `POST /api/v2/live-agent-runs`
- `GET /api/v2/live-agent-runs/{run_id}`
- `GET /api/v2/live-agent-runs/{run_id}/events`
- `POST /api/v2/live-agent-runs/{run_id}/pause`
- `POST /api/v2/live-agent-runs/{run_id}/resume`
- `POST /api/v2/live-agent-runs/{run_id}/cancel`
- `GET /api/v2/live-agent-runs/{run_id}/heartbeat`
- `GET /api/v2/runs`
- `GET /api/v2/runs/{run_id}`
- `GET /api/v2/runs/{run_id}/events`
- `GET /api/v2/runs/{run_id}/artifacts/{artifact_id}`
- `GET /api/v2/runs/{run_id}/context-packets/{packet_id}`
- `GET /api/v2/runs/{run_id}/tool-results/{tool_result_id}`
- `GET /api/v2/runs/{run_id}/blobs/{blob_id}`
- `GET /api/v2/extensions/plugins`
- `GET /api/v2/extensions/skills`
- `GET /api/v2/extensions/search`

## Testing

Backend:

```powershell
.\.venv\Scripts\python.exe -m unittest discover -s tests
.\.venv\Scripts\python.exe -m compileall src tests
```

Frontend:

```powershell
cd frontend
npm.cmd run build
```

## Development Rules

- Keep the ordinary product path Planner-led and AgentGraph-only.
- User interaction must remain `User <-> Planner`.
- Executors must not ask the user directly.
- Executor results must include `execution_result.verification` with pass,
  skipped, failed, or blocked evidence for the Planner to judge.
- Planner Chat Discuss mode must never start workflow execution. Work mode can
  start a live AgentGraph run only from a validated ready PlannerTaskState.
- Legacy Planner Chat drafts must be confirmed before a live AgentGraph run
  starts.
- Conversation Harness profiles cannot write files, run commands, commit, push,
  deploy, or publish externally.
- Task Execution Harness profiles cannot ask the user, commit, push, deploy,
  publish externally, or write long-term memory directly.
- Product live Agent workflows must run through AgentGraph.
- New context, patch, command, repair, validation, plugin, and MCP behavior
  should enter through `ActionGateway`.
- CodeWorker tool-loop actions must remain scoped to `read_file`,
  `search_files`, `inspect_git_diff`, `propose_patch`, `apply_patch_sandbox`,
  `run_command_sandbox`, `read_tool_output`, and `return_execution_result`.
  Executors must not use `run_command`, publish externally, install plugins, or
  enable MCP servers from inside the harness.
- Executors cannot write long-term memory directly. Legacy workflow memory
  writes must go through `MemoryService` as evidence-backed, staged/gated
  deltas. Long-term planner file memory must go through
  `PlannerMemoryWriteProposal` and `PlannerFileMemoryCommitter`; Workflow
  Supervisor and Task Execution must not write those files.
- New tools, MCP manifests, and skills should be represented in the runtime
  capability registries before they are exposed to a harness. MCP manifests are
  parsed and validated locally and are never enabled by default.
- Budget-affecting work should use `BudgetBroker` preflight and reservations.
- Large text should be persisted through `BlobStore`; do not add another durable
  context or tool-result ref format.
- Current code facts must be grounded in repo evidence: native search/read,
  tests, logs, or diffs. RAG, memory, future Obsidian notes, and external docs
  are hints until verified against the current repo.
- Future MCP retrieval adapters must call Coder retrieval services and ACLs;
  do not expose raw Chroma/BM25 or a global unscoped query engine.
- The ordinary UI should not expose runtime JSON, engine knobs, harness graphs,
  context policies, token budgets, manual capability checklists, or advanced
  edge editing.

## Secrets

Do not commit API keys or local secrets. Copy `.env.example` to `.env` for local
model configuration. `.env`, `.env.local`, and `.local-env.ps1` are ignored by
Git.

Provider configuration is OpenAI-compatible and optional. Without credentials,
the runtime uses mock mode for safe local development.

For OpenHands runtime smoke tests, prefer local environment variables rather
than committed files:

```powershell
$env:CODER_LLM_PROVIDER_PROFILE="deepseek-default"
$env:DEEPSEEK_API_KEY="..."
$env:LLM_API_KEY=$env:DEEPSEEK_API_KEY
$env:LLM_BASE_URL="https://api.deepseek.com"
$env:LLM_MODEL="deepseek-v4-flash"
$env:CODER_ENABLE_OPENHANDS_RUNTIME="1"
```

## License

License: AGPL-3.0-or-later. See [LICENSE](LICENSE).

Earlier versions of this repository were released under the MIT License. This
license change applies to this version and later versions; it does not revoke
rights already granted for earlier MIT-licensed versions.
