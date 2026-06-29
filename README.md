# Coder

Coder is a Planner-first coding workbench with a React frontend and a Rust API
v3 runtime/control plane.

Current `main` is Rust-only. The supported product path is the Rust API v3
server, the React workflow canvas, Rust workflow/agent/harness execution, run
evidence, reports, memory/knowledge/RAG baselines, MCP baselines, provider
settings, release tooling, and installer tooling.

The previous Python/FastAPI v2 compatibility implementation was removed from
`main` after the Rust-only migration. It remains available in git history at
tag `pre-rust-only-legacy-v2`.

## Product Path

```text
User talks to Planner first
-> Planner Chat clarifies scope, risks, and acceptance criteria
-> Start Work is an explicit execution action
-> WorkflowRunner
-> HarnessSpec selects native Rust or OpenHands backend
-> Executor runs inside role-specific tools, permissions, memory, and verification
-> Codex-style timeline projects commands, tools, approvals, file changes, checks
-> Review Changes exposes diff, checks, evidence, accept, and undo
-> Planner-authored final summary
```

Planner Chat is side-effect free. It can answer casual questions, ask
clarifying questions, maintain internal plan state, and mark work ready, but it
does not write files, run commands, or start workflows. Execution starts only
when the user clicks Start Work. That explicit action validates readiness,
passes structured plan context into workflow execution, and opens the
Codex-style work timeline.

Harnesses are the execution boundary. A harness controls backend selection,
tools, permissions, sandbox policy, memory scope, approvals, verification, event
capture, and evidence. Each agent is expected to be Codex-grade inside its
role-specific harness, meaning runtime claims must be backed by tool events,
repo evidence, patch refs, command checks, or stored raw backend events.

OpenHands is the preferred execution backend when available for coding-agent
runtime behavior such as terminal/file/task execution. Coder owns the product
control plane: Planner conversation, workflow graph, Agent/Harness specs,
permission policy, approvals, event normalization, evidence storage, final
reports, and the React product UI.

Rust v3 covers the ordinary product surface behind the React UI:

- health, capabilities, and role cards
- workflow validation, import/export, and library storage
- Planner Chat sessions, internal plan state, readiness, and explicit Start Work
- stored run inspection, timeline projection, changesets, undo, reports,
  artifacts, blobs, and repo evidence
- repo, command, patch, MCP, skills, extensions, provider settings, and memory
  APIs
- local Plugins & Skills marketplace surface, hooks display, and cache status
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

OpenHands remains an optional external backend. Without a running OpenHands
server, local development can use native Rust fallback capabilities and the
mock workflow endpoint used by smoke tests.

## Provider Setup

Use the app `Settings` page for DeepSeek or OpenAI-compatible API keys. The
normal user path does not require `LLM_BASE_URL`, `LLM_API_KEY`, or
`SESSION_API_KEY`. See [`docs/PROVIDER_SETUP.md`](docs/PROVIDER_SETUP.md).

For local OpenHands smoke tests and headless development, environment variables
remain available as fallback. Prefer environment variables rather than committed
files:

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
