# Coder

Coder is a Planner-first coding workbench with a React frontend and a Rust API
v3 runtime/control plane.

Current `main` is Rust-only. The supported product path is the Rust API v3
server, the React Planner UI, Provider Settings, Rust workflow/agent/harness
execution, run evidence, reports, memory/knowledge/RAG baselines, MCP
baselines, release tooling, and installer tooling.

The removed pre-Rust compatibility implementation remains available only in git
history at tag `pre-rust-only-legacy-v2`.

## Product Path

Coder is Codex split into two cooperating agents:

- Planner talks to the user, organizes context, asks clarifying questions, and
  owns public summaries.
- Executor performs the ReAct work loop through harness-controlled tools,
  permissions, evidence, and verification.

```text
User configures provider in Settings
-> User talks to Planner first
-> Planner Chat clarifies scope, risks, and acceptance criteria
-> Start Work is an explicit execution action
-> WorkflowRunner
-> HarnessSpec selects native Rust or OpenHands backend
-> Executor runs Reason -> Act -> Observe through role-specific tools
-> Codex-style timeline projects commands, tools, approvals, file changes, checks
-> Review Changes exposes diff, checks, evidence, accept, and undo
-> Planner-authored final summary
```

Planner Chat is side-effect free and LLM-backed in product mode. It can answer
casual questions, ask clarifying questions, maintain internal plan state, and
mark work ready, but it does not write files, run commands, or start workflows.
Execution starts only when the user clicks Start Work. That explicit action
validates readiness, passes structured plan context into workflow execution,
and opens the Codex-style work timeline.

Harnesses are the execution boundary. A harness controls backend selection,
tools, permissions, sandbox policy, memory scope, approvals, verification, event
capture, and evidence. Each agent is expected to be Codex-grade inside its
role-specific harness, meaning runtime claims must be backed by tool events,
repo evidence, patch refs, command checks, or stored raw backend events.

OpenHands is the preferred execution backend when available for coding-agent
runtime behavior such as terminal/file/task execution. Native Rust fallback is
limited deterministic plumbing for CI and local smoke tests, not a second full
agent runtime. Coder owns the product control plane: Planner conversation,
workflow graph, Agent/Harness specs, permission policy, approvals, event
normalization, evidence storage, final reports, and the React product UI.

Rust v3 covers the ordinary product surface behind the React UI:

- health, capabilities, and role cards
- workflow validation, import/export, and library storage
- Planner Chat sessions, internal plan state, readiness, and explicit Start Work
- stored run inspection, timeline projection, changesets, undo, reports,
  artifacts, blobs, and repo evidence
- repo, command, patch, MCP, skills, extensions, provider settings, and memory
  APIs
- experimental Plugins & Skills developer/debug surface, hooks display, and
  cache status
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

The future desktop packaging path is documented in
[`docs/DESKTOP_APP_PLAN.md`](docs/DESKTOP_APP_PLAN.md). Current development
mode stays as the Rust API server plus Vite frontend.

## Desktop Proof Of Concept

The desktop path is an opt-in Tauri skeleton and is not part of the main CI
release gate yet. It keeps the existing web/dev workflow intact.

```powershell
npm run desktop:dev
npm run desktop:build
```

Desktop dev mode opens the React app through Vite. Start the Rust API server on
`127.0.0.1:8876` as shown above before using the product flow. Static desktop
builds default API calls to `http://127.0.0.1:8876` unless
`VITE_CODER_API_BASE_URL` or `window.CODER_API_BASE_URL` is set.

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

Rust v3 Planner-to-Review smoke test:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-rust-v3.ps1 -Store .tmp\smoke-rust-v3
```

By default this is a mock/plumbing validation. It configures Provider Settings
in mock mode, creates a Planner Chat session, sends two side-effect-free turns,
starts work, reads timeline and final-report surfaces, then verifies Review
Changes and Undo against a temporary Git repository. Passing `-LiveProvider`
labels the run as product validation and uses the configured provider path.

Optional live LLM smoke, skipped when no provider key is configured:

```powershell
$env:CODER_LIVE_LLM_SMOKE="1"
powershell -ExecutionPolicy Bypass -File .\scripts\live-llm-smoke.ps1 -SkipIfMissingProvider
```

Mock tests prove deterministic plumbing. The optional live LLM smoke is the
product-confidence check for the real provider path.

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

OpenHands remains the preferred external Executor backend. Without a running
OpenHands server, local development can use native Rust fallback capabilities
and the mock workflow endpoint used by smoke tests.

Optional live OpenHands validation is available when a server is configured:

```powershell
$env:OPENHANDS_LIVE_SMOKE="1"
$env:OPENHANDS_AGENT_SERVER_URL="http://127.0.0.1:8000"
powershell -ExecutionPolicy Bypass -File .\scripts\live-openhands-smoke.ps1
```

Without `OPENHANDS_LIVE_SMOKE=1`, the script can be run with
`-SkipIfMissingOpenHands` to report `skipped` for CI and local release checks.

## Provider Setup

Use the app `Settings` page for DeepSeek or OpenAI-compatible API keys, model
selection, base URLs, and provider proxy URLs. The normal user path does not
require `LLM_BASE_URL`, `LLM_API_KEY`, or `SESSION_API_KEY`. See
[`docs/PROVIDER_SETUP.md`](docs/PROVIDER_SETUP.md).

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
- Keep Planner Chat LLM-backed in product mode.
- Keep user interaction in `User <-> Planner`.
- Keep Start Work as the only execution boundary.
- Keep the ordinary UI starting at Planner Chat; the workflow canvas is an
  Advanced -> Developer -> Workflow editor surface.
- Executors must not ask the user directly, commit, push, deploy, publish
  externally, or write long-term memory directly.
- Keep OpenHands as an optional external backend.
- Keep marketplace/plugin UI deferred from the ordinary product path.
- Keep environment variables as developer/headless fallback, not normal setup.
- Keep GPU support optional and provider-scoped; it is not core runtime.
- Keep the Advanced React workflow canvas, user-defined agents, workflows, harnesses,
  provider settings, evidence/report systems, memory/knowledge/RAG baselines,
  MCP baselines, release tooling, and installer tooling.

## Secrets

Do not commit API keys or local secrets. `.env`, `.env.local`, and
`.local-env.ps1` are ignored by Git.

## License

License: MIT. See [LICENSE](LICENSE).
