# MVP v0.2 Requirements

## Goal

v0.2 should turn the current prototype into the first usable foundation for a
local-first Agent workflow app.

The central feature is manual/template workflow building, not a single coding
agent. The default library should contain one polished coding workflow that
demonstrates the foundation:

```text
Planner -> Executor -> Tester / Reviewer
```

## User choices

Product assumptions for v0.2:

- target user: someone who knows some code;
- primary scenario: build and run local project workflows;
- workflow creation: ordinary users start from templates;
- advanced editing: users can manually adjust canvas nodes and edges;
- deployment: local-first;
- model setup: user provides OpenAI or DeepSeek API key;
- knowledge: project code summary plus user document knowledge base.

## User-facing flow

```text
open app
  -> configure model provider
  -> select local project
  -> choose default coding workflow template
  -> optionally edit agents and connections
  -> run workflow
  -> inspect each agent's context packet
  -> approve risky actions
  -> inspect outputs, patch, checks, and run history
  -> save workflow
```

## Required product surfaces

### Chinese UI with stable internal English schema

The UI should use Chinese labels for ordinary users. Internal schema names,
API fields, workflow JSON, tests, and code identifiers should remain English.

Implementation direction:

- add a minimal frontend i18n dictionary;
- keep workflow JSON fields unchanged;
- show advanced English identifiers only in advanced panels.

### Template library

v0.2 needs one polished template:

- name: default coding workflow;
- agents: Planner, Executor, Tester / Reviewer;
- tools: project index, patch proposal, patch apply, rollback, checks;
- gates: human approval before implementation and before patch apply;
- outputs: structured artifacts.

The UI should present this as a template card before exposing raw JSON.

### Manual workflow canvas

The existing canvas should become a product surface:

- add readable node labels;
- add node type descriptions;
- show connection direction clearly;
- validate missing agents/tools/edges before run;
- keep JSON editor as advanced mode;
- preserve manual edge editing.

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
- permission policy.

### Model settings

v0.2 should support user-provided keys:

- OpenAI API key;
- DeepSeek API key;
- optional OpenAI-compatible base URL;
- default model;
- test connection action;
- local mock mode when keys are missing.

API keys must not be stored in workflow JSON.

### Context packet inspection

Every agent run should show:

- received task;
- upstream artifacts;
- selected project files/snippets;
- retrieved document chunks;
- allowed tools;
- token estimate;
- output artifact.

This is mandatory for trust and debugging.

### Project summary

The app should build or reuse a project summary:

- file tree;
- important files;
- detected framework;
- candidate test/build commands;
- module summaries when available.

### User document knowledge base

MVP scope:

- local `.md` and `.txt` documents;
- local storage;
- chunking;
- retrieval;
- provenance shown in context packets.

PDF, Word, web sync, and large knowledge base management are out of v0.2.

### Run history and recovery

Users should be able to:

- open stored run details;
- inspect completed and blocked live runs;
- resume blocked approval runs after app restart if the checkpoint is valid;
- see why a run failed or blocked.

## Default coding workflow contract

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

- changes;
- changed files;
- implementation summary;
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
- next action.

## Explicit non-goals

v0.2 should not implement:

- public marketplace;
- arbitrary GitHub agent pack installation;
- complex multi-agent free-chat teams;
- production desktop packaging;
- parallel/loop/subworkflow nodes unless required for the default workflow;
- PDF/Word knowledge ingestion;
- cloud sync;
- multi-user team permissions.

## Acceptance criteria

v0.2 is successful when a user can:

1. open a Chinese UI;
2. configure OpenAI or DeepSeek provider settings;
3. select a local project;
4. choose the default coding workflow template;
5. manually inspect and adjust the workflow canvas;
6. run the workflow;
7. see each agent's context packet and output artifact;
8. approve or reject risky steps;
9. inspect patch preview, apply result, check result, and rollback option;
10. save and reload the workflow;
11. inspect run history;
12. understand token usage well enough to know why the workflow is efficient.

## Suggested implementation order

1. Documentation and architecture alignment.
2. Frontend i18n foundation and template-first entry.
3. ContextPacket data model and event display.
4. Default coding workflow artifact schemas.
5. Provider settings for OpenAI and DeepSeek.
6. Project summary improvements.
7. Local document knowledge base MVP.
8. Run history details and restart-resume for blocked runs.
