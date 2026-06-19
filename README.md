# Coder

Local-first agent workflow workbench for controlled coding tasks.

Users define agents, nodes, edges, approvals, scopes, and context policy in a
workflow document, then run it through a FastAPI backend and React workbench UI.

Default behavior is conservative. The current runtime supports inspection,
planning, approval gates, patch previews, scoped patch apply, rollback, checks,
event logs, and scoped project indexing.

## Current product target

The active product target is MVP v0.2: a local-first workflow builder for users
who know some code. The app should make the default flow template-first for
ordinary use, while keeping the workflow JSON, API fields, and internal schema
stable in English for advanced editing.

Current implementation work should follow [docs/mvp-v0.2.md](docs/mvp-v0.2.md)
and the active priority notes in [docs/requirements.md](docs/requirements.md).
Older unfinished ideas should be treated as backlog unless they directly block
the v0.2 acceptance criteria.

## Current capabilities

- JSON workflow schema for agents, nodes, edges, and conditions.
- React + TypeScript workflow workbench with canvas, inspector, JSON editor,
  library save/load, live run launcher, and run timeline.
- FastAPI runtime API with synchronous runs, live background runs, SSE
  events, file-backed run storage, and local agent/workflow library storage.
- Human approval gates with same-run approval resume:
  `POST /api/v2/live-runs/{run_id}/approve`.
- Token-conscious agent context policy and estimated token tracking.
- Project scope selection and path guard enforcement for tools.
- Built-in tools:
  - `project_index`
  - `recommend_modules`
  - `dry_run_patch`
  - `propose_patch`
  - `apply_patch`
  - `rollback_patch`
  - `run_check`
- Scoped patch preview, apply snapshots, rollback, and UI diff/check display.

See [docs/requirements.md](docs/requirements.md) for the product direction and
roadmap.

## Install

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

## Run the frontend in development

```powershell
cd frontend
npm run dev
```

Open:

```text
http://127.0.0.1:5173
```

Vite proxies `/api/*` to the API on port `8876`.

## Run a workflow from the CLI

```powershell
coder --repo . `
  --scope src `
  --workflow examples\workflows\coding-workbench.json `
  --request "Inspect runtime safety"
```

Use `--approve` to pre-approve human gates for CLI/debug runs.

## API keys and local secrets

Do not commit API keys or local secrets. Copy `.env.example` to `.env` for local
model configuration. `.env` is ignored by Git.

Supported provider configuration is OpenAI-compatible and remains optional; when
credentials are missing, the runtime uses a mock executor for safe local
testing.

## Module map

Generate a clickable module map:

```powershell
coder --repo "D:\projects\some-app" --map-only --scope src
```

The product contract is the JSON workflow runtime.

## License

MIT. See [LICENSE](LICENSE).
