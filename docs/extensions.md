# Extension System

Extensions are globally installed and routed by the runtime. Users do not need
to attach every capability to every Agent.

Ordinary users still see Agents, workflows, plugins, and skills. Runtime routing
goes through `ActionGateway` so plugin, skill, context, patch, command, and MCP
paths can share budget and permission controls.

## Plugins

Plugins provide executable operations:

- Command Runner
- File Patch Service
- MCP or connector operations
- Browser or external automation
- AgentEngine packages

External-effect operations require preview and approval metadata. Runtime
execution should enter through `ActionGateway`, which reserves with
`BudgetBroker` before dispatching to services or extension runtimes.

`call_plugin` and `call_mcp` enter through `ActionGateway`. Permissioned or
medium/high-risk extension operations are blocked before execution unless an
approval is present.

## Skills

Skills provide knowledge and procedures:

- Debugging guides
- Framework checklists
- Review procedures
- Domain templates

`ExtensionRouter` routes relevant skills per work item. `ContextService`
constructs the packet and records loaded and omitted skill tokens, but
`BudgetBroker` performs the pre-execution context reservation.
Direct `load_skill` is intentionally not a public runtime action.

## API

New product endpoints:

- `/api/v2/extensions/plugins`
- `/api/v2/extensions/skills`
- `/api/v2/extensions/installed`
- `/api/v2/extensions/search`
- `/api/v2/extensions/install`

Existing `/api/v2/skills/*` endpoints remain temporary compatibility aliases.
Legacy `WorkflowSpec` paths must not become new extension integration points.

## v0.9.5 Boundary

- Ordinary users still manage Agents, workflows, plugins, and skills.
- `RunController` owns Planner loop continuation; extensions do not decide
  global run state.
- `BudgetBroker` reserves extension, context, tool, and model-call budgets
  before execution.
- `ActionGateway` is the entry point for extension-backed runtime actions.
- `AgentRun` and `AgentEngineRegistry` are the execution entry point for
  AgentEngine packages.
- Extension metadata and cache files live behind partitioned extension/cache
  stores.
- Legacy `WorkflowSpec` endpoints remain compatibility aliases, not new
  extension surfaces.
