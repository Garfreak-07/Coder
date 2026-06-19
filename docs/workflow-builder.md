# Workflow Builder Design

## Product model

Workflows are manually built and editable. Templates reduce setup friction but
do not replace manual control.

The default product should have one polished coding workflow template. Future
templates can be added after the default workflow proves that agent handoffs,
context packets, approvals, and run history work reliably.

## Ordinary mode and advanced mode

### Ordinary mode

Ordinary users should interact with:

- template cards;
- agent cards;
- tool toggles;
- knowledge source selection;
- provider settings;
- run button;
- approval panel;
- context/result viewer.

They should not need to edit JSON.

### Advanced mode

Advanced users can:

- open the canvas;
- add nodes;
- connect edges;
- edit conditions;
- edit JSON;
- configure contracts and policies directly.

## Default workflow

The default coding workflow:

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
  -> End
```

This remains the only required v0.2 workflow template.

## Workflow builder UI requirements

### Template card

The template card should show:

- name;
- purpose;
- agents;
- tools;
- required approvals;
- model/key requirements;
- knowledge source requirements;
- risk level.

### Agent cards

Each agent card should show:

- role;
- goal;
- input;
- output;
- allowed tools;
- token budget.

### Connection explanations

Edges should have readable explanations:

- what artifact passes through;
- when the edge is active;
- what happens if the condition fails.

### Validation panel

Before a run starts, the app should validate:

- workflow has start and end;
- all edges reference existing nodes;
- all agent nodes reference existing agents;
- all tools exist and are allowed;
- provider settings are available;
- required knowledge sources are indexed;
- output artifact schemas are defined;
- risky actions have approval gates.

## AI-assisted workflow creation

AI-assisted workflow creation should be constrained. The agent should recommend
or modify templates instead of freely inventing an arbitrary graph.

Recommended flow:

```text
user describes goal
  -> system selects closest template
  -> AI proposes agent roles and tool choices
  -> user confirms
  -> workflow JSON is generated from a known template
  -> user can manually edit
```

This keeps the foundation reliable while still giving users AI help.

## Why not free-form graph generation first

Free-form graph generation is attractive but risky:

- generated workflows may be invalid;
- tool permissions may be unsafe;
- edge conditions may be wrong;
- agent outputs may not match downstream inputs;
- debugging becomes hard for ordinary users.

Template-constrained generation gives better reliability.

## Capability integration direction

Tools, MCP servers, skills, and GitHub agent packs should not be separate UX
concepts at first. The UI should group them under "capabilities".

Each capability should declare:

- what it does;
- required permissions;
- input/output schema;
- risk level;
- install source;
- runtime adapter.

Workflow nodes can then call capabilities through adapters.

## Future workflow types

After the default coding workflow is reliable, add templates in this order:

1. project analysis workflow;
2. bug investigation workflow;
3. documentation QA workflow;
4. PR review workflow;
5. knowledge-base Q&A workflow.

Each new template should reuse ContextPacket, artifacts, provider settings,
knowledge retrieval, and approval gates.
