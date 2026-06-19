# Coder Product Requirements

## Current direction

Coder is a local-first agent workflow workbench for controlled coding tasks.

The product is no longer centered on a fixed LangGraph coding flow. The core
requirement is that users can create agents, draw workflow edges, save the
workflow, and run it with visible state, approvals, token controls, and audit
events.

The old LangGraph prototype can remain as a compatibility path while v2 is
implemented, but future product behavior should be driven by workflow JSON, not
by hard-coded Python graph changes.

## Target user

- Individual developers who want AI coding help but need control over scope,
  cost, and file mutation.
- Small teams that want repeatable AI-assisted workflows with audit logs.
- Users who want model-provider flexibility, including OpenAI-compatible APIs,
  local models, and future external agent adapters.

## Primary product flow

```text
select project
  -> inspect project map
  -> choose or create agents
  -> draw workflow nodes and edges
  -> configure permissions and context policy
  -> run workflow
  -> watch event stream
  -> approve risky steps
  -> review patch/check results
  -> retry, finish, or block
```

## Required capabilities

### Workflow authoring

- Users can create a workflow visually.
- Workflows are saved as JSON.
- Nodes and edges in JSON are the source of truth.
- Edge conditions determine real runtime routing.
- Workflows can be imported, exported, listed, saved, and reloaded.

Initial node types:

- `start`
- `agent`
- `tool`
- `condition`
- `human_gate`
- `end`

Future node types:

- `loop`
- `parallel`
- `subworkflow`
- `patch_review`
- `mcp_tool`
- `external_agent`

### Agent configuration

Agents are user-configurable objects, not just prompts.

Each agent must support:

- id, name, role, goal, instructions
- provider/model config
- tool list
- permission policy
- context policy
- output key/schema

Agents should be stored in the local library and reusable across workflows.

### Efficient agent collaboration

Agent collaboration must be token-conscious by design.

Rules:

1. Do not pass full transcripts by default.
2. Pass structured state and selected summaries.
3. Each agent declares `input_keys`, `summary_keys`, `max_items_per_key`,
   and `max_chars_per_value`.
4. Full outputs and event history are opt-in.
5. Runtime tracks estimated token use per run.
6. Static agent instructions should stay stable so providers can benefit from
   prompt caching.
7. Agents hand off compact artifacts, not entire conversations.

### Runtime execution

The v2 runtime is a workflow interpreter:

```text
load workflow
  -> validate graph
  -> create run state
  -> execute node
  -> write compact output
  -> evaluate outgoing edges
  -> enqueue next node
  -> emit events
  -> pause / finish / block / fail
```

LangGraph, AutoGen, CrewAI, OpenAI Agents SDK, MCP tools, and local tools can
all be adapters below this layer. They should not define the product contract.

### App backend

The app backend should expose stable APIs:

```text
GET  /api/v2/health
GET  /api/v2/library
POST /api/v2/library/agents
GET  /api/v2/library/agents/{agent_id}
POST /api/v2/library/workflows
GET  /api/v2/library/workflows/{workflow_id}

GET  /api/v2/runs
POST /api/v2/runs
GET  /api/v2/runs/{run_id}
GET  /api/v2/runs/{run_id}/events

GET  /api/v2/live-runs
POST /api/v2/live-runs
GET  /api/v2/live-runs/{run_id}
GET  /api/v2/live-runs/{run_id}/events
```

Use synchronous runs for CLI/debug cases. Use live runs and Server-Sent Events
for the app.

### Frontend

The old single-file web UI should be replaced.

Recommended stack:

```text
React + TypeScript + Vite
React Flow for the workflow canvas
FastAPI backend
Server-Sent Events for run logs
future Tauri or Electron wrapper for desktop app packaging
```

Main frontend surfaces:

- project explorer
- workflow canvas
- agent library
- node/agent inspector
- run timeline
- message/event log
- approval panel
- patch/diff panel
- settings page

### Safety requirements

Agents request actions; runtime enforces actions.

Required gates:

- tool allowlist
- path scope guard
- command approval
- network approval
- human gate before mutation
- patch preview before write
- snapshot before apply
- rollback support
- max step count
- max agent call count
- max tool call count
- token budget
- event audit log

Default behavior must be conservative. Real file mutation should arrive only
after patch preview, approval, and rollback support are implemented.

## Framework alternatives

### LangGraph

Status: not recommended as the main product contract.

LangGraph is useful for code-defined graphs and stateful agent flows, but it is
not a good fit as the direct representation of a user-edited app canvas. It can
remain as an optional runner or compatibility path, but the product should not
require Python graph edits when users add agents or edges.

### Flowise Agentflow

Status: best reference product for visual workflow behavior.

Flowise is close to the desired visual model: nodes, edges, conditions, loops,
human input, state, streaming, and MCP-style integrations. It is useful as a
design reference. Directly adopting it as the core is risky because Coder needs
a local-first coding-specific safety model and app identity.

Decision: borrow concepts, do not make it the core dependency.

### OpenAI Agents SDK

Status: strong candidate for an agent executor adapter.

Useful for agent execution, handoffs, guardrails, tracing, MCP integration,
sessions, and future sandbox-style execution. It is not a visual workflow
engine. It should sit below Coder's own WorkflowRunner.

Decision: add later as `AgentExecutor`.

### AutoGen

Status: candidate adapter for multi-agent team conversations.

AutoGen is strong for agent teams, group chat, selection, and handoff. It is
less suitable as the strict workflow canvas runtime because Coder needs edges,
conditions, approvals, and patch gates to be deterministic product behavior.

Decision: add later for conversation/team nodes.

### CrewAI Flows

Status: useful reference, possible adapter.

CrewAI Flows are event-driven and support state, branching, and loops, but they
are still primarily code-defined. They are not the source-of-truth canvas model.

Decision: optional adapter/reference, not core.

### LlamaIndex Workflows

Status: useful for RAG/data pipelines.

Good fit for retrieval, document processing, and knowledge workflows. Not the
main coding workflow engine.

Decision: use later for retrieval/RAG tools if needed.

### Temporal

Status: future production reliability option.

Temporal is strong for durable execution, queues, retries, and recovery. It is
too heavy for the current local-first prototype but may be valuable if long
background runs become important.

Decision: defer.

### n8n / general low-code automation

Status: reference only.

n8n is mature for automation but not specialized for safe local coding agents,
patch review, file scope controls, or model context budgeting.

Decision: reference only.

## Current implemented v2 slice

Implemented:

- workflow/agent/node/edge schema
- JSON-driven runner
- real edge condition routing
- human approval gate
- compact agent context policy
- estimated token tracking
- mock agent executor when credentials are missing
- React + TypeScript + Vite frontend scaffold
- React Flow workflow canvas with node/edge rendering
- workflow node creation and node inspector editing
- edge inspector editing for `from`, `to`, `when`, `priority`, and
  `max_traversals`
- workflow JSON editor, import, export, save, and reload path
- workflow library list/load/save UI
- agent list, basic agent editor, local agent save, and library agent import
- live run launcher from the UI
- SSE run event timeline with event payload details and compact run summary
- approval-required rerun shortcut in the run timeline
- built-in tools:
  - `project_index`
  - `recommend_modules`
  - `dry_run_patch`
  - `run_check`
- CLI v2 execution:

```powershell
python -m coder_graph.cli --repo . --v2-workflow examples\workflows_v2\coding-workbench.json --request "refactor runtime"
```

- FastAPI v2 runtime API
- live background runs
- SSE event streaming
- file-backed run storage
- local workflow/agent library storage
- optional serving of built `frontend/dist` from the v2 API

## Near-term roadmap

1. Add first-class approval resume endpoints so a paused live run can continue
   without starting a fresh approved run.
2. Add project scope selection and path guard enforcement in v2 tools.
3. Add real patch proposal/apply/rollback tools.
4. Add patch/diff panel and rollback UI.
5. Add provider-specific executor adapters.
6. Add MCP tool adapter.
7. Migrate the default coding workflow fully to v2.
8. Retire the old fixed LangGraph path when v2 reaches parity.
