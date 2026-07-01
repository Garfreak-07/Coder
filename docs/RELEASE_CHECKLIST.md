# V1 Release Validation Checklist

Run these commands from the repository root unless a step says otherwise.
Set `CARGO_TARGET_DIR` inside the repo on Windows so Cargo does not write to a
parent directory.

```powershell
cd Coder
$env:CARGO_TARGET_DIR = Join-Path (Get-Location) ".tmp\cargo-target"
```

## Required Offline Gates

Rust formatting, linting, and tests:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Frontend tests and production build:

```powershell
cd frontend
npm.cmd run test
npm.cmd run build
cd ..
```

Planner-to-Review smoke:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-rust-v3.ps1 -Store .tmp\smoke-rust-v3
```

This is mock/plumbing validation by default. It must report `validation:
plumbing`, two Planner turns, a started run, timeline items, a completed final
report, one Review Changes changeset, and `undo_status: undone`.

Installer dry runs:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -DryRun
node packaging/npm/bin/coder-rust.js --dry-run
bash ./scripts/install.sh --dry-run
```

Rust-only guard:

```powershell
node scripts/check-rust-only-main.js
```

Whitespace check:

```powershell
git diff --check
```

## Product-Surface Gates

Provider UI test coverage:

```powershell
cd frontend
npm.cmd run test
cd ..
```

The frontend suite must include the Provider Settings test that checks the
DeepSeek preset, provider test button, proxy URL field, mock-mode debug gate,
redacted password input, and product-mode setup blocking when credentials are
missing.

Review/Undo targeted backend test:

```powershell
cargo test -p coder-server changeset_review_diff_accept_and_undo_roundtrip
```

No key leakage targeted tests:

```powershell
cargo test -p coder-server provider_settings_endpoints_store_secret_refs_without_returning_keys
cargo test -p coder-server planner_chat_writes_session_jsonl_without_raw_secret_text
cargo test -p coder-server report_timeline_artifact_and_jsonl_redact_key_like_strings
```

Desktop plan status:

```powershell
Test-Path .\src-tauri\tauri.conf.json
Get-Content .\docs\DESKTOP_APP_PLAN.md
```

The desktop path remains future/experimental and outside the main CI release
gate until a public desktop release plan explicitly promotes it.

Known non-blocking items:

```powershell
Get-Content .\docs\NON_BLOCKING_ENHANCEMENTS.md
```

The release note must call out any remaining non-blocking items instead of
silently treating them as complete.

## Manual Optional Live Gates

Do not mark these complete unless they actually run against live services.

Live DeepSeek smoke:

```powershell
cd Coder
. .\.local-env.ps1
$env:CODER_LIVE_LLM_SMOKE="1"
powershell -ExecutionPolicy Bypass -File .\scripts\live-llm-smoke.ps1 -Provider deepseek -SkipIfMissingProvider
```

Expected live result when credentials and network are available: provider
`deepseek`, provider test mode `live`, two Planner Chat turns, and Start Work
returning either a run id or a Planner clarification. A skipped result is not a
live pass.

Live OpenHands smoke:

```powershell
$env:OPENHANDS_LIVE_SMOKE="1"
$env:OPENHANDS_AGENT_SERVER_URL="http://127.0.0.1:8000"
powershell -ExecutionPolicy Bypass -File .\scripts\live-openhands-smoke.ps1 -SkipIfMissingOpenHands
```

Expected live result when an OpenHands Agent Server is available:
`backend_selected >= 1`, `timeline_backend_items >= 1`,
`timeline_react_items >= 1`, `result_doc_changed >= 1`,
`review_changes >= 1`, raw OpenHands events are stored, final report preview is
readable with `Status: completed`, the run records `run.completed`, Undo
succeeds or safely reports conflict/unsupported, and `secrets_check: passed`.
A skipped result is not a live pass, and a run that edits the file but reports
`blocked` is not a live pass.

Latest local live result recorded for this checklist:

```text
timestamp: 2026-07-01 20:47:45 +08:00
base_commit_before_record: 27ab5509
server_url: http://127.0.0.1:8000
provider: DeepSeek through OpenAI-compatible API
model: deepseek-v4-flash
run_id: 2718536d-950b-4415-970d-20f50844ecf2
status: ok
final_report: completed
timeline_items: 77
timeline_react_items: 64
review_changes: 1
undo_status: undone
secrets_check: passed
```
