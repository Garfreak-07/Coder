# Rust Migration Map

This document maps current Python/React subsystems to Rust-first equivalents.
It complements `docs/current-feature-inventory.md` and defines deletion gates:
no Python subsystem may be removed or moved to `legacy-python/` until its user
behavior is covered by a Rust equivalent and tests listed here.

## Migration Phases

| Phase | Goal | Must not do |
|---|---|---|
| Phase 0 | Freeze current capabilities and mappings | Delete Python, remove UI features, rewrite frontend, change license |
| Phase 1 | Stabilize Rust workspace and specs | Replace current product path |
| Phase 2 | Add event log/blob store parity | Store secrets or large payloads inline |
| Phase 3 | Add mock workflow runner | Depend on OpenHands availability |
| Phase 4 | Add OpenHands backend spike | Embed Python SDK in Rust |
| Phase 5 | Add Rust API v3 and frontend adapter | Remove v2 endpoints prematurely |
| Phase 6 | Migrate real workflows behind flags | Drop Python fallback before parity |
| Phase 7 | Quarantine/delete legacy internals | Remove any user-visible behavior silently |
| Separate | MIT license migration after ownership confirmation | Combine license churn with runtime rewrites |

## Current Phase 11 Decision

Python is not physically moved to `legacy-python/` in this checkpoint. The v2
Python/FastAPI tree remains the default compatibility path while the React app
can opt into Rust API v3 with `VITE_CODER_API_VERSION=v3`,
`CODER_USE_RUST_API=1`, `?coder_api_version=v3`, or local storage key
`coder_api_version=v3`.

This is an intentional quarantine gate, not a deletion. Moving the Python tree
now would break the current v2 fallback, Python compatibility tests, and local
developer entrypoints before the frontend default has switched to v3. Physical
quarantine is allowed only after:

- the React default path is Rust v3,
- Planner Chat and run execution no longer depend on v2 live-run streams,
- Python compatibility tests have Rust/frontend replacements or are retired,
- CI is green with the Python package no longer on the default product path.

Until then, Python is treated as legacy compatibility code: kept buildable,
tested in CI, and excluded from new Rust control-plane ownership.

## Subsystem Mapping

| Current subsystem | Current files | Rust target | Migration action | Deletion gate |
|---|---|---|---|---|
| Planner Chat sessions | `server/planner_chat_sessions.py`, `server/app.py`, planner chat frontend | `coder-core` run/session types, `coder-server` API, `coder-agent` prompt rendering | Rust v3 session/turn endpoints and a gated frontend adapter cover baseline discuss/work readiness; v2 remains the compatibility fallback for richer live session behavior | Discuss/work behavior has parity tests and UI uses Rust API by default |
| Legacy draft/confirm | `planner-chat/draft`, `planner-chat/confirm` | Rust run preview and confirmation gate | Frontend v3 adapter maps draft/confirm to `/api/v3/runs/preview` and `/api/v3/runs`; v2 remains fallback | Existing frontend no longer depends on legacy draft payloads |
| Agent workflow model | `core/agent_workflow.py`, `frontend/src/types.ts` | `coder-config::AgentSpec`, `HarnessSpec`, `WorkflowSpec`, UI protocol DTOs | Add adapters between legacy JSON and specs | Roundtrip preserves layout, max rounds, agents, harness bindings |
| Workflow canvas | `frontend/src/features/agent-workflow`, `workflowGraph.ts` | Frontend adapter plus Rust validation API | Validate spec through Rust when available, keep current UI simple | Browser/UI tests prove save/import/export parity |
| Agent role cards | `core/archetypes.py` | Agent templates in `coder-config`/`coder-agent` | Generate role catalog from stable specs | UI no longer needs Python role card endpoint |
| Harness profiles | `harness_runtime/profiles.py`, `contracts.py` | `coder-harness` and `HarnessSpec` | Translate current profile IDs into user-facing work modes | All canonical profiles map to HarnessSpec examples/tests |
| Runtime provider selection | `HarnessRuntimeManager` | Backend registry | Keep manager until Rust registry dispatches mock/native/OpenHands | Rust dispatch covers OpenHands enabled/unavailable/fallback cases |
| OpenHands provider | `openhands_provider.py`, `openhands_tools` | `coder-openhands` | Implement external Agent Server health, send, stream, normalize | Real or simulated OpenHands events stored as Coder JSONL |
| Internal fallback | `fallback_provider.py` | Mock/native Rust backend | Implement mock workflow runner and later native tools | Python fallback unused by default and tests pass on Rust mock/native |
| ActionGateway/tool execution | `actions/*` | `coder-tools`, `coder-sandbox`, Rust tool gateway | Ported command preview plus patch preview/apply API paths with side-effect eventing; Rust `coder-harness` and v3 API now mirror tool registry filtering, plugin operation approval policy, and MCP manifest validation | Patch/command/plugin/MCP policy tests pass in Rust |
| Patch pipeline | `coding/patch_*` | Rust patch tool with artifact refs | Added path-safe patch preview plus approval-gated patch apply with check-before-apply and patch lifecycle events | Rollback and scope safety tests pass |
| Command checks | `coding/command_*` | Rust command runner | Started with policy-gated argv runner, side-effect-free API approval preview, cwd scoping, timeout, bounded output, and CLI event recording; add richer report integration next | Command tests pass and approval events emitted |
| Event model | `agent_graph/events.py`, run events stores | `coder-events` | Promote canonical JSONL event envelope with sequence IDs | Replay/listing tests cover current live/stored events |
| Run storage | `server/storage.py`, `server/stores/*` | `coder-store` | Metadata/events/artifacts/blobs/repo-evidence plus JSON checkpoint read/write/list are in Rust store/API | Stored run listing/detail, artifact, blob, repo-evidence, and checkpoint APIs read Rust-created records |
| Run control | `server/agent_manager.py`, live run APIs | `coder-server` run-control API plus future cancellation tokens | Added v3 pause/resume/cancel/heartbeat endpoints with control events; cancel writes cancelled report | Pause/resume/cancel/heartbeat tests pass |
| Blob/artifact stores | `core/artifacts.py`, stores | `coder-store` | Add content hash refs and traversal-safe reads | Large payload and secret redaction tests pass |
| Final reports | `agent_graph/final_report.py` | `coder-core::FinalReport` plus report builder | Event/repo-evidence-backed report preview/write for Rust runs now includes command checks, blockers, patch-preview and patch-apply changed files, and patch refs | Completed/blocked/failed/cancelled report tests pass |
| Runtime state views | `runtime_state/*` | Event replay plus UI projections | Replace internal cache exposure with bounded DTOs | UI/debug exports have equivalent data |
| Context packets | `context/*`, context stores | Rust context assembler | Preserve hot/warm/cold behavior behind public refs | Token/large-payload tests pass |
| Repo evidence tools | `context/repo_*`, Git inspection helpers | `coder-tools` repo search/read/status/diff/find plus `coder-store` repo evidence refs | Port path safety, sensitive-path filtering, bounded line-range reads, bounded previews, and evidence write behavior; wire CLI tools to optional repo evidence recording | Existing repo_search/read/discovery/status/diff and evidence-store tests have Rust equivalents |
| Agentic router | `context/agentic_router.py` | Later Rust context router | Defer until repo tools and memory are stable | Router policy tests pass in Rust |
| Memory service | `memory/service.py`, `run_memory.py` | `coder-memory` | Project memory JSON loading, bounded `memory.read` recording, and event-only `memory.write.proposed` proposals are exposed through v3 | Long-term writes are confirmed and executor cannot write directly |
| Knowledge import/RAG | `memory/knowledge_import.py`, `hybrid_*` | `coder-memory`, `coder-rag` | Port lexical retrieval first, dense optional later | ACL and hint-only behavior covered |
| Extensions/plugins | `extensions/*` | `coder-extensions` plugin registry | Rust plugin/harness-runtime manifest types, builtin plugin manifests, external-effect preview validation, extension search, installed list, and v3 list/validate API are in place; execution remains approval-gated/deferred | External-effect approval and manifest tests pass |
| Skills | `skills/*` | Rust skill store/router | Rust v3 covers installed/discover/updates/install/update/auto-update/enable/disable/remove/pin/unpin/rollback/update-policy; unsafe developer import is denied in the baseline | Skill lifecycle tests pass |
| MCP | `tools/mcp.py`, registries | Rust MCP registry/server/client | Rust `coder-harness` and v3 API now validate MCP manifests, force server/operation default enablement off, and keep operations approval-required; execution/client support remains later | Manifest validation and no-auto-enable tests pass |
| Provider settings | `server/settings.py`, frontend settings | `coder-model` profiles and Rust settings API | Keep secret refs only, redact values | Provider status/test behavior has Rust parity |
| Python CLI | `cli.py` | `coder-cli` | Add Rust commands while keeping Python CLI | Rust CLI can run and inspect mock/OpenHands spike workflows |
| FastAPI server | `server/app.py` | `coder-server` Axum API v3 | Added v3 health, validation, workflow/library, planner-chat baseline, run/event/report/store reads, tools, memory, extensions/skills/MCP, provider settings, command preview/run, and patch preview/apply endpoints; preserve v2 until frontend default migrates | Frontend can run against Rust server for main flow |
| React app | `frontend/src` | Same app with Rust API adapter | Gated v3 adapter covers workflow/library, run inspection, report/artifact/blob retrieval, provider settings, skills/extensions, Planner Chat baseline, and run preview/confirmation while preserving v2 fallback | Main pages validated through browser smoke |
| Tests | `tests/`, frontend build, future Cargo tests | Multi-language CI gates | Keep Python tests until parity tests replace them | Equivalent Rust/frontend tests exist before deleting Python tests |

## Stable Public Protocol Targets

| Protocol area | First Rust shape | Notes |
|---|---|---|
| Specs | YAML/JSON `AgentSpec`, `HarnessSpec`, `WorkflowSpec` | Human-editable, versioned |
| Events | JSONL with event id, run id, sequence, timestamp, kind, payload, refs | Large payloads by artifact/blob ref |
| API | `/api/v3/*` JSON and SSE/WebSocket event stream | Keep v2 until frontend migration |
| Debug export | JSON/JSONL/Markdown | No binary-only format |
| Blobs | raw bytes by content hash | Used for logs, diffs, screenshots, long text |

## OpenHands Migration Gates

OpenHands support is considered preserved only when Rust can:

1. Read external server config.
2. Health-check the OpenHands Agent Server.
3. Surface server unavailable/auth/workspace errors clearly.
4. Send a user task or attach to an existing conversation where supported.
5. Stream or fetch OpenHands events.
6. Normalize events into Coder JSONL.
7. Preserve raw event refs for debug export.
8. Keep secrets out of events and artifacts.

`coder-openhands` defaults must follow the documented Agent Server contract:
`/conversations`, `/conversations/{conversation_id}/events`, websocket
`/conversations/{conversation_id}/events/socket`, and
`Authorization: Bearer <session key>`. Compatibility with older SDK-style
paths such as `/api/conversations`, `/events/search`, `/run`,
`/sockets/events/{conversation_id}`, and `X-Session-API-Key` is allowed only
through explicit `openhands.api_paths` and `openhands.run_start_strategy`
configuration.

Until then, current Python OpenHands paths remain available.

## Canvas Migration Gates

The workflow canvas can move to Rust-backed specs only when:

1. Existing Planner -> Executor graph imports into `WorkflowSpec`.
2. Exported `WorkflowSpec` imports back without losing positions or max rounds.
3. Save As collision behavior is preserved.
4. Runtime internals remain hidden from ordinary UI.
5. Validation errors are shown in user-facing terms.

## Deletion Rules

- Delete internal concepts only, not capabilities.
- Do not delete Python code in the same change that introduces an unproven Rust
  equivalent.
- Do not remove OpenHands, canvas, custom agents/workflows/harnesses, memory,
  evidence, or run control.
- If a subsystem is ambiguous, mark it `UNKNOWN_NEEDS_INVESTIGATION` and keep it.
