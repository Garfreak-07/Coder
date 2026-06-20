# Coder

Planner-led local agent workflow workbench for controlled coding tasks.

Coder runs a local workflow where a strong Planner owns the global decision,
Executor performs authorized implementation work, and Tester returns evidence.
Agents exchange compact structured artifacts instead of full transcripts.

## Current Product Target

The active direction is:

```text
Planner-led Orchestrator
+ Structured Artifact Handoff
+ Agent-only Workflow UI
+ Hidden Runtime Graph
```

The ordinary user-facing workflow is:

```text
Planner Agent -> Executor Agent -> Tester Agent
      ^                                   |
      +----------- loop decision ---------+
```

Only Planner can ask the human or decide whether to continue, finish, stop, or
request confirmation. Runtime internals such as context selection, artifact
storage, loop routing, path guards, patch safety, approvals, and replay stay
behind that agent-only surface.

Current implementation work should follow [docs/requirements.md](docs/requirements.md).

## Core Artifacts

The default workflow uses six validated artifact types:

- `run_contract`
- `planner_order`
- `execution_result`
- `test_result`
- `planner_decision`
- `round_summary`

Legacy `plan_artifact`, `patch_artifact`, and `review_artifact` are retained
only for old saved workflows.

## Current Capabilities

- `AgentWorkflowSpec` for the user-visible Planner / Executor / Tester layer.
- Compiler from Agent-only workflow to the internal runtime `WorkflowSpec`.
- FastAPI runtime API and React + TypeScript workbench.
- Mock-mode executor for local development without model credentials.
- Structured artifact validation, event emission, storage, and replay.
- Context packet events before agent calls.
- Provider settings for OpenAI-compatible model providers.
- Local run history, stored run replay, and artifact/blob loading.
- Scoped path guards, patch preview/apply/rollback primitives, command
  approvals, and preflight checks retained as internal safety capabilities.

## Install

Requires Python 3.11 or newer.

```powershell
git clone https://github.com/Garfreak-07/Coder.git
cd Coder
python -m venv .venv
.\.venv\Scripts\activate
pip install -e .
```

Frontend dependencies are in `frontend/`:

```powershell
cd frontend
npm install
```

## Run the API

```powershell
coder-api --host 127.0.0.1 --port 8876
```

If `frontend/dist` exists, the API serves it from:

```text
http://127.0.0.1:8876
```

## Run the Frontend in Development

```powershell
cd frontend
npm run dev
```

Open:

```text
http://127.0.0.1:5173
```

Vite proxies `/api/*` to the API on port `8876`.

## Run the Default Workflow from the CLI

```powershell
coder --repo . `
  --workflow examples\workflows\coding-workbench.json `
  --request "Build the smallest Planner-led loop"
```

Use `--approve` only for workflows that include explicit human gates.

## API Keys and Local Secrets

Do not commit API keys or local secrets. Copy `.env.example` to `.env` for local
model configuration. `.env` is ignored by Git.

Supported provider configuration is OpenAI-compatible and remains optional.
When credentials are missing, the runtime uses mock mode for safe local testing.

## License

MIT. See [LICENSE](LICENSE).
