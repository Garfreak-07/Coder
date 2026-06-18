# Research Notes

Coder should follow proven patterns from current software-engineering agent work instead of inventing heavy abstractions too early.

## References to track

### OpenHands / OpenHands SDK

OpenHands frames software agents as systems that need sandboxed execution, lifecycle control, model-agnostic routing, memory management, and user interfaces such as CLI, browser, VS Code, and APIs.

What Coder borrows:

- local-first execution;
- model-provider flexibility;
- clear lifecycle stages;
- future visual workspace;
- safety boundaries before autonomous edits.

### Agentless

Agentless argues that many software-engineering tasks do not need complex autonomous agents. A simple flow of localization, repair, and patch validation can be competitive and cheaper.

What Coder borrows:

- keep the workflow simple;
- prefer deterministic steps;
- avoid letting the LLM freely decide every next action;
- optimize for cost and interpretability.

### SWE-AGILE

SWE-AGILE focuses on dynamic reasoning context: keep short recent context, compress older reasoning into concise digests, and avoid context explosion.

What Coder borrows:

- module maps as compressed project context;
- short role-specific prompts;
- structured state instead of full chat history;
- future reasoning digests for long tasks.

## Chosen architecture

```text
Deterministic scan
  ↓
Module map / compressed context
  ↓
Planner Agent
  ↓
Reviewer Agent
  ↓
Deterministic approval / execution / checks / routing
```

The important design choice: agents do not own the whole workflow. LangGraph state and deterministic code own the workflow.

## Memory model

```text
Durable memory:
  module-map.json
  snapshots
  future project index

Task memory:
  LangGraph state
  selected scope
  planner_result
  reviewer_result

Scratch memory:
  not persisted
  not displayed
```

This keeps token use low and makes each step auditable.

