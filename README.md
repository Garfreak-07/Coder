# Coder

Coder is a Rust v3 control-plane coding workbench with a React frontend.

Current `main` is Rust-only. The supported product path is the Rust API v3
server, the React workflow canvas, Rust workflow/agent/harness execution, run
evidence, reports, memory/knowledge/RAG baselines, MCP baselines, provider
settings, release tooling, and installer tooling.

The previous Python/FastAPI v2 compatibility implementation was removed from
`main` after the Rust-only migration. It remains available in git history at
tag `pre-rust-only-legacy-v2`.

## Product Path

```text
User request
-> Planner Chat
-> Rust API v3 run preview / confirmation
-> WorkflowRunner
-> native Rust or OpenHands harness backend
-> stored events / evidence-backed final_report
```

Planning Chat Discuss mode does not start execution. Work mode can start a Rust
run only after readiness and confirmation gates pass.

Rust v3 covers the ordinary product surface behind the React UI:

- health, capabilities, and role cards
- workflow validation, import/export, and library storage
- Planner Chat sessions and run preview/confirmation
- stored run inspection, events, reports, artifacts, blobs, and repo evidence
- repo, command, patch, MCP, skills, extensions, provider settings, and memory
  APIs
- lexical, deterministic dense, and hybrid knowledge retrieval baselines

## Install

Install Rust and Node.js, then install frontend dependencies:

```powershell
git clone https://github.com/Garfreak-07/Coder.git
cd Coder
cd frontend
npm install
cd ..
```

## Run Locally

Start the Rust API server on the Vite proxy port:

```powershell
cargo run -p coder-cli --bin coder-rust -- server --host 127.0.0.1 --port 8876
```

Start the frontend:

```powershell
cd frontend
npm.cmd run dev
```

Open `http://127.0.0.1:5173`. Vite proxies `/api/*` to
`http://127.0.0.1:8876`, and the frontend uses Rust API v3 directly.

## Test

Rust:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Frontend:

```powershell
cd frontend
npm.cmd ci
npm.cmd run test
npm.cmd run build
```

Rust v3 smoke test:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-rust-v3.ps1 -Store .tmp\smoke-rust-v3
```

Installer dry-runs:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -DryRun
node packaging/npm/bin/coder-rust.js --dry-run
```

POSIX installer dry-run:

```bash
bash ./scripts/install.sh --dry-run
```

The Rust CLI/distribution baseline is documented in
[`docs/distribution.md`](docs/distribution.md).

## Useful Rust Commands

```powershell
cargo run -p coder-cli --bin coder-rust -- doctor
cargo run -p coder-cli --bin coder-rust -- config validate --path examples\coder.yaml
cargo run -p coder-cli --bin coder-rust -- workflow preview planner-led "summarize this repo"
cargo run -p coder-cli --bin coder-rust -- workflow run --mock planner-led "summarize this repo"
cargo run -p coder-cli --bin coder-rust -- server --host 127.0.0.1 --port 8766
```

## OpenHands

OpenHands remains an optional external backend. Without credentials or the
runtime flag, local development can use the internal fallback provider or Rust
mock workflow path.

For local OpenHands smoke tests, prefer environment variables rather than
committed files:

```powershell
$env:CODER_LLM_PROVIDER_PROFILE="deepseek-default"
$env:DEEPSEEK_API_KEY="..."
$env:LLM_API_KEY=$env:DEEPSEEK_API_KEY
$env:LLM_BASE_URL="https://api.deepseek.com"
$env:LLM_MODEL="deepseek-v4-flash"
$env:CODER_ENABLE_OPENHANDS_RUNTIME="1"
```

`examples/coder.yaml` shows the explicit compatibility profile for older
SDK-style OpenHands servers.

## Guardrails

- Keep the ordinary product path Planner-led and Rust-backed.
- Keep user interaction in `User <-> Planner`.
- Executors must not ask the user directly, commit, push, deploy, publish
  externally, or write long-term memory directly.
- Keep OpenHands as an optional external backend.
- Keep the React workflow canvas, user-defined agents, workflows, harnesses,
  provider settings, evidence/report systems, memory/knowledge/RAG baselines,
  MCP baselines, release tooling, and installer tooling.

## Historical v2 Path

Users who need the removed Python/FastAPI v2 compatibility implementation can
check out:

```powershell
git checkout pre-rust-only-legacy-v2
```

That tag points to the final pre-Rust-only compatibility state.

## Secrets

Do not commit API keys or local secrets. `.env`, `.env.local`, and
`.local-env.ps1` are ignored by Git.

## License

License: MIT. See [LICENSE](LICENSE).
