# Rust Full Completion Report

## Commit Range

- Latest Rust-default completion work starts after `5b586b36`.
- Rust-default switch commit: `e1958377`.
- Legacy CI contract commit: `8d77bde7`.
- Runtime/default-path completed range: `5b586b36..e1958377`.
- Latest GitHub CI evidence: `CI` run `28328526497` completed successfully
  for `8d77bde7130ee02c459472d6a2d7049d8529cf88`.

## Rust Default

Rust v3 is now the default product path:

- the React API resolver returns v3 when no override is set,
- the default local server command is `coder-rust server` on the Vite proxy
  port,
- README local run instructions use Rust first,
- v2 requires an explicit `VITE_CODER_API_VERSION=v2`,
  `CODER_USE_RUST_API=0`, `?coder_api_version=v2`, or local storage override.

## Legacy Python

Python/FastAPI v2 is retained as an explicit legacy compatibility path. It has
not been physically moved to `legacy-python/` in this checkpoint because the
remaining Python compatibility tests and older `/api/v2/*` clients still need a
stable path while replacement coverage is completed.

See `docs/legacy-python.md`.

## Coverage Added In This Checkpoint

- Frontend resolver tests now prove v3 is the default and v2 is explicit.
- Legacy Python compatibility CI installs the `openhands` extra so the suite
  remains green without skipping OpenHands custom tool tests.
- `scripts/smoke-rust-v3.ps1` exercises the default Rust v3 HTTP product path:
  health, library save/load, run preview, mock run, events, report preview,
  final-report artifact, and repo-evidence listing.

## Existing Rust v3 Coverage

The Rust workspace already includes tests for:

- graph-based `WorkflowRunner` transitions, terminal reports, max rounds, and
  monotonic events,
- native Rust repo/search/read/git/command/patch tools with approval gates,
- OpenHands Agent Server path/auth/error/event handling and redaction,
- Rust server v3 health, workflow, Planner Chat, run, report, memory,
  knowledge, skills, extensions, MCP manifest, provider settings, and tool
  endpoints,
- evidence-backed reports derived from run events and stored evidence.

Frontend coverage includes workflow spec roundtrips, Rust API selection,
library save/get mapping, run event mapping, and TypeScript/Vite build.

## Verification

Local verification on this checkpoint:

- `.\.venv\Scripts\python.exe -m unittest discover -s tests`
- `.\.venv\Scripts\python.exe -m compileall src tests`
- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cd frontend; npm.cmd run test`
- `cd frontend; npm.cmd run build`
- `powershell -ExecutionPolicy Bypass -File .\scripts\smoke-rust-v3.ps1 -Store .tmp\smoke-rust-v3`

## Known Limitations

- MCP execution/client support remains explicitly disabled or approval-gated at
  the baseline; manifest validation and deny-by-default behavior are tested.
- Dense RAG remains optional; lexical retrieval is the always-available Rust
  baseline.
- WebSocket preference for OpenHands is documented and tested where supported;
  polling remains the safe fallback.
- MIT migration is not completed in this runtime checkpoint because it requires
  explicit owner/contributor approval and must be a separate license-only
  change.

## Run Rust Product Locally

```powershell
cargo run -p coder-cli --bin coder-rust -- server --host 127.0.0.1 --port 8876
cd frontend
npm.cmd run dev
```

Open `http://127.0.0.1:5173`.

## Force v2 Legacy Fallback

```powershell
.\.venv\Scripts\coder-api.exe --host 127.0.0.1 --port 8876
cd frontend
$env:VITE_CODER_API_VERSION="v2"
npm.cmd run dev
```

## Optional Live OpenHands Smoke

Live OpenHands testing must remain opt-in:

```powershell
$env:CODER_RUN_LIVE_OPENHANDS_TESTS="1"
$env:OPENHANDS_SERVER_URL="http://127.0.0.1:8000"
cargo test -p coder-openhands openhands_real_server_contract_smoke -- --ignored
```
