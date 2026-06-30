# Planner-First Release Checklist

## Current Status

- Complete for the local release gate: Planner Chat, Planner-only long-term
  memory, explicit Start Work, plan-context propagation, timeline projection,
  review/undo, local Plugins & Skills surface, OpenHands payload projection,
  native fallback smoke, React build/tests, and packaging dry-runs.
- Live OpenHands execution is environment-gated and non-blocking for this gate.
- Real provider credentials are environment-gated and non-blocking for CI; in
  product mode missing provider configuration returns a clear configuration
  error.

## Product Flow

- [x] Planner chat casual conversation works without status-only placeholder responses.
- [x] Planner chat planning works through workflow-resolved `AgentSpec` and
  `planner-model` HarnessSpec.
- [x] Planner global/project memory proposal is represented in `PlanDraft.memory_proposals`.
- [x] Planner memory read, proposal, and confirmation APIs require `planning_chat`.
- [x] Workflow memory stays scoped to run/workflow paths for workflow agents and
  execution harnesses.
- [x] Chat turns never start execution.
- [x] Start Work executes only after PlanDraft readiness and explicit user action.
- [x] Final report includes event-log evidence and plan context summary/checks.
- [x] Timeline endpoint projects public command, tool, file, approval,
  verification, and final summary items.
- [x] Changeset review exposes diff, accept, and conservative undo.

## Harness And Runtime

- [x] Planner Conversation Harness is read-only and denies file writes, command
  execution, network, secrets, publishing, commit, push, and deploy.
- [x] OpenHands executor path remains preferred when an OpenHands harness is configured.
- [x] OpenHands payload includes workflow, node, agent, harness, tools,
  permissions, memory scopes, plan context, and non-secret model refs.
- [x] Native Rust fallback works without OpenHands for deterministic CI,
  preflight, approvals, and evidence capture.
- [x] Native Rust fallback is not a duplicate OpenHands terminal/file/task loop.

## Non-Blocking

- [x] Non-blocking enhancement list explicitly tracks production embedding
  providers, published npm/Homebrew channels, signed artifacts/checksums,
  richer MCP compatibility, and live OpenHands matrix work.
- [x] Avatar, voice, Live2D, VTuber, companion-persona, game-loop, and
  unrelated multimedia systems are not Coder core release items.

## React UX

- [x] React displays Planner transcript, Start Work, Codex-style timeline,
  Review Changes, and final summary.
- [x] React sends the selected workflow config into Planner Chat so backend
  readiness and harness policy remain server-owned.
- [x] React frontend tests/build cover planner harness export, Start Work
  config handoff, execution memory scopes, run timeline mapping, review changes,
  and final report surfaces.

## Release Gates

- [x] `cargo test --workspace`
- [x] `frontend: npm.cmd run test`
- [x] `frontend: npm.cmd run build`
- [x] `cargo fmt --all --check`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `frontend: npm.cmd ci`
- [x] `powershell -ExecutionPolicy Bypass -File .\scripts\smoke-rust-v3.ps1 -Store .tmp\smoke-rust-v3`
- [x] `powershell -ExecutionPolicy Bypass -File .\scripts\live-llm-smoke.ps1 -SkipIfMissingProvider`
  is opt-in through `CODER_LIVE_LLM_SMOKE=1`; without live credentials it
  reports `skipped` and does not call a paid provider.
- [x] Local Planner loop API smoke against `coder-rust server`: Planner Chat,
  Start Work, run events, and report preview plan-context check.
- [x] `powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -DryRun`
- [x] `node packaging/npm/bin/coder-rust.js --dry-run`
- [x] `bash ./scripts/install.sh --dry-run` covered by Ubuntu `installer-dry-run` CI; local bash unavailable on this Windows host
