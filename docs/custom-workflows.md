# Custom Agents and Workflows

Coder's long-term shape is a small workflow runner, not a single fixed coding agent.

The built-in coding workflow should become one template among many:

```text
Project map → Planner Agent → Reviewer Agent → Human gate → Patch/check/review loop
```

Users should eventually be able to import their own agents and workflows.

## What to borrow from existing systems

- AutoGen Studio: declarative JSON specs, reusable components, visual debugging.
- CrewAI Flows: controlled workflows are often better than free-form agent conversations.
- LangGraph: graph state, conditional routing, human-in-the-loop checkpoints.
- Agentless: simple localization/repair/validation can outperform complex agent theater.
- OpenHands: sandboxing, lifecycle control, model-provider flexibility, and visible workspace matter.

## Coder's design choice

Use declarative workflow specs, but keep execution conservative.

Early versions should support:

```text
load spec → validate spec → show workflow → run only trusted built-in nodes
```

Do not immediately execute arbitrary user Python from imported workflows.

## Agent spec

```json
{
  "id": "planner",
  "role": "Planner Agent",
  "goal": "Create a short, scoped implementation plan.",
  "input_keys": ["user_request", "modules", "allowed_paths"],
  "output_schema": {
    "summary": "string",
    "steps": "list[string]"
  },
  "model": null,
  "tools": []
}
```

## Workflow spec

```json
{
  "id": "coding-review",
  "name": "Coding Review Workflow",
  "max_loops": 3,
  "agents": [],
  "steps": [],
  "stop_conditions": []
}
```

## Safety rules

- Imported workflow specs are data, not executable code.
- Tools must come from an allowlist.
- File writes require selected scope.
- Mutation requires snapshot support.
- High-risk or out-of-scope changes stop the workflow.
- Max loops defaults to 3.

## Why this matters

If users can design or import agents, Coder becomes a small workflow platform:

```text
Built-in templates
  + user-defined agents
  + visual workflow design
  + strict safety gates
```

That makes future workflow design faster without turning the product into an unsafe automation box.

