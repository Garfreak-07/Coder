# Rust Migration Map

This document is now a closure record for the Rust-only migration.

## Final Decision

Current `main` supports the Rust v3 product path only. The previous
Python/FastAPI v2 compatibility implementation was removed from current `main`
and preserved in git history at tag `pre-rust-only-legacy-v2`.

## Active Rust Targets

| Product area | Current Rust target | Status |
|---|---|---|
| Planner Chat | Rust session, turn, preview, and run APIs | Complete |
| Workflow canvas | React adapter to Rust `ProjectConfig` and `WorkflowSpec` | Complete |
| User agents/workflows | Rust `AgentSpec`, `HarnessSpec`, `WorkflowSpec`, and library storage | Complete |
| Native execution | Rust workflow runner and native/mock backend | Complete |
| OpenHands | External Agent Server adapter with explicit compatibility config | Complete |
| Events/store | Rust event log, metadata, artifacts, blobs, checkpoints, and repo evidence | Complete |
| Final report | Rust event/evidence-backed report builder | Complete |
| Repo tools | Rust path-safe repo tools and evidence refs | Complete |
| Command tools | Rust command preview/run and approval policy | Complete |
| Patch tools | Rust patch preview/apply and approval policy | Complete |
| Memory | Rust project memory loading and proposal events | Complete |
| Knowledge/RAG | Rust lexical, deterministic dense, and hybrid retrieval baselines | Complete |
| Extensions/skills | Rust lifecycle endpoints and policy checks | Complete |
| MCP | Rust manifest validation and mock execution baseline | Complete |
| Provider settings | Rust settings, redaction, status, and test endpoint | Complete |
| CLI/distribution | `coder-rust`, release archives, installers, npm wrapper, Homebrew template | Complete |

## Current Protocol Targets

| Protocol area | Current shape |
|---|---|
| Specs | YAML/JSON Rust `ProjectConfig`, `AgentSpec`, `HarnessSpec`, `WorkflowSpec` |
| Events | JSON events with run id, sequence, timestamp, kind, payload, and refs |
| API | `/api/v3/*` JSON and event retrieval |
| Reports | Evidence-backed Rust `FinalReport` |
| Blobs | Content-addressed blob reads by digest |

## Historical Recovery

Use the preservation tag only when the removed compatibility implementation is
needed:

```powershell
git checkout pre-rust-only-legacy-v2
```

No current deletion gate remains for Python/FastAPI v2 because it is no longer a
maintained product path in `main`.
