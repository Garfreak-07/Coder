# Current Feature Inventory

This inventory describes current Rust-only `main`.

## Product Surface

| Area | Current implementation | Status |
|---|---|---|
| Planner Chat | React Planner Chat page backed by Rust API v3 sessions, readiness, preview, and run start | Active |
| Workflow canvas | React Agent Workflow editor with Rust workflow import/export and validation adapters | Active |
| User-defined agents and workflows | `AgentWorkflowSpec` UI model mapped to Rust `ProjectConfig`, `AgentSpec`, `HarnessSpec`, and `WorkflowSpec` | Active |
| Native Rust backend | Rust workflow runner, native/mock execution, tool registry, and report builder | Active |
| OpenHands backend | Optional external backend through `coder-openhands` | Active |
| Run storage | Rust metadata, event, artifact, blob, checkpoint, and repo-evidence stores | Active |
| Run controls | Rust pause, resume, cancel, heartbeat, listing, detail, event, and report endpoints | Active |
| Final reports | Rust `FinalReport` from event and evidence refs | Active |
| Repo tools | Rust find/search/read/range/status/diff helpers with path safety and evidence refs | Active |
| Command tools | Rust command preview/run with approval policy and bounded output | Active |
| Patch tools | Rust patch preview/apply with approval policy and evidence refs | Active |
| Memory | Rust project memory load and write proposal events | Active |
| Knowledge/RAG | Rust text import plus lexical, deterministic dense, and hybrid retrieval baselines | Active |
| Skills/extensions | Rust installed/discover/update/install/enable/disable/remove/pin/unpin/rollback/update-policy APIs | Active |
| MCP | Rust manifest validation, deny-by-default registry, and mock execution baseline | Active |
| Provider settings | Rust provider settings, redaction, status, and test endpoint | Active |
| CLI | `coder-rust` doctor, config, workflow, run, server, OpenHands, tools, and evidence commands | Active |
| Release/install | GitHub release workflow, PowerShell/POSIX installers, npm wrapper, and Homebrew template | Active |

## API Surface

Current frontend product calls use `/api/v3/*` endpoints. The React code no
longer contains runtime switching to a removed v2 backend.

Primary Rust API groups:

- health, capabilities, and role cards
- workflow validation, default workflow, and workflow library storage
- Planner Chat sessions and turns
- run preview, start, list, detail, events, controls, reports, artifacts,
  checkpoints, blobs, and repo evidence
- repo, command, patch, memory, knowledge, MCP, extensions, skills, and provider
  settings endpoints

## Historical Note

The previous Python/FastAPI v2 compatibility implementation was removed from
current `main`. It remains available in git history at tag
`pre-rust-only-legacy-v2`.

Current `main` does not maintain a Python/FastAPI v2 package, CI job, or
frontend API selector.
