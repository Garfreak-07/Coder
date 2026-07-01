# OpenHands Integration

OpenHands is the required backend for Start Work executor tool loops in the
default product workflow. Coder owns the workflow boundary around that backend:

- build the OpenHands conversation payload from Coder workflow, agent, harness,
  model, permission, memory, verification, and plan context
- trigger or attach to an OpenHands conversation
- poll or stream OpenHands events until a terminal status or timeout
- store raw OpenHands payloads as blob refs
- publish normalized public Coder events for the Start Work timeline
- produce an evidence-backed final report

Planner Chat does not currently run through OpenHands. The decision and future
adapter boundary are recorded in `docs/OPENHANDS_PLANNER_REUSE_DECISION.md`.

## Settings UI

Normal users configure OpenHands from Settings under
`Execution Backend / OpenHands`. Editing `examples/coder.yaml` is only a
developer/headless fallback.

The server exposes:

```text
GET  /api/v3/openhands/settings
POST /api/v3/openhands/settings
GET  /api/v3/openhands/status
```

Settings include:

- `server_url`, defaulting to `http://127.0.0.1:8000`
- masked `session_api_key`
- `workspace_mode`, currently `local` or `ephemeral`

OpenHands is always enabled for Start Work. The settings API keeps legacy
`enabled` and `allow_native_fallback` fields for response compatibility, but
the server forces `enabled=true` and `allow_native_fallback=false`. Normal users
should not see controls that disable OpenHands or route work to native fallback.

The session key is stored only in the Rust server's in-memory settings or read
from `OPENHANDS_SESSION_API_KEY` as a headless fallback. Settings responses
return only whether a key is configured and its source; they do not return the
plaintext key.

The Test OpenHands action performs a direct `GET /health` request with proxy
bypass, so local OpenHands agent servers are not accidentally routed through a
system proxy.

When an `openhands` executor harness runs, Coder emits a public
`backend.selected` event before execution. If OpenHands is not reachable, Coder
emits `backend.blocked`. Native Rust fallback is not used for product Start
Work. The run remains blocked, the timeline shows `Executor backend: blocked -
OpenHands not reachable`, and Planner/Start Work returns a user-facing message
that OpenHands is required and what should be checked.

## Raw Events

Raw OpenHands event payloads are persisted through `RunStore` large-text blob
refs. Public events carry `raw_ref` and an `openhands.raw_event` ref, but the
ordinary timeline does not include the raw JSON payload by default.

This keeps secret-like OpenHands fields and backend-specific event shapes out of
the normal chat and timeline UI while still preserving replay/debug evidence.

## Public ReAct Events

`OpenHandsHarnessBackend` maps raw events into the same public executor event
contract used by deterministic executor tests:

- `executor.reasoning_summary`
- `executor.action_selected`
- `tool.started`
- `tool.completed`
- `observation.recorded`
- `executor.next_step`
- `executor.completed`
- `executor.blocked`
- `executor.failed`

Each public event includes workflow, node, agent, harness, backend, step, and
summary/status fields. The RunStore event envelope remains the source of the
canonical event timestamp.

## Timeline Projection

OpenHands command-shaped events also emit command timeline events:

- `command.previewed` for selected shell or terminal actions before result data
- `command.completed` for command observations with successful result data
- `command.failed` for non-zero, failed, timeout, or error statuses

OpenHands file or patch-shaped events emit `patch.applied` with a sanitized
`files` list. The server timeline projector turns these into public
`command_execution` and `file_change` items.

Backend selection events are projected as public `executor_step` items:

- `Executor backend: OpenHands`
- `Executor backend: native fallback` for legacy/test-only traces, not product
  Start Work
- `Executor backend: blocked - OpenHands not reachable`

## Test Boundary

Normal CI uses fake OpenHands event shapes and does not require a live
OpenHands server. Live compatibility checks should remain opt-in because they
need external services and credentials.

## Optional Live Smoke

Use `scripts/live-openhands-smoke.ps1` only when a real OpenHands server is
available. The script is gated by `OPENHANDS_LIVE_SMOKE=1`; without that flag it
can report `skipped` and will not contact OpenHands.

```powershell
$env:OPENHANDS_LIVE_SMOKE="1"
$env:OPENHANDS_AGENT_SERVER_URL="http://127.0.0.1:8000"
$env:OPENHANDS_SESSION_API_KEY="..."
powershell -ExecutionPolicy Bypass -File .\scripts\live-openhands-smoke.ps1
```

The smoke creates a temporary git repository under `.tmp/`, runs a
documentation-only task through the configured `openhands` harness, and checks:

- OpenHands server health
- `backend.selected` recorded with `backend=openhands`
- `docs/OPENHANDS_LIVE_SMOKE_RESULT.md` updated in the temporary repo
- timeline API includes `Executor backend: OpenHands`
- public ReAct timeline events such as executor, tool, command, or patch events
- raw OpenHands event refs in the run store
- final report preview and summary
- Review Changes includes the OpenHands result-doc change
- Undo succeeds or safely reports a supported conflict/unsupported state for that
  change
- no configured API key appears in events, reports, timeline, or changes output

The live smoke must finish with `Status: completed` and a `run.completed` event.
Editing the result document is not enough if the final report is `blocked` or
`failed`.

The current live smoke intentionally requires a documentation-only file edit so
Review Changes and Undo are exercised. CI must keep using fake adapter tests and
must not require this live smoke.

## Full Path Live Smoke

Use `scripts/live-full-path-smoke.ps1` when the full Planner Chat to OpenHands
path needs live validation. This script exercises the Coder API surface instead
of calling the workflow runner directly:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\live-full-path-smoke.ps1 -Live -LoadLocalEnv -WorkRoot .tmp\live-full-path-smoke -OpenHandsServerUrl http://127.0.0.1:8000 -OpenHandsSessionApiKey "..."
```

The script loads local provider credentials only when `-LoadLocalEnv` is passed.
It reads the provider proxy from `HTTPS_PROXY` or `HTTP_PROXY`, sends that proxy
through Provider Settings for the selected provider, and forces
`NO_PROXY=127.0.0.1,localhost,::1` for local Coder/OpenHands traffic.

The live task is documentation-only and explicitly tells OpenHands not to
commit, push, publish, or clean the working tree. The result document must be
left as an uncommitted diff so Review Changes and Undo can be verified through
Coder. If OpenHands commits the change, the script fails with a clear error.

The script verifies:

- live provider test mode for the configured provider
- OpenHands settings connectivity
- two Planner Chat turns retained in the session
- Start Work completes through the configured OpenHands executor
- Timeline contains backend, public ReAct, and final summary items
- final report preview is completed
- `docs/FULL_PATH_SMOKE_RESULT.md` is updated and left uncommitted
- Review Changes returns a diff for the updated document
- Undo succeeds or safely reports a supported state
- serialized API artifacts and stored report/event files do not contain the
  configured provider or OpenHands session keys

## Local Live Compatibility Record

Latest validated local run:

```text
timestamp: 2026-07-01 20:47:45 +08:00
base_commit_before_record: 27ab5509
OpenHands Agent Server: http://127.0.0.1:8000
provider: DeepSeek via OpenAI-compatible API
model: deepseek-v4-flash
run_id: 2718536d-950b-4415-970d-20f50844ecf2
result: status ok, final report completed, run.completed recorded
timeline: 77 items, 64 public ReAct items
review/undo: 1 Review Changes entry, undo_status undone
secrets_check: passed
```

Compatibility details from that run:

- Coder sends `workspace.kind=LocalWorkspace` and the local repo `working_dir`
  in the conversation payload.
- Coder sends Agent Canvas `kind=Agent` with `terminal`, `file_editor`, and
  `task_tracker` tool names.
- OpenAI-compatible DeepSeek settings are passed to OpenHands through the agent
  `llm` payload from environment variables. API keys are not written to Coder
  metadata or docs.
- OpenHands `finish` tool events are normalized to `executor.completed`.
- The smoke workflow is single-round so an executor completion ends the run.
- The smoke uses `max_events: 100`; this avoids a local OpenHands
  `/events/search?limit=200` HTTP 500 observed during validation.

Latest validated full path run:

```text
timestamp: 2026-07-01 23:27:48 +08:00
OpenHands Agent Server: http://127.0.0.1:8000
provider: deepseek
model: deepseek-v4-flash
session_id: pcs_c7bc9624-ea17-4bd1-8c0b-c8c280a4d445
run_id: b4a4b08a-eea9-4409-9e9f-4604add1266f
result: status ok, Start Work completed, final report completed
timeline: 199 items, 137 public ReAct items, 1 final summary
review/undo: 1 Review Changes entry, undo_status undone
secrets_check: passed
```
