# Coder

Coder is a Planner-first coding workbench with a React frontend and a Rust API
v3 runtime. The current product path is native: Coder owns planning,
execution, verification, evidence, cache policy, and review surfaces.

## Product Path

Coder is split into cooperating agents:

- Planner talks to the user, clarifies scope, tracks readiness, and writes
  public summaries.
- Workflow Planner receives verifier evidence and makes a bounded
  finish-or-improve decision. Closed, objective success takes a zero-provider
  fast path; failures and open-ended quality goals use the real model.
- Executor performs the native ReAct work loop through harness-controlled tools.
- Verifier checks the result and feeds PASS/FAIL evidence back to the loop.

```text
User configures a provider in Settings
-> User talks to Planner
-> Planner marks the task ready
-> User clicks Start Work
-> WorkflowRunner loads examples/coder.yaml
-> native-code-edit executes through native-rust tools or provider-backed exact edits/file writes
-> browser-verification or verifier checks the result
-> Workflow Planner finishes, requests one bounded improvement, or reports a blocker
-> Timeline, evidence, report, Review Changes, and Undo are exposed in the UI
```

Planner Chat is side-effect free. It can ask questions, maintain plan state,
and mark work ready, but it must not write files, run commands, or start work.
It remains usable while Start Work runs: status/cancel/guidance are local
control operations and newer chat turns are revision-merged after completion.
Execution starts only from Start Work.

Harnesses are the execution boundary. A harness controls backend selection,
tool availability, permissions, sandbox policy, memory scope, approvals,
verification, event capture, and evidence. Runtime claims must be backed by
tool events, repo evidence, patch refs, command checks, stored blobs, or final
reports.

After Start Work, the Rust API can inject a provider-backed
`native-model-file-write` executor behind `native-rust`. The preferred path is
a runtime-bounded tool-call loop where the model asks for repo, git, command, skill,
subagent, and write tools. Rust executes them through the shared tool pipeline,
and observations are returned to the model. The default non-interactive
Executor uses 24 turns and stops immediately on `finish`. Providers that do not
return tool calls can still use the strict JSON file-plan fallback.

Rust v3 covers the ordinary product surface behind the React UI:

- health, capabilities, and role cards
- workflow validation, import/export, and library storage
- Planner Chat sessions, readiness, and explicit Start Work
- native executor tools for repo search/read, git status/diff, command preview
  and run, background commands, patch preview/apply, skills, and subagents
- stored run inspection, timeline projection, changesets, undo, reports,
  artifacts, blobs, checkpoints, and repo evidence
- provider settings, provider tests, proxy isolation, memory, knowledge, MCP,
  plugin/skill developer surfaces, and cache status

Maintained docs start at [`docs/README.md`](docs/README.md). The current
architecture is summarized in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Install

Install Rust and Node.js, then install frontend dependencies:

```sh
git clone https://github.com/Garfreak-07/Coder.git
cd Coder
cd frontend
npm install
cd ..
```

## Run Locally

Start the Rust API server:

```sh
cargo run -p coder-cli --bin coder-rust -- server --host 127.0.0.1 --port 8876
```

The CLI and server default to a workspace-local `.coder` store. Durable run
state and disposable runtime caches are derived from that store unless an
explicit `--store`, `CODER_RUNTIME_CACHE_DIR`, or `CODER_CACHE_DIR` override is
set.

Start the frontend:

```sh
cd frontend
npm run dev
```

Open `http://127.0.0.1:5173`. Vite proxies `/api/*` to
`http://127.0.0.1:8876`, and the frontend uses Rust API v3 directly.

## Desktop Proof Of Concept

The desktop path is an opt-in Tauri skeleton and is not part of the main CI
release gate yet.

```sh
npm run desktop:dev
npm run desktop:build
```

Desktop dev mode opens the React app through Vite. Start the Rust API server on
`127.0.0.1:8876` first.

## Test

Rust:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Frontend:

```sh
cd frontend
npm ci
npm run test
npm run build
```

Optional live smokes are developer and CI confidence checks. Ordinary users do
not run these scripts.

Rust v3 Planner-to-Review smoke test:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\smoke-rust-v3.ps1 -Store .tmp\smoke-rust-v3
```

Live LLM smoke, skipped when no provider key is configured:

```powershell
$env:CODER_LIVE_LLM_SMOKE="1"
powershell -ExecutionPolicy Bypass -File .\scripts\live-llm-smoke.ps1 -SkipIfMissingProvider
```

Native full-path self-test:

```powershell
$env:CODER_SELFTEST_LIVE="1"
powershell -ExecutionPolicy Bypass -File .\scripts\live-coder-selftest-suite.ps1 -SkipIfMissingLiveConfig
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

## Useful Rust Commands

```sh
cargo run -p coder-cli --bin coder-rust -- doctor
cargo run -p coder-cli --bin coder-rust -- config validate --path examples/coder.yaml
cargo run -p coder-cli --bin coder-rust -- workflow preview planner-led "summarize this repo"
cargo run -p coder-cli --bin coder-rust -- workflow run --mock planner-led "summarize this repo"
cargo run -p coder-cli --bin coder-rust -- server --host 127.0.0.1 --port 8766
```

## Provider Setup

Use the app Settings page for DeepSeek, OpenAI-compatible providers, model
selection, base URLs, API keys, provider network mode, and optional provider
proxy URLs. Environment variables remain available for headless development:

```powershell
$env:CODER_LLM_PROVIDER_PROFILE="deepseek-default"
$env:DEEPSEEK_API_KEY = Read-Host "DeepSeek API key"
$env:LLM_API_KEY=$env:DEEPSEEK_API_KEY
$env:LLM_BASE_URL="https://api.deepseek.com"
$env:LLM_MODEL="deepseek-chat"
```

DeepSeek and Ollama default to direct provider networking. Providers that often
need a proxy default to environment proxy mode. See
[`docs/PROVIDER_SETUP.md`](docs/PROVIDER_SETUP.md).

Local helper files such as `.local-env.ps1` are ignored by Git and are for one
developer machine only. Do not commit API keys into scripts, docs, examples, or
workflow specs.

## Guardrails

- Keep the ordinary product path Planner-led and Coder-owned.
- Keep Planner Chat side-effect free in product mode.
- Keep Start Work as the execution boundary.
- Keep the ordinary UI starting at Planner Chat; workflow editing is an
  advanced developer surface.
- Executors must not ask the user directly, commit, push, deploy, publish
  externally, or write long-term memory directly.
- Keep environment variables as developer/headless fallback, not normal setup.
- Keep GPU support optional and provider-scoped; it is not core runtime.

## Secrets

Do not commit API keys or local secrets. `.env`, `.env.local`, and
`.local-env.ps1` are ignored by Git.

## License

License: MIT. See [LICENSE](LICENSE).
