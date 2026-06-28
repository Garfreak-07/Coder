# Rust Full Completion Blockers

This file records the remaining blockers after the Rust v3 default-path switch.
The repository is buildable and CI is green at the time these blockers were
recorded.

## 1. MIT License Migration Requires Approval

Exact blocker:

- The directive requires MIT migration, but also requires explicit repository
  owner approval and contributor/copyright acceptability before changing the
  license.

Files affected:

- `LICENSE`
- `Cargo.toml`
- crate package metadata under `crates/*/Cargo.toml`, if any license fields are
  added later
- `pyproject.toml`
- `README.md`
- docs mentioning the previous license

Commands run:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `.\.venv\Scripts\python.exe -m unittest discover -s tests`
- `.\.venv\Scripts\python.exe -m compileall src tests`
- `cd frontend; npm.cmd run test`
- `cd frontend; npm.cmd run build`
- `powershell -ExecutionPolicy Bypass -File .\scripts\smoke-rust-v3.ps1 -Store .tmp\smoke-rust-v3`

Failing output summary:

- No failing output. This is an approval/legal blocker, not a build failure.

Minimal next patch required:

- After owner/contributor approval, make a license-only commit that replaces
  previous license metadata and `LICENSE` with MIT text.

User approval needed:

- Yes.

## 2. Python Physical Quarantine Is Not Yet Performed

Exact blocker:

- Rust v3 is the default product path and Python is explicitly legacy, but the
  Python tree has not been physically moved to `legacy-python/`.
- Moving it now would require import/package/test path rewrites and may break
  older `/api/v2/*` clients without a separate compatibility packaging pass.

Files affected:

- `src/coder_workbench/**`
- `tests/**`
- `pyproject.toml`
- `.github/workflows/ci.yml`
- `README.md`
- `docs/legacy-python.md`

Commands run:

- Same verification commands listed in blocker 1.
- GitHub CI run `28328633545` passed with jobs:
  `Legacy Python compatibility (3.12)`, `Rust workspace`, and
  `Frontend build`.

Failing output summary:

- No failing output. The blocker is migration risk and remaining compatibility
  contract, not a current test failure.

Minimal next patch required:

- Either move Python into `legacy-python/` with updated packaging/test roots, or
  split CI into explicit Rust/default and legacy Python package jobs while
  preserving `/api/v2/*` fallback docs.

User approval needed:

- No for CI/package restructuring, unless users still depend on root-level
  Python package entrypoints.

## 3. MCP Execution Is Explicit Baseline-Limited

Exact blocker:

- Rust validates MCP manifests and keeps enablement deny-by-default, but full
  MCP client/tool execution is not claimed as complete.

Files affected:

- `crates/coder-harness/**`
- `crates/coder-server/**`
- `src/coder_workbench/tools/mcp.py`
- docs describing MCP capability

Commands run:

- Same verification commands listed in blocker 1.

Failing output summary:

- No failing output. Tests cover the current disabled/approval-required
  baseline.

Minimal next patch required:

- Add a local mock MCP server/client execution path, record approval/evidence
  events, and add Rust server/frontend tests for invocation.

User approval needed:

- No.

## 4. Dense RAG Remains Optional

Exact blocker:

- Rust memory/knowledge supports text import and lexical retrieval. Dense RAG is
  still optional and must not be represented as required for normal CI.

Files affected:

- `crates/coder-memory/**`
- `docs/hybrid_rag_tool.md`
- memory/RAG frontend surfaces, if dense controls become user-facing

Commands run:

- Same verification commands listed in blocker 1.

Failing output summary:

- No failing output. Lexical retrieval is green; dense retrieval remains a
  feature-gated enhancement.

Minimal next patch required:

- Add a `RetrievalBackend` dense implementation behind feature/config with
  mocked tests and no live embedding requirement in CI.

User approval needed:

- No.

## 5. Package Manager Installers Are Deferred

Exact blocker:

- Rust CLI distribution baseline exists, but package manager installers are not
  implemented.

Files affected:

- `docs/distribution.md`
- future release workflow files
- future installer scripts/packages

Commands run:

- Same verification commands listed in blocker 1.

Failing output summary:

- No failing output. Release artifact plan is documented; installer polish is
  not implemented.

Minimal next patch required:

- Add release workflow artifacts and installer scripts/packages, then test
  archive contents on supported platforms.

User approval needed:

- No, unless package naming or publishing ownership is constrained.
