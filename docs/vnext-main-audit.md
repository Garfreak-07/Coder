# Coder vNext Main Audit

This document records historical vNext planning context. It is not the current
product baseline.

Current `main` is Rust-only. The active baseline is documented in:

- `README.md`
- `docs/current-feature-inventory.md`
- `docs/rust-migration-map.md`
- `docs/RUST_FULL_COMPLETION_REPORT.md`
- `docs/RUST_FULL_COMPLETION_BLOCKERS.md`

The older planning phase targeted the same product invariants that remain
active today:

- Planner-led workflows
- user interaction through Planner Chat
- executor boundaries that prevent direct user contact, publishing, deploying,
  committing, pushing, or direct long-term memory writes
- evidence-backed reports
- ordinary UI that hides internal runtime details
- OpenHands preserved as an optional external backend
- user-defined agents, workflows, harness bindings, provider settings, memory,
  knowledge, MCP, and release/install tooling preserved

Historical implementation details from the pre-Rust-only phase are available in
git history at tag `pre-rust-only-legacy-v2`.
