# Coder

Planner-led local agent workflow workbench for controlled coding tasks.

Coder is built around an AgentGraph runtime where Planner owns global decisions,
Code Worker performs bounded implementation work, Tester returns evidence, and
the runtime passes compact structured artifacts instead of transcripts.

The current architecture target is v0.9.2:

```text
Ordinary Agent workflow UI
-> AgentRecipe / RuntimeProfileCompiler
-> RuntimeProfileCache
-> RunController / RunGuard
-> AgentGraphRunner / AgentGraphScheduler
-> ActionGateway
-> BudgetBroker
-> ContextService
-> AgentRun
-> AgentEngineRegistry
-> PlannerEngine / CodeWorkerEngine / TesterEngine / FinalReviewEngine / SynthesizerEngine
-> patch preview / sandbox apply / sandbox check / DebugFinding replan
-> TraceSpan, partitioned stores, structured artifacts, PlannerDecision
```

Only Planner can ask the user. Non-Planner Agents return artifacts, blockers, or
evidence to Planner.

## Repository Layout

```text
src/coder_workbench/
  agent_model/       AgentRecipe, RuntimeProfileCompiler, runtime profiles
  actions/           ActionSpec and ActionGateway for controlled effects
  agent_engine/      AgentEngine specs, registry, harness validation, engines
  agent_graph/       Planner-led graph runner, scheduling, cache, merge logic
  agent_harness/     Harness loops and shared ArtifactRepairService
  budget/            BudgetBroker reservations before model/tool/context work
  coding/            Repo intelligence, PatchService, CommandService, checks
  context/           ContextService, ContextPacketV2 and TokenLedger wiring
  core/              AgentWorkflowSpec, artifacts, authority, legacy compile
  extensions/        Extension manifests, plugin/skill routing and runtime
  observability/     TraceSpan and TraceContext models
  runtime/           Legacy WorkflowSpec interpreter and run state
  runtime_kernel/    RunController, RunGuard, round state, plan fingerprinting
  server/            FastAPI app, storage, live run managers
  server/stores/     Partitioned run events, artifacts, blobs, ledgers, cache
  skills/            Installed skill store, registry client, skill router
  tools/             Compatibility tool registry and low-level tool wrappers
frontend/
  src/               React + TypeScript workbench
tests/               Python unittest suite
docs/                Architecture and migration notes
```

## Runtime Boundary

Product Agent workflow runs use:

```text
AgentWorkflowSpec
-> PlannerOrder.plan_graph
-> RunController / RunGuard
-> GraphRunCache
-> ActionGateway
-> AgentTaskEnvelope
-> ContextService
-> BudgetBroker reservations
-> AgentRun
-> AgentEngine
-> execution_result / test_result
-> PlannerInputBundle
-> PlannerDecision
-> RunController next-round decision
```

`WorkflowSpec` and `WorkflowRunner` are legacy compatibility paths for old saved
workflows and advanced preview only. Do not add new product behavior there.
Legacy live-run and compile-preview endpoints are explicitly marked deprecated
or `runtime_type=legacy_preview`; normal product execution uses AgentGraph.

## Core Artifacts

Default AgentGraph artifacts:

- `run_contract`
- `planner_order`
- `execution_result`
- `test_result`
- `planner_decision`
- `round_summary`

Coding diagnostics:

- `repo_index`
- `command_discovery`
- `risk_map`
- `symbol_index`
- `coding_context_packet`
- `patch_preview`
- `check_result`
- `debug_finding`
- `coding_evaluation_report`

Legacy artifacts `plan_artifact`, `patch_artifact`, and `review_artifact` are
retained only for old `WorkflowSpec` flows.

## Key Services

- `ContextService` builds `ContextPacketV2`, selects skill context, prepares
  coding context packets, and writes token ledger entries.
- `RunController` owns PlannerDecision loop control, max rounds, and repeated
  plan fingerprint guards, and writes run diagnostics.
- `BudgetBroker` reserves model, tool, and context budgets before execution.
  Reservation diagnostics are written to run results.
- `ActionGateway` is the runtime entry point for context construction, patch
  preview, sandbox patch apply, sandbox command checks, and artifact
  repair/validation.
- `RuntimeProfileCompiler` converts ordinary Agent roles into internal engine,
  context, token, artifact, plugin, skill, memory, repair, and tool policies.
- `RuntimeProfileCache` avoids recompiling identical workflow/extension/profile
  combinations.
- `AgentRun` dispatches work through `AgentEngineRegistry`.
- `AgentGraphExecutor` is a compatibility adapter over the registered engines.
- `PatchService` owns proposed change validation, risk path blocking, patch
  preview, apply, and rollback behind `ActionGateway`.
- `CommandService` owns scoped cwd validation, command approval, timeouts, and
  output capture behind `ActionGateway`.
- `ArtifactRepairService` owns one-shot JSON artifact repair for model outputs.
- `ExtensionRouter` routes globally installed plugins and skills per work item.
- `TraceContext` attaches `trace_id`, `span_id`, and `parent_span_id` to run
  events.
- `PartitionedRunStores` provides the event, artifact, blob, ledger, extension,
  and cache write path over the `.coder` layout.

## Install

Requires Python 3.11 or newer.

```powershell
git clone https://github.com/Garfreak-07/Coder.git
cd Coder
python -m venv .venv
.\.venv\Scripts\activate
pip install -e .
```

Install frontend dependencies:

```powershell
cd frontend
npm install
```

## Run Locally

Start the API:

```powershell
.\.venv\Scripts\coder-api.exe --host 127.0.0.1 --port 8876
```

Start the frontend dev server:

```powershell
cd frontend
npm.cmd run dev
```

Open:

```text
http://127.0.0.1:5173
```

Vite proxies `/api/*` to `http://127.0.0.1:8876`.

If `frontend/dist` exists, the API can serve the built frontend from:

```text
http://127.0.0.1:8876
```

## CLI Example

```powershell
.\.venv\Scripts\coder.exe --repo . `
  --workflow examples\workflows\coding-workbench.json `
  --request "Build the smallest Planner-led loop"
```

Use `--approve` only for workflows that include explicit human gates or local
effects that should be preapproved.

## API Surface

Common development endpoints:

- `GET /api/v2/health`
- `GET /api/v2/agent-role-cards`
- `POST /api/v2/agent-workflows/validate`
- `POST /api/v2/agent-workflows/runtime-profiles`
- `POST /api/v2/live-agent-runs`
- `GET /api/v2/live-agent-runs/{run_id}`
- `POST /api/v2/live-agent-runs/{run_id}/planner-response`
- `GET /api/v2/extensions/plugins`
- `GET /api/v2/extensions/skills`
- `GET /api/v2/extensions/search`

Legacy `/api/v2/skills/*`, `/api/v2/live-runs`, and `WorkflowSpec` endpoints
remain for compatibility and are marked deprecated where they overlap the
AgentGraph product path. New Agent product behavior should use the AgentGraph
and Extensions endpoints.

`compile_agent_workflow_legacy_preview()` is the explicit compatibility
compiler for advanced preview and migration/debug only. `compile_agent_workflow()`
remains a compatibility alias.

## Testing

Backend:

```powershell
.\.venv\Scripts\python.exe -m unittest discover -s tests
.\.venv\Scripts\python.exe -m compileall src tests
```

Frontend:

```powershell
cd frontend
npm.cmd run build
```

Focused architecture boundary tests:

```powershell
.\.venv\Scripts\python.exe -m unittest tests.test_architecture_boundaries
```

## Development Rules

- Keep the ordinary product path Agent-first.
- User interaction must remain `User <-> Planner` only.
- Worker, Tester, and Final Tester must not ask the user directly.
- Product live Agent workflows must not compile into legacy `WorkflowSpec`.
- New context, patch, command, repair, and validation behavior should enter
  through `ActionGateway`; services such as `ContextService`, `PatchService`,
  and `CommandService` stay behind that boundary.
- Budget-affecting work should reserve through `BudgetBroker` before execution.
- New model-output repair behavior should go through `ArtifactRepairService`.
- New Planner, Tester, FinalReview, Synthesizer, and Worker execution behavior
  should enter through `AgentEngineRegistry`; keep `AgentGraphExecutor` as an
  adapter only.
- Coding auto-loop behavior should preserve the path:
  `proposed_changes -> patch preview -> sandbox apply/check -> DebugFinding -> PlannerDecision`.
- Extensions are globally installed and routed per work item.
- Ordinary UI should not expose runtime JSON, harness graphs, context policies,
  token budgets, or manual capability checklists.

## Documentation

Architecture notes:

- [docs/architecture.md](docs/architecture.md)
- [docs/extensions.md](docs/extensions.md)
- [docs/agent-recipes.md](docs/agent-recipes.md)
- [docs/agent-engines.md](docs/agent-engines.md)
- [docs/coding-kernel.md](docs/coding-kernel.md)
- [docs/deletion-plan.md](docs/deletion-plan.md)

## Secrets

Do not commit API keys or local secrets. Copy `.env.example` to `.env` for local
model configuration. `.env` is ignored by Git.

Provider configuration is OpenAI-compatible and optional. Without credentials,
the runtime uses mock mode for safe local development.

## License

MIT. See [LICENSE](LICENSE).
