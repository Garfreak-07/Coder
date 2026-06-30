# Plugin And Skills Page

The React Plugins & Skills page is deferred from the core Planner/Executor
product path. It is not shown in the main sidebar by default; the existing
React page is only reachable through the developer debug UI while the core loop
is the release focus: `Planner Chat -> Start Work -> Timeline -> Review`.

Developer debug UI is enabled only with `?debug=1` or
`localStorage.coder_debug_ui=1`.

Frontend files:

```text
frontend/src/features/plugins/PluginsPage.tsx
frontend/src/features/plugins/PluginMarketplace.tsx
frontend/src/features/plugins/InstalledPlugins.tsx
frontend/src/features/plugins/SkillDetailsPanel.tsx
frontend/src/features/plugins/McpDependenciesPanel.tsx
frontend/src/features/plugins/HooksPanel.tsx
frontend/src/features/plugins/PluginSettingsPanel.tsx
```

Backend surfaces remain available as non-core experimental/developer APIs for
local skill and plugin validation, harness loading, and future Codex-inspired
plugin work:

```text
GET  /api/v3/plugins/marketplaces
POST /api/v3/plugins/marketplaces
DELETE /api/v3/plugins/marketplaces/{name}
POST /api/v3/plugins/marketplaces/{name}/upgrade

GET /api/v3/plugins
GET /api/v3/plugins/installed
GET /api/v3/plugins/{plugin_id}
GET /api/v3/plugins/{plugin_id}/skills/{skill_name}

GET  /api/v3/skills/extra-roots
POST /api/v3/skills/extra-roots
GET  /api/v3/hooks
```

Retained developer scope:

- local marketplace list
- installed plugins and skills
- skill detail read
- enable/disable/pin/update/remove/rollback through existing skill APIs
- MCP dependency display
- hook display
- cache/settings display

Deferred from core product:

- main navigation marketplace UI
- marketplace add/remove/upgrade UI in the ordinary product path
- remote sharing
- shared-with-me
- public marketplace publishing
- cloud auth
- paid marketplace
