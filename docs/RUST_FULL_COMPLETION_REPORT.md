# Rust Full Completion Report

## Final Status

- Rust-only main: COMPLETE
- Python/FastAPI v2 removed from main: COMPLETE
- Legacy fallback preserved only by git tag pre-rust-only-legacy-v2: COMPLETE
- Frontend v2 API switching removed: COMPLETE
- Legacy Python CI removed: COMPLETE
- Rust v3 control plane: COMPLETE
- React frontend for Rust v3: COMPLETE
- MCP execution baseline: COMPLETE
- Memory/knowledge/RAG baseline: COMPLETE
- Planner Conversation Harness and memory policy: COMPLETE
- OpenHands-first executor boundary: COMPLETE
- Release/installer baseline: COMPLETE
- MIT migration: COMPLETE
- Normal CI without live provider credentials: COMPLETE

## Preservation Point

The previous Python/FastAPI v2 compatibility implementation was removed from
current `main`. It remains available in git history at tag
`pre-rust-only-legacy-v2`, which points to:

```text
20e85853413f51d01db745ccd262f9b543196ecb
```

## Current Product Path

The current product path is:

```text
React frontend
-> Rust API v3
-> Planner Chat using planner AgentSpec + planner-model HarnessSpec
-> Rust workflow runner
-> planner-model supervisor boundary, OpenHands executor, or native Rust fallback
-> Rust event/evidence/report stores
```

The frontend now calls Rust API v3 directly. It no longer has runtime switches
for selecting a removed v2 backend.

## Completed Work

- Removed the tracked `legacy-python/` implementation from `main`.
- Removed the legacy Python compatibility job from CI.
- Removed frontend API version selection code and v2 endpoint fallbacks.
- Added `scripts/check-rust-only-main.js` to prevent the removed path from
  silently returning.
- Updated README and migration documents to describe current `main` as
  Rust-only.
- Added explicit `planner-conversation` / `planner-model` harness resolution for
  Planner Chat.
- Added PlanDraft memory proposals and Planner-only long-term memory
  proposal/confirmation policy.
- Kept executor code-edit workflows OpenHands-first while preserving native Rust
  fallback/preflight/evidence tooling.
- Added Planner events, Work confirmation gating, and final report plan context
  summaries.

## CI State

Normal CI has explicit jobs for:

- Rust workspace
- Frontend build/test
- Installer dry-run
- Planner memory/OpenHands-first product loop

No normal CI job installs or tests the removed Python/FastAPI v2 implementation.
No normal CI job requires DeepSeek, OpenAI, Anthropic, Gemini, OpenHands live
server credentials, external embedding service credentials, npm publishing
tokens, or Homebrew tap write access.

## Verification Command Set

Final Rust-only validation uses:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd frontend
npm.cmd ci
npm.cmd run test
npm.cmd run build
cd ..
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-rust-v3.ps1 -Store .tmp\smoke-rust-v3
powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -DryRun
node packaging/npm/bin/coder-rust.js --dry-run
```

POSIX installer dry-run:

```bash
bash ./scripts/install.sh --dry-run
```

If local Windows does not provide `bash`, the POSIX dry-run is covered by the
Ubuntu `installer-dry-run` CI job.

## Remaining Non-Blocking Enhancements

- production embedding provider integrations
- published npm/Homebrew channels
- signed release artifacts and checksum verification
- richer MCP compatibility matrix
