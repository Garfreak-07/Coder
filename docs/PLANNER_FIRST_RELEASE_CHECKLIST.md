# Planner-First Release Checklist

## Product Flow

- [x] Planner chat casual conversation works without status-only placeholder responses.
- [x] Planner chat planning works through workflow-resolved `AgentSpec` and
  `planner-model` HarnessSpec.
- [x] Planner global/project memory proposal is represented in `PlanDraft.memory_proposals`.
- [x] Planner memory proposal and confirmation APIs require `planning_chat`.
- [x] Workflow memory stays scoped to run/workflow paths for workflow agents.
- [x] Work mode executes only after PlanDraft readiness and explicit confirmation.
- [x] Final report includes event-log evidence and plan context summary/checks.

## Harness And Runtime

- [x] Planner Conversation Harness is read-only and denies file writes, command
  execution, network, secrets, publishing, commit, push, and deploy.
- [x] OpenHands executor path remains preferred when an OpenHands harness is configured.
- [x] OpenHands payload includes workflow, node, agent, harness, tools,
  permissions, memory scopes, plan context, and non-secret model refs.
- [x] Native Rust fallback works without OpenHands for deterministic CI,
  preflight, approvals, and evidence capture.
- [x] Native Rust fallback is not a duplicate OpenHands terminal/file/task loop.

## React UX

- [x] React displays Planner transcript, plan draft/readiness, open questions,
  acceptance criteria, risks, memory proposals, run events, evidence, and final report.
- [x] React sends the selected workflow config into Planner Chat so backend
  readiness and harness policy remain server-owned.
- [x] React frontend tests cover planner harness export, Work config handoff,
  memory proposal display surface, run event mapping, and final report surfaces.

## Release Gates

- [x] `cargo test --workspace`
- [x] `frontend: npm.cmd run test`
- [x] `frontend: npm.cmd run build`
- [x] `cargo fmt --all --check`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `frontend: npm.cmd ci`
- [x] `powershell -ExecutionPolicy Bypass -File .\scripts\smoke-rust-v3.ps1 -Store .tmp\smoke-rust-v3`
- [x] `powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -DryRun`
- [x] `node packaging/npm/bin/coder-rust.js --dry-run`
- [ ] `bash ./scripts/install.sh --dry-run` local bash unavailable on this Windows host; use CI POSIX installer dry-run
