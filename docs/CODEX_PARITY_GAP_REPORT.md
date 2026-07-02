# Codex Parity Gap Report

## Mirrored Codex Behavior

- Coder is Codex split into Planner and Executor roles.
- Planner Chat is side-effect free; chat turns cannot start execution.
- Planner Chat is LLM-backed in product mode and returns setup-required text
  instead of pretending to plan when provider credentials are missing.
- Product-mode provider settings default to live mode; mock mode is explicit
  CI/developer plumbing, not normal user behavior.
- Start Work is an explicit execution action.
- Executor work follows a public Reason -> Act -> Observe lifecycle through a
  harness; OpenHands is preferred when configured.
- Run events are projected into user-facing timeline items instead of raw JSON.
- Timeline items cover plan updates, executor steps, tools, commands, file
  changes, approvals, verification, and final summary.
- Review Changes exposes changed files, diff, checks, accept, and undo.
- Generated browser apps require browser-level behavior validation; syntax and
  file-creation checks are not enough for user-visible confidence.

## Removed Coder Behavior

- Main Planner UI no longer shows a separate planning screen.
- Main Planner UI no longer exposes a separate execution-mode toggle.
- Chat input is no longer disabled by internal plan state.
- Structured plan details are no longer rendered as the default chat message card.
- Raw run event replay is hidden behind debug UI.
- Plugins & Skills marketplace UI is hidden from core navigation and remains a
  developer/debug surface.
- Obsolete Python/FastAPI compatibility remnants are absent from the main path;
  the audit is recorded in `docs/PYTHON_COMPATIBILITY_REMNANTS_AUDIT.md`.

## Still Different From Codex

- Coder keeps a user-editable workflow canvas, but it is now an
  Advanced -> Developer -> Workflow editor surface rather than the normal
  starting point.
- Coder stores local run state through `RunStore`, repo evidence, artifacts,
  blobs, and checkpoints rather than Codex session JSONL names.
- Plugin/skill marketplace support is retained only as a local developer/debug
  surface in this milestone.
- Review/undo is conservative: undo requires current diff to match the recorded
  review diff.
- Mock tests prove deterministic plumbing only. Optional live LLM smoke is
  required for confidence in the real product provider path.
- Earlier Snake E2E verified file creation and JS syntax only. It did not
  validate playable browser behavior. Browser interaction validation is now
  required.

## Intentionally Different

Coder splits Codex into Planner and Executor:

- Planner talks to the user and owns public summaries.
- Executor acts through harnesses and never chats directly with the user.
- Harness specs remain the tool, permission, memory, and backend boundary.
- External capability boundaries are tracked in
  `docs/CAPABILITY_BOUNDARY_MATRIX.md`; registered tools declare permission,
  approval, evidence, and timeline behavior.
- OpenHands stays the preferred executor backend when configured.
- Environment variables remain developer/headless fallback; normal users use
  Provider Settings.
- Planner Chat stays on Coder's direct provider path in this phase; the
  OpenHands reuse decision is recorded in
  `docs/OPENHANDS_PLANNER_REUSE_DECISION.md`.
- GPU support is optional future provider capability, not core runtime.

## Non-Blocking

- Remote plugin sharing and public marketplace publishing.
- Live OpenHands matrix gates.
- Streaming planner deltas.
- GPU-backed local semantic index or local model acceleration.
