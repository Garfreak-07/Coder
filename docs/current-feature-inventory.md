# Current Feature Inventory

This document is the Phase 0 no-regression baseline for the Rust-first rebuild.
It inventories current user-visible features and important internal subsystems
before any future deletion or quarantine of Python code.

Classification values:

- `KEEP_AS_USER_FEATURE`: keep the behavior and user concept.
- `REPLACE_WITH_RUST_CORE`: preserve behavior through Rust control-plane code.
- `MOVE_TO_OPENHANDS_BACKEND`: preserve behavior through the OpenHands adapter.
- `MOVE_TO_PLUGIN_OR_LATER_PHASE`: preserve the public shape, implement later.
- `DELETE_INTERNAL_CONCEPT_ONLY`: remove/rename only after behavior is mapped.
- `UNKNOWN_NEEDS_INVESTIGATION`: do not delete until product impact is known.

## Inventory Table

| Current item | User-visible? | Current location | Current behavior | Classification | Rust equivalent | Risk | Tests needed |
|---|---:|---|---|---|---|---|---|
| Planner Chat discuss mode | Yes | `frontend/src/features/planner-chat`, `src/coder_workbench/server/app.py` | Multi-turn planning that never starts workflow execution | KEEP_AS_USER_FEATURE | Rust session/run preview API with Planner agent state | High: accidental execution from discuss mode | Existing planner chat tests plus Rust API session tests |
| Planner Chat work mode | Yes | `frontend/src/features/planner-chat`, `src/coder_workbench/server/app.py` | Starts live AgentGraph only when task state is ready | KEEP_AS_USER_FEATURE | Rust run start gate backed by workflow validation | High: bypassing readiness or user confirmation | Ready/not-ready work-mode tests |
| Legacy draft/confirm flow | Yes | `/api/v2/planner-chat/draft`, `/api/v2/planner-chat/confirm` | User reviews a run contract before execution | REPLACE_WITH_RUST_CORE | Rust run preview and confirmation gate | Medium: breaking older UI/API clients | Compatibility tests until replaced |
| Agent Workflow canvas | Yes | `frontend/src/features/agent-workflow`, `frontend/src/workflowGraph.ts` | Visual Planner -> Executor graph editing | KEEP_AS_USER_FEATURE | `WorkflowSpec` editor and UI protocol DTOs | Critical: Coder differentiator can regress | Canvas roundtrip, import/export, visual smoke tests |
| Workflow save/save-as/import/export | Yes | `frontend/src/features/agent-workflow`, library endpoints | Persist and exchange workflow JSON | KEEP_AS_USER_FEATURE | Versioned WorkflowSpec bundle YAML/JSON | High: user data loss | Collision, roundtrip, invalid graph tests |
| Agent role cards | Yes | `src/coder_workbench/core/archetypes.py` | Ordinary-user role choices for Planner/Executor | KEEP_AS_USER_FEATURE | `AgentSpec` templates and role catalog | Medium: leaking internal engine IDs | Role card catalog and UI copy tests |
| User-defined agents | Yes | Agent workflow JSON, library storage | Users configure agents, capabilities, model tier, skills/memory packs | KEEP_AS_USER_FEATURE | `AgentSpec` plus template library | High: custom workflows lose power | AgentSpec validation and migration fixtures |
| User-defined workflows | Yes | `AgentWorkflowSpec`, library storage | Users save custom agent graphs and loop policy | KEEP_AS_USER_FEATURE | `WorkflowSpec` with nodes, edges, stop policy | High: invalid graph execution | Workflow validation success/failure tests |
| User-defined harness/work mode bindings | Partly | `harness_runtime/profiles.py`, `HarnessBindings` | Binds planning/supervisor/execution modes to runtime profiles | KEEP_AS_USER_FEATURE | `HarnessSpec` and node Agent+Harness binding | High: work scenario control hidden/lost | HarnessSpec validation and UI adapter tests |
| Planner -> Executor default workflow | Yes | `examples/workflows/coding-workbench.json`, `core/agent_workflow.py` | Minimal Planner-led loop with max auto rounds | KEEP_AS_USER_FEATURE | Default `WorkflowSpec` template | High: default onboarding breaks | Default workflow validation and UI load tests |
| Planner authority model | Indirect | `agent_graph`, `core/authority.py`, tests | Planner owns subjective decisions and final report | REPLACE_WITH_RUST_CORE | Rust workflow runner and report assembly policy | Critical: executor may overstep | Authority and final-report tests |
| Task Execution Harness boundary | Indirect | `agent_harness`, `harness_runtime/contracts.py` | Executors cannot talk to users, commit, push, deploy, or write long-term memory | REPLACE_WITH_RUST_CORE | Harness permissions, tool registry, memory policy | Critical: safety regression | Permission denial and capability tests |
| HarnessRuntimeManager | No | `src/coder_workbench/harness_runtime/manager.py` | Central provider selection and safety/sandbox preflight | DELETE_INTERNAL_CONCEPT_ONLY | Backend registry plus HarnessBackend trait | Medium: behavior split across adapters | Provider selection and safety tests |
| OpenHandsRuntimeProvider | No | `harness_runtime/openhands_provider.py` | OpenHands SDK-backed planner/executor runtime | MOVE_TO_OPENHANDS_BACKEND | OpenHands Agent Server REST/WebSocket adapter | Critical: OpenHands support loss | External server health/auth/event tests |
| InternalFallbackProvider | No | `harness_runtime/fallback_provider.py` | Local fallback/mock execution when OpenHands unavailable | REPLACE_WITH_RUST_CORE | Mock/native Rust backend | Medium: local dev and tests break | Mock backend and no-credential tests |
| OpenHands custom repo tools | Indirect | `openhands_tools/repo_context.py` | Read-only repo find/search/read exposed to OpenHands | MOVE_TO_OPENHANDS_BACKEND | Rust MCP or HTTP tool bridge | High: OpenHands loses repo grounding | Tool schema, ACL, evidence tests |
| OpenHands hybrid RAG tool | Indirect | `openhands_tools/hybrid_rag_search.py` | Read-only retrieval for OpenHands modes | MOVE_TO_OPENHANDS_BACKEND | Rust memory/RAG tool bridge | Medium: planning context quality drops | Tool argument and ACL tests |
| ActionGateway | No | `src/coder_workbench/actions/gateway.py` | Controlled routing for context, patch, command, plugin/MCP actions | REPLACE_WITH_RUST_CORE | Rust tool execution gateway | High: side effects bypass policy | Tool action and approval tests |
| ToolExecutionService | No | `actions/tool_execution.py` | Ordered, bounded execution of tool actions | REPLACE_WITH_RUST_CORE | Rust native tool executor | Medium: concurrency/timeout regressions | Ordering, timeout, cancellation tests |
| Patch application and rollback | Yes | `coding/patch_*`, `actions/gateway.py`, `/api/v2/patches/rollback` | Preview/apply/rollback patch behavior with scope checks | REPLACE_WITH_RUST_CORE | Rust `apply_patch` tool plus artifact refs | Critical: repo writes unsafe | Scope, rollback, dangerous path tests |
| Command execution/checks | Yes | `coding/command_*`, harness permissions | Runs allowed verification commands and records evidence | REPLACE_WITH_RUST_CORE | Rust command runner with approval events | Critical: command safety | Allow/deny/timeout/output tests |
| Verification evidence | Yes | `execution_result.verification`, final report tests | Executor reports checks as pass/skipped/failed/blocked evidence | KEEP_AS_USER_FEATURE | CheckResult events and report refs | High: reports become model claims | Evidence-backed report tests |
| Final report | Yes | `agent_graph/final_report.py`, `artifact_projector.py` | Structured completed/blocked/failed/cancelled user result | KEEP_AS_USER_FEATURE | `FinalReport` schema from Rust events | Critical: user-facing completion contract | Report schema and evidence ref tests |
| Run status | Yes | `RunResult`, live run APIs | queued/running/completed/blocked/failed/cancelled states | KEEP_AS_USER_FEATURE | `RunState` and event-derived status | High: UI cannot explain state | Lifecycle transition tests |
| Pause/resume/cancel/heartbeat | Yes | `server/agent_manager.py`, run control tests | Controls long/background live runs | KEEP_AS_USER_FEATURE | Rust run lifecycle APIs and cancellation tokens | High: runaway runs | Pause/resume/cancel/heartbeat tests |
| SharedRunState | No | `runtime_state` | Compact refs for control, planner, work item, artifact, memory views | DELETE_INTERNAL_CONCEPT_ONLY | Rust event replay plus bounded UI projection | Medium: debug views lose data | State projection and ref-only tests |
| NativeRuntimeStore | No | `harness_runtime/store.py`, `server/storage.py` | Stores native runtime refs/events | REPLACE_WITH_RUST_CORE | Rust run metadata/events/artifacts store | High: resume/debug breaks | Store roundtrip and migration tests |
| BlobStore/artifacts | Indirect | `core/artifacts.py`, server stores | Content-addressed large text/blob storage and artifact retrieval | REPLACE_WITH_RUST_CORE | Rust BlobStore and ArtifactStore | Critical: large event payloads/secrets | Blob hash, traversal, redaction tests |
| Context packets | Indirect | `context`, `server/stores/contexts.py` | Hot/warm/cold context projection with refs | DELETE_INTERNAL_CONCEPT_ONLY | Context assembler projecting from events/memory/repo evidence | Medium: token bloat or missing context | Compaction and ref-only tests |
| Long context compaction | Indirect | `context/compaction.py` | Shrinks oversized context into previews and blob refs | REPLACE_WITH_RUST_CORE | Rust context projection and large payload policy | Medium: event/log bloat | Large payload and preview tests |
| Repo discovery/search/read/diff/status | Indirect | `context/repo_*`, `coding/repo_index.py`, Git inspection helpers | Grounded repo evidence with path safety and current worktree visibility | REPLACE_WITH_RUST_CORE | Rust native repo tools | High: unsafe path reads, weak grounding, or confusing uncommitted state | Scope, gitignore, binary/env, git status/diff tests |
| Agentic context router | Indirect | `context/agentic_router.py` | Routes between repo evidence, run evidence, and RAG hints | MOVE_TO_PLUGIN_OR_LATER_PHASE | Rust router after core events/tools stabilize | Medium: lower context quality | Router policy tests |
| Legacy workflow memory | Indirect | `memory/service.py` | Staged evidence-backed workflow memory deltas | REPLACE_WITH_RUST_CORE | MemoryScope and proposal events | Medium: memory writes unsafe | Staged write and confirmation tests |
| Agent-scoped memory | Yes/indirect | `memory/models.py`, stores/retriever | ACL-ready user/project/agent/run memory cards | KEEP_AS_USER_FEATURE | Rust memory backend and MemoryCard projection | High: privacy/ACL regression | ACL and role policy tests |
| Knowledge import | Yes | `/api/v2/knowledge-sources/import-text`, `memory/knowledge_import.py` | Text/Markdown knowledge sources and chunks | KEEP_AS_USER_FEATURE | Rust knowledge source loader | Medium: RAG feature loss | Import/chunk/list tests |
| Hybrid RAG | Yes/indirect | `memory/hybrid_*`, optional deps | Lexical+dense retrieval with ACL and RRF | MOVE_TO_PLUGIN_OR_LATER_PHASE | Rust lexical first, optional dense backend later | Medium: optional deps and ACL | Lexical baseline and ACL tests |
| Extensions/plugins | Yes | `extensions`, `/api/v2/extensions/*` | Plugin manifests, search, installed extension surface | KEEP_AS_USER_FEATURE | Rust plugin registry and policy layer | High: ecosystem regression | Manifest, search, install policy tests |
| Skills | Yes | `skills`, `/api/v2/skills/*`, frontend skills panel | Discover/install/update/pin/rollback/disable skills | KEEP_AS_USER_FEATURE | Rust skill store plus progressive disclosure | High: external-effect policy | Skill lifecycle and signature tests |
| MCP direction | Indirect | `tools/mcp.py`, runtime registries | Validated MCP manifests never enabled by default | MOVE_TO_PLUGIN_OR_LATER_PHASE | Rust MCP client/server registry | High: unsafe tool exposure | Manifest validation and deny-by-default tests |
| Provider settings/model config | Yes | `server/settings.py`, Settings page | Configure provider, base URL, API key source, mock mode | KEEP_AS_USER_FEATURE | Rust model profiles and secret refs | High: credential leak or broken model config | Redaction and provider status tests |
| Runtime capability registries | No | `runtime_capabilities` | Tool/MCP/skill capability resolution per harness | DELETE_INTERNAL_CONCEPT_ONLY | HarnessSpec validation and tool registry | Medium: hidden capability leak | Capability matrix tests |
| BudgetBroker | No | `budget` | Token/effect budget preflight and reservations | MOVE_TO_PLUGIN_OR_LATER_PHASE | Rust budget policy after runtime parity | Low/medium: cost controls weaker | Budget denial tests |
| Observability tracing | Indirect | `observability` | Trace spans and diagnostics | MOVE_TO_PLUGIN_OR_LATER_PHASE | Rust tracing/OpenTelemetry exporters | Low: debug quality loss | Trace emission tests |
| API v2 health/capability/library endpoints | Yes | `server/app.py` | Current frontend API | KEEP_AS_USER_FEATURE | Rust API v3 with compatibility adapter | High: frontend breakage | Endpoint compatibility tests |
| API v2 run/artifact endpoints | Yes | `server/app.py`, `server/storage.py` | Run list/detail/events/artifacts/blobs | KEEP_AS_USER_FEATURE | Rust run store endpoints | Critical: run inspection loss | API pagination/retrieval tests |
| Frontend Planner Chat page | Yes | `frontend/src/features/planner-chat` | Chat, readiness, run report, evidence/debug exports | KEEP_AS_USER_FEATURE | Same UI backed by Rust API when ready | High: main UX regression | UI tests and browser smoke |
| Frontend Agent Workflow page | Yes | `frontend/src/features/agent-workflow` | Canvas editor, selector, validation panel, save/import/export | KEEP_AS_USER_FEATURE | WorkflowSpec adapter and validation endpoint | Critical: workflow canvas loss | Adapter and visual smoke tests |
| Frontend Extensions page | Yes | `frontend/src/features/skills` | Installed skills/plugins management | KEEP_AS_USER_FEATURE | Rust extension/skill endpoints | Medium: ecosystem regression | UI/API lifecycle tests |
| Frontend Settings page | Yes | `ProviderSettingsPanel` | Provider config form and test action | KEEP_AS_USER_FEATURE | Rust model settings endpoint | Medium: credential UX regression | Form and redaction tests |
| Python CLI `coder` | Yes | `src/coder_workbench/cli.py` | Run Agent workflow from command line | KEEP_AS_USER_FEATURE | Rust `coder` CLI after parity | High: CLI users break | CLI smoke and mock run tests |
| Python API CLI `coder-api` | Yes | `server/cli.py` | Start current FastAPI server | KEEP_AS_USER_FEATURE | Rust `coder server` with compatibility mode | Medium: local dev break | Server startup and health tests |
| Existing unittest suite | No | `tests/` | Tests covering runtime, UI surface, memory, RAG, storage, skills | KEEP_AS_USER_FEATURE | Keep as compatibility gate until Rust parity tests replace pieces | Critical: losing regression signal | Continue running Python tests in CI |

## Required Phase 0 Conclusions

- No current user-visible feature is approved for deletion.
- OpenHands-backed harness behavior is explicitly preserved and migrates through
  an external OpenHands backend adapter.
- The workflow canvas remains a normal user feature and migrates to stable
  Agent/Harness/Workflow specs.
- Python runtime code remains available until Rust parity is proven.
- Internal names such as `AgentGraphRunner`, `HarnessRuntimeManager`,
  `SharedRunState`, `ContextPacket`, and `ArtifactProjector` are migration
  implementation details, not future user-facing concepts.

## Existing API Groups

- Health and capabilities: `/api/v2/health`, `/api/v2/capabilities`,
  `/api/v2/agent-role-cards`.
- Library and workflow editing: `/api/v2/library`,
  `/api/v2/library/agents`, `/api/v2/library/agent-workflows`,
  `/api/v2/agent-workflows/default`, `/api/v2/agent-workflows/validate`,
  `/api/v2/agent-workflows/runtime-profiles`.
- Planner Chat: `/api/v2/planner-chat/draft`,
  `/api/v2/planner-chat/confirm`, `/api/v2/planner-chat/sessions`,
  `/api/v2/planner-chat/sessions/{session_id}/turn`.
- Runs: `/api/v2/live-agent-runs`, `/api/v2/runs`,
  `/api/v2/runs/{run_id}/events`, pause/resume/cancel/heartbeat, artifact,
  context-packet, tool-result, and blob retrieval.
- Memory/RAG: `/api/v2/knowledge-sources/import-text`,
  `/api/v2/knowledge-sources`, `/api/v2/rag/reindex`,
  `/api/v2/rag/status`.
- Extensions/skills: `/api/v2/extensions/*`, `/api/v2/skills/*`.
- Patches: `/api/v2/patches/rollback`.

## Missing Rust Tests To Add Next

- `coder-config`: parse `.coder/coder.yaml` including agents, harnesses, and
  workflows.
- `coder-store`: event sequence ordering, replay, artifact traversal denial,
  blob hash stability and retrieval, large payload preview/ref behavior, and
  sanitized repo evidence refs under `runs/{run_id}/repo_evidence/`.
- `coder-workflow`: mock runner completed/blocked/failed/max-rounds cases.
- `coder-openhands`: unavailable server, classified auth/workspace failure, raw
  event normalization, redacted secrets.
- `coder-server`: `/api/v3` health, config validation, workflow validation,
  mock run, run listing, event listing, stored artifact/blob retrieval, and
  stored repo evidence payload retrieval by ref.
- `coder-memory`: project memory file loading, bounded memory write previews,
  and content-free memory read events.
- `coder-cli`: mock/OpenHands run commands plus stored run list/show
  inspection helpers.
- Frontend adapter: legacy canvas to `WorkflowSpec` plus additive Rust API v3
  helper functions for validation and stored run inspection.
- `coder-tools`: path-safe read-only repo file discovery, full-file reads,
  bounded line-range reads, bounded text search, read-only git status evidence,
  and bounded git diff previews. The first Rust slice skips runtime/vendor
  directories and sensitive repo paths. The CLI can optionally record
  find/read metadata/read-range/search/diff outputs into `coder-store` repo
  evidence refs.
- Frontend adapter: legacy canvas to `WorkflowSpec` and back.
