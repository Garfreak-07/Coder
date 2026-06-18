# Visual Module Map

The visual module map is the entry point for ordinary developers who do not know which files to modify.

## Intended UX

```text
Import project folder
  ↓
Scan project
  ↓
Generate clickable module map
  ↓
User selects one or more modules
  ↓
Coder works inside that scope
  ↓
Auto-loop up to 3 times
  ↓
Stop only when done, blocked, out-of-scope, or high-risk
```

## First version

The first version generates a static local HTML file:

```powershell
langgraph-coder --repo "D:\projects\some-app" --map-only
```

Artifacts:

```text
outputs/module-map.json
outputs/module-map.html
```

This keeps the project small. No server, no database, no heavy frontend framework yet.

## Module scoring

Each module has two visible labels:

- importance: how central this module appears to be;
- risk: how dangerous broad edits may be.

These are separate because an important module is not always the best first place to edit.

## Confirmation policy

After a user selects a module, Coder should not repeatedly ask for permission during normal low-risk work.

It should stop and ask only when:

- the plan needs files outside the selected module;
- a high-risk module is affected;
- dependencies/config/build files must change;
- files must be deleted or moved;
- checks fail after 3 loops;
- the reviewer marks the change as high risk.

