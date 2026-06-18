# Capability Packs

Coder should expose skills, tools, MCP servers, and local A2A routing through capability packs.

The product rule is simple:

```text
Users install capabilities.
The runtime expands capabilities into skills, tools, MCP servers, permissions, and A2A routing.
```

## Why

Current agent systems converge on the same pattern:

- Skills are discoverable task packages, not random text fields.
- MCP servers are connectors installed behind a product-controlled allowlist.
- A2A-style routing should be derived from agent cards and workflow edges.
- Raw JSON remains useful, but only as an advanced escape hatch.

This keeps the default user path small:

```text
choose project -> choose/recommend capabilities -> approve permissions -> run workflow
```

## Storage layout

Recommended long-term layout:

```text
src/coder_graph/builtin_skills/   built-in skill instructions
~/.coder/skills/                  user-wide installed skills
<repo>/.coder/skills/             project-specific skills
<repo>/.coder/agents/             saved Agent Cards
<repo>/.coder/workflows/          saved workflow specs
```

Skill lookup priority:

```text
project skill > user skill > built-in skill
```

## Capability schema

The MVP uses built-in Python data in `coder_graph.capabilities`. A future marketplace can use the same shape as JSON:

```json
{
  "id": "local-checks",
  "name": "Local Checks",
  "kind": "tool_pack",
  "description": "Run configured test, lint, or build commands with approval.",
  "tags": ["test", "lint", "build", "check"],
  "skills": ["choose_validation_checks", "interpret_check_output"],
  "tools": ["read", "search", "shell"],
  "mcp_servers": [],
  "permissions": {
    "read_files": true,
    "run_commands": true,
    "requires_approval": true
  },
  "risk": "medium"
}
```

## UI policy

Default UI:

- role
- goal
- provider/model/session
- permission checkboxes
- recommended capabilities

Advanced UI:

- raw Agent Card JSON
- explicit skills/tools
- MCP server JSON
- local A2A fields

## A2A policy

Coder keeps local A2A internal for now:

```text
endpoint = local://agent/<agent_id>
subscriptions = incoming workflow edge sources
message_types = <source>.result for incoming sources
```

A formal external A2A adapter can be added later without forcing regular users to configure protocol fields.
