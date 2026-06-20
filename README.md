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

The default user-facing template is:

```text
Planner Agent -> Executor Agent -> Tester Agent
      ^                                   |
      +----------- loop decision ---------+
```

The product runtime also supports AgentGraph execution where Planner can split a
round into multiple work items, assign them to reachable downstream Agents, wait
on `depends_on`, and submit an ordered compact `PlannerInputBundle` back to
Planner.

Only Planner can ask the human or decide whether to continue, finish, stop, or
request confirmation. Runtime internals such as context selection, artifact
storage, loop routing, path guards, patch safety, approvals, and replay stay
behind that agent-only surface.

Current implementation work follows the v0.4 AgentWorkflow builder track:
ordinary users create Agents, choose capabilities, connect Agents, set loop
limits, save, and run while runtime graph details stay internal.

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

## AgentGraph Plan Semantics

AgentGraph `PlannerOrder.plan_graph.work_items` separates execution dependencies
from result presentation:

1. Planner may emit work items in any list order.
2. `depends_on` is the only semantic execution dependency.
3. Work items with empty `depends_on` are ready together by default.
4. `merge_index` controls stable result presentation back to Planner.
5. `merge_index` does not make earlier work items block later independent work.
6. Resource limits such as `max_concurrency` may limit dispatch, but do not
   create dependency meaning.
7. `PlannerInputBundle` and `round_summary.ordered_state` are sorted by
   `merge_index`.

Execution order is derived from dependency readiness, not from Planner list
order or `merge_index`.

## Runtime Boundary

Product runs use the AgentGraph runtime:

```text
AgentWorkflowSpec -> PlannerOrder.plan_graph -> GraphRunCache -> AgentTaskEnvelope
-> Execution/Test caches -> PlannerInputBundle -> PlannerDecision
```

Legacy `WorkflowSpec` / `WorkflowRunner` remain for advanced inspection and old
saved workflows only. New product behavior should not be added to the legacy
runner.

## Current Capabilities

- `AgentWorkflowSpec` for the user-visible Planner / Executor / Tester layer.
- v0.4 Agent workflow validation with one primary Planner, arbitrary Agent
  count, hidden handoff inference, and deterministic save-blocking errors.
- Initial capability registry for Planner, Executor/Worker, and Tester/Reviewer
  capabilities.
- `AgentGraphRuntime` powers product `/api/v2/live-agent-runs` executions
  without compiling Agent workflows into legacy `WorkflowSpec`.
- Legacy `WorkflowSpec` compilation remains available only for advanced runtime
  preview and old saved workflows.
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
