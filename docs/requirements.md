# Coder Product Requirements

## Document status

This is the canonical product and architecture requirements document for Coder.
It replaces the older split planning documents that previously described the
product vision, MVP v0.2, workflow builder, foundation architecture, and
context/RAG design.

Branch-specific handoff notes should stay local and are not part of the
published GitHub documentation.

## Product goal

Coder is a local-first AI coding workflow workbench for controlled project work.

The product is not a single coding agent, not a chat app, and not a generic
low-code automation clone. Coder owns a deterministic workflow runtime that
lets users build, inspect, approve, replay, and recover AI-assisted coding
workflows while keeping model context, local memory, file mutation, and audit
records under explicit control.

The current goal has moved beyond the old MVP v0.2 checklist. The next product
foundation is a resource-conscious runtime:

- compact ContextPackets for model calls;
- structured artifacts for agent handoff;
- content-addressed Blob storage for large data;
- compact RunState and EventLog records that reference stored objects;
- lazy Run Replay loading instead of loading full historical runs at once;
- real token and resource budgets, not best-effort warnings.

The product surface remains a template-first workflow app, but the technical
priority is to make context, artifacts, history, approvals, and replay durable
without copying the same large content into State, Events, Checkpoints, and
RunResult multiple times.

The next phase is `Coder v0.3 - Trust Runtime`. Its goal is to prove one
default local AI coding workflow end to end:

```text
select project
  -> configure provider or use mock mode
  -> run workflow preflight
  -> Planner produces PlanArtifact
  -> user approves the plan
  -> Executor produces PatchArtifact
  -> user reviews patch preview
  -> user approves patch apply
  -> runtime applies patch and runs checks
  -> Reviewer produces ReviewArtifact
  -> user replays or recovers the run
```

The phase should prefer fewer capabilities with stronger runtime guarantees
over adding more agent types, providers, marketplaces, or external framework
surface area.

## Target user

The primary user knows some code and can understand files, commands, API keys,
diffs, and scopes, but should not need to understand workflow-engine internals.

This user wants to:

- use their own model provider keys, especially OpenAI-compatible and DeepSeek
  APIs;
- keep project files and run history local by default;
- create repeatable project workflows from templates;
- manually adjust workflows when needed;
- inspect why an agent received specific context;
- approve risky file, command, network, GitHub, or MCP actions;
- avoid wasting tokens by sending full transcripts, full repositories, or full
  historical runs to the model.

## Product principles

1. Coder owns the workflow contract.
   External agent frameworks can be adapters, references, or capability
   providers. They must not define the saved workflow format.

2. Workflows are product data.
   Nodes, edges, agents, policies, budgets, and templates are saved as stable
   JSON with English internal field names.

3. The UI is ordinary-user friendly.
   User-facing labels should be Chinese where appropriate, while schemas, APIs,
   tests, node types, tool names, and code identifiers stay English.

4. Agents exchange artifacts, not transcripts.
   Agent outputs should be structured, validated, summarized, routed, and
   inspectable.

5. Context is centrally selected.
   The context manager builds ContextPackets from user goals, upstream
   artifacts, project summaries, selected snippets, retrieved knowledge chunks,
   constraints, and required output schemas.

6. Large content is stored once.
   State, Events, Checkpoints, and run summaries should store IDs and compact
   summaries. Full content belongs in Artifact, Context, or Blob stores.

7. Auditability must not inflate model context.
   Saving complete local run records is different from sending those records to
   the model. Replay and audit data should be loaded and transmitted on demand.

8. Agents request actions. The runtime enforces actions.
   Tool allowlists, path scopes, command approval, network approval, patch
   preview, snapshot, rollback, and audit records are runtime responsibilities.

## Next phase scope

P0 work must harden the trust runtime foundation:

- Artifact Schema and validation for `plan_artifact`, `patch_artifact`, and
  `review_artifact`;
- ContextPacket productization and on-demand replay loading;
- Run Replay core timeline with compact events and object references;
- Patch Safety through preview, approval, snapshot, apply, and rollback;
- Workflow Preflight Validation before run start;
- Approval UX with readable reasons, affected files/commands, and rejection
  records;
- Durable Recovery for blocked approval runs after API restart;
- Token Budget enforcement that blocks after compaction fails;
- Artifact, Blob, Event, and RunState storage separation.

P1 work should improve usability once P0 is stable:

- Provider Settings UI for OpenAI, DeepSeek, OpenAI-compatible base URLs,
  connection tests, and mock mode;
- Project Summary quality and candidate check detection;
- Run History filtering and search;
- Retry Current Node and standardized failure reasons;
- Loop UX with iteration grouping and clearer retry semantics.

P2 work can wait until the foundation is reliable:

- local `.md` / `.txt` knowledge storage;
- MCP tool discovery and longer-lived server sessions;
- more provider presets;
- advanced diff viewer;
- project-level long-term memory.

The next phase explicitly does not include public marketplace, agent-pack
marketplace, cloud sync, multi-user team permissions, desktop packaging,
PDF/Word knowledge ingestion, arbitrary website automation, free-form
multi-agent chat teams, parallel nodes, subworkflow nodes, GitHub automatic
write capability, or replacing the Coder runtime with an external agent
framework.

## Primary product flow

```text
open app
  -> configure provider or use mock mode
  -> select a local project
  -> choose a workflow template
  -> optionally edit agents, tools, nodes, edges, scopes, and budgets
  -> run workflow
  -> inspect ContextPackets, artifacts, events, token estimates, and approvals
  -> approve or reject risky steps
  -> review patch preview, apply result, check output, and rollback option
  -> replay or resume a stored run when needed
```

## Core architecture

Recommended foundation:

```text
UI / App Shell
  -> Workflow Library and Template Layer
  -> Coder Workflow Runtime
  -> Context Manager
  -> Artifact Store / Context Store / Blob Store
  -> Agent Executor Adapters
  -> Capability Registry
  -> Safety, Approval, Audit, Rollback
  -> Run Store and Replay APIs
```

### Workflow runtime

The runtime is a workflow interpreter:

```text
load workflow
  -> validate graph
  -> create or resume run state
  -> execute node
  -> write compact output
  -> evaluate outgoing edges
  -> enqueue next node
  -> emit compact events
  -> pause, finish, block, or fail
```

OpenAI Agents SDK, AutoGen, CrewAI, LlamaIndex, MCP servers, and other systems
can sit below this runtime as executors or capabilities. They should not become
the source of truth for Coder workflows.

### WorkflowSpec

The workflow remains the source of truth:

```text
workflow
  id
  metadata
  agents
  nodes
  edges
  budgets
  permissions
  template metadata
```

Current node types:

- `start`
- `agent`
- `tool`
- `mcp_tool`
- `condition`
- `loop`
- `human_gate`
- `end`

Future node types:

- `parallel`
- `subworkflow`
- `rag_retrieve`
- `memory_write`
- `patch_review`
- `external_agent`

### AgentSpec

Agents are configurable execution contracts, not just prompts:

```text
agent
  id
  display_name
  role
  goal
  instructions
  model_policy
  input_contract
  output_contract
  context_policy
  memory_policy
  tool_policy
  permission_policy
  token_budget
```

Each agent declares what it may receive, what it must produce, which tools it
may call, and which actions require approval.

### Capability

Tools, MCP servers, skills, and future external agent packs normalize into one
product abstraction:

```text
capability
  id
  type
  display_name
  description
  input_schema
  output_schema
  permissions
  install_source
  runtime_adapter
  risk_level
```

MCP is an integration protocol, not a trust boundary. Coder still enforces
scopes, approval, and audit.

## Context, artifact, and storage model

### ContextPacket

Every agent invocation receives a ContextPacket generated by Coder:

```json
{
  "task": {
    "user_goal": "",
    "workflow_goal": "",
    "current_node": "",
    "current_agent_role": ""
  },
  "upstream_artifacts": [],
  "project_context": {
    "project_summary": "",
    "selected_files": [],
    "selected_snippets": []
  },
  "knowledge_context": {
    "knowledge_sources": [],
    "retrieved_chunks": []
  },
  "memory_context": {
    "run_memory": [],
    "project_memory": [],
    "user_preferences": []
  },
  "constraints": {
    "allowed_tools": [],
    "write_scopes": [],
    "token_budget": 0,
    "approval_required": true
  },
  "required_output": {
    "artifact_type": "",
    "schema": {}
  },
  "provenance": [],
  "token_estimate": {
    "input_tokens": 0,
    "budget_remaining": 0
  }
}
```

The UI should explain each packet in user language:

- which files and snippets were included;
- which document chunks were retrieved;
- which upstream artifacts were included;
- which tools and scopes were allowed;
- which items were omitted or summarized to save tokens.

### Artifacts

Agents produce artifacts, not loose prose.

Required coding workflow artifacts:

- `plan_artifact`
- `patch_artifact`
- `review_artifact`

Artifacts must be validatable, summarizable, routable, and inspectable. The
runtime assigns an `artifact_id` after validation and emits compact
`artifact.produced` events with only ID, type, summary, and size metadata.
Validation failure blocks the run and emits a readable
`artifact.validation_failed` event.

`plan_artifact`:

```json
{
  "artifact_type": "plan_artifact",
  "summary": "",
  "target_files": [],
  "required_context": [],
  "implementation_steps": [],
  "risks": [],
  "recommended_checks": [],
  "executor_instructions": ""
}
```

`patch_artifact`:

```json
{
  "artifact_type": "patch_artifact",
  "implementation_summary": "",
  "changed_files": [],
  "patches": [],
  "risks": [],
  "suggested_check_command": ""
}
```

`review_artifact`:

```json
{
  "artifact_type": "review_artifact",
  "status": "pass | needs_changes | failed | blocked",
  "evidence": [],
  "issues": [],
  "risk_level": "low | medium | high",
  "recommended_action": ""
}
```

### Storage responsibilities

Large data should not be copied through every runtime object.

```text
RunState
  -> small state, current node data, object IDs, compact summaries

Artifact Store
  -> full structured artifacts

Context Store
  -> full ContextPackets

Blob Store
  -> large snippets, diffs, logs, snapshots, raw tool output

Event Log
  -> event type, timestamps, summaries, object IDs, size metadata

Run Index
  -> searchable/listable run metadata without reading full run payloads
```

Do not emit events like this:

```json
{
  "type": "node.completed",
  "result": {
    "patch": "very large patch content..."
  }
}
```

Prefer compact event payloads:

```json
{
  "type": "node.completed",
  "artifact_id": "artifact_123",
  "summary": "changed 4 files",
  "size_bytes": 182340
}
```

Use content hashes for duplicate-prone content:

```text
blob_id = sha256(content)
```

ContextPacket references should be able to point to a stored blob:

```json
{
  "source": "src/runtime.py",
  "blob_id": "sha256:...",
  "start_line": 40,
  "end_line": 100
}
```

### Run Replay

Replay should be lazy and paginated. Opening a run must not load every event,
artifact, context packet, diff, check log, and snapshot at once.

Recommended APIs:

```text
GET  /api/v2/runs
GET  /api/v2/runs/{run_id}
GET  /api/v2/runs/{run_id}/events?cursor=...&limit=...
GET  /api/v2/runs/{run_id}/context-packets/{packet_id}
GET  /api/v2/runs/{run_id}/artifacts/{artifact_id}
GET  /api/v2/runs/{run_id}/blobs/{blob_id}
POST /api/v2/workflows/validate
```

Recommended persisted layout:

```text
runs/index.sqlite
runs/{run_id}/metadata.json
runs/{run_id}/events.jsonl
runs/{run_id}/artifacts/
runs/{run_id}/contexts/
blobs/
```

JSONL append-only events are preferred over constantly rewriting a growing full
run JSON file.

## Token and resource requirements

Token control is a product requirement, not an optimization pass.

Default context rules:

1. Empty `input_keys` must not mean "include all State".
2. Full State access requires explicit `include_all_state: true` and should be
   treated as advanced/risky behavior.
3. Lists must be recursively compacted. Truncating list length is not enough if
   list items contain large strings or dicts.
4. Full event history, full tool output, and full artifacts are opt-in.
5. Static agent instructions should stay stable so providers can benefit from
   prompt caching.
6. Estimated token use should be visible before and after agent calls.
7. Budget overflow should first compact or drop low-priority context, then block
   the run if it still exceeds limits.

Initial budget targets:

```text
Planner input budget        12K
Planner output budget        2K
Executor input budget       24K
Executor output budget       4K
Reviewer input budget       12K
Reviewer output budget       2K
Default run budget        60K-80K
```

Initial context and storage limits:

```text
Max characters per snippet       8K
Max snippets per agent           12
Max ContextPacket size           128 KB
Max inline event payload          16 KB
Recent hot in-memory events       200
Default parallel agent runs       2
Default parallel tool runs        4
Default retained full runs        20-50
Default history retention         30 days
Default data directory quota      5 GB
Max check log per run             5-10 MB
```

Token estimation should use a real tokenizer when available. If no tokenizer is
available, keep the estimate conservative and apply a safety factor.

## Required product surfaces

### Template-first workflow builder

The default app should show a polished coding workflow template before exposing
raw JSON. Advanced users can open the canvas and JSON editor.

The builder should support:

- template cards;
- readable node labels;
- agent cards;
- tool and capability toggles;
- workflow canvas editing;
- edge and condition editing;
- JSON import/export;
- workflow save/load;
- validation before run.

### Agent editor

The agent editor should support:

- display name;
- role;
- goal;
- instructions;
- provider/model override;
- allowed tools;
- input contract;
- output artifact type;
- context policy;
- memory policy placeholder;
- permission policy;
- token budget.

### Provider settings

Provider settings should support:

- OpenAI API key;
- DeepSeek API key;
- optional OpenAI-compatible base URL;
- default model;
- connection test action;
- local mock mode when keys are missing.

API keys must not be stored in workflow JSON.

### Project summary

The app should build or reuse a local project summary:

- file tree;
- ignored directories;
- important files;
- detected framework;
- candidate test/build commands;
- module summaries when available.

This summary feeds the context manager. Agents should not scan the repository
from scratch by default.

### Local document knowledge

The first knowledge system should be deliberately small:

- local `.md` and `.txt` documents;
- local storage;
- stable chunk IDs;
- retrieval through configured embeddings or local embeddings;
- source path and chunk ID provenance in ContextPackets.

PDF, Word, web sync, and large knowledge base management are later work.

### Run history and recovery

Users should be able to:

- list stored runs without reading full payloads;
- open completed and blocked run details;
- replay events incrementally;
- load full artifacts/context only when requested;
- resume blocked approval runs after app restart when checkpoints are valid;
- understand why a run failed, blocked, or exceeded budget.

## Default coding workflow contract

Default workflow:

```text
Start
  -> Project Summary
  -> Planner
  -> Human Approval
  -> Executor
  -> Patch Preview
  -> Patch Approval
  -> Patch Apply
  -> Check
  -> Tester / Reviewer
  -> optional retry loop
  -> End
```

### Planner

Input:

- user request;
- project summary;
- relevant knowledge chunks;
- available tools and scopes.

Output: `plan_artifact`

Required fields:

- summary;
- target files;
- required snippets;
- implementation steps;
- risks;
- recommended checks;
- executor instructions.

### Executor

Input:

- `plan_artifact`;
- selected snippets;
- constraints;
- patch schema.

Output: `patch_artifact`

Required fields:

- implementation summary;
- changed files;
- structured patches;
- risks;
- suggested check command.

### Tester / Reviewer

Input:

- `patch_artifact`;
- check output;
- changed file summary.

Output: `review_artifact`

Required fields:

- status;
- evidence;
- issues;
- risk level;
- next action.

## Safety requirements

Default behavior must be conservative. Real file mutation should require patch
preview, approval, snapshot, and rollback support.

Required gates:

- tool allowlist;
- path scope guard;
- command approval;
- network approval;
- human gate before mutation;
- patch preview before write;
- snapshot before apply;
- rollback support;
- max step count;
- max agent call count;
- max tool call count;
- token budget;
- event audit log;
- stale-base detection before patch apply;
- binary-file rejection before patch apply.

Run creation accepts optional repo-relative `scopes`. Tools must reject paths
that escape the selected project or selected scopes.

## Current implemented state

Implemented:

- workflow/agent/node/edge schema;
- JSON-driven runner;
- real edge condition routing;
- first-class `loop` node type in backend and frontend;
- loop runtime state and loop events;
- workflow step/tool/agent/token limits for loop safety;
- human approval gate;
- compact agent context policy;
- inspectable `agent.context_packet` events before agent calls;
- agent-declared `artifact_type` contracts for default coding workflow agents;
- runtime validation for `plan_artifact`, `patch_artifact`, and
  `review_artifact`;
- compact `artifact.produced` and `artifact.validation_failed` runtime events;
- estimated token tracking;
- mock agent executor when credentials are missing;
- React + TypeScript + Vite frontend scaffold;
- React Flow workflow canvas with node/edge rendering;
- workflow node creation and node inspector editing;
- edge inspector editing;
- workflow JSON editor, import, export, save, and reload path;
- workflow library list/load/save UI;
- agent list, basic agent editor, local agent save, and library agent import;
- live run launcher from the UI;
- SSE run event timeline with event payload details and compact run summary;
- approval-required resume action in the run timeline;
- command-specific approval keyed by command and working directory;
- persisted approval audit records for human, command, and MCP approvals;
- approval rejection path for live runs;
- project scope selection for runs;
- path guard enforcement for scoped tools;
- built-in project, patch, apply, rollback, check, and MCP tools;
- expanded OpenAI-compatible provider presets;
- patch proposal, scoped patch apply, snapshot, and rollback primitives;
- stale-base detection and binary-file rejection before patch apply;
- patch/diff, apply, check, and rollback display in the UI run panel;
- runtime summary panel with health, tool count, live runs, and stored runs;
- run history detail loading for stored and live runs;
- split stored run metadata, compact result, and JSONL event log files;
- paginated stored run event replay API;
- reattach to queued/running live run event streams from the browser;
- gate-specific approval resume;
- frontend i18n foundation for Chinese labels with English internal schema;
- template-first frontend entry;
- readable Chinese canvas node labels;
- loop node creation, loop inspector fields, and ContextPacket cards;
- stored ContextPackets externalized from event logs with compact summaries;
- on-demand stored ContextPacket loading in the run event panel;
- stored Artifacts externalized into per-run `artifacts/` directories with
  compact result references;
- content-addressed Blob storage for large artifact values;
- on-demand stored Artifact and Blob API endpoints;
- Artifact cards in the run event panel;
- lightweight `POST /api/v2/workflows/validate` preflight API;
- lazy loading for additional stored run events in the UI;
- FastAPI runtime API;
- live background runs;
- live run snapshots persisted under the local run store;
- file-backed run storage;
- local workflow/agent library storage;
- optional serving of built `frontend/dist` from the API.

## Active roadmap

Near-term work should prioritize the `Coder v0.3 - Trust Runtime` foundation:

1. Kernel contracts:
   - wire workflow preflight into the UI before live run start;
   - show preflight errors, warnings, provider status, permission summary, and
     estimated token budget;
   - enforce artifact schema failures in live recovery paths as clearly as
     synchronous runs.
2. Storage separation:
   - move large patch previews, check logs, snapshots, and raw tool output into
     Blob storage, not only large artifact fields;
   - add a lightweight run index so listing runs does not require scanning run
     directories;
   - add orphan blob cleanup when deleting historical runs.
3. Default workflow productization:
   - render PlanArtifact, PatchArtifact, and ReviewArtifact with artifact-specific
     UI sections;
   - strengthen Patch Approval to show diff, affected files, rollback status,
     and related artifact/context links;
   - keep all file writes on the patch preview -> approval -> snapshot -> apply
     path.
4. Recovery and replay:
   - expand persisted blocked run snapshots into active resume after API process
     restart;
   - add retry-current-node behavior;
   - group loop replay by iteration and show each iteration's ContextPacket and
     Artifact links.
5. Experience hardening:
   - add provider settings UI for OpenAI, DeepSeek, OpenAI-compatible base URL,
     default model, connection test, and mock mode;
   - improve project summaries and candidate check detection;
   - add run history filtering/search and standardized failure reasons.

## Non-goals for the next phase

Do not spend near-term effort on:

- public marketplace;
- arbitrary GitHub agent pack installation;
- complex free-form multi-agent chat teams;
- production desktop packaging;
- cloud sync;
- multi-user team permissions;
- PDF/Word knowledge ingestion;
- general automation across all websites;
- free-form multi-agent chat teams;
- parallel nodes and subworkflow nodes;
- GitHub automatic write capability;
- large-scale provider expansion;
- adopting any external framework as the core workflow runtime.

These can be revisited after the context, artifact, replay, and resource-budget
foundation is stable.
