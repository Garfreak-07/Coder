# Coder Handoff

## Current branch

- Branch: `codex/v02-template-i18n`
- Base: `main`
- Reason: `docs/requirements.md` states PR #1 and PR #2 are merged and new unrelated work should start from updated `main` on a new branch.

## Latest completed work

Implemented the first part of the MVP v0.2 "frontend i18n foundation and template-first entry" track:

- Added `frontend/src/i18n.ts` with a minimal Chinese UI dictionary.
- Kept workflow JSON, API fields, node types, tool names, and internal schema identifiers in English.
- Added template card metadata and safe template instantiation in `frontend/src/template.ts`.
- Added a template-first entry surface before the saved workflow library:
  - default coding workflow template;
  - blank advanced workflow template.
- Localized ordinary-user UI surfaces in `frontend/src/App.tsx`:
  - app title;
  - template entry;
  - workflow library;
  - run launcher;
  - runtime summary;
  - JSON advanced panel heading/actions;
  - inspector labels;
  - agent editor labels;
  - run event empty state.
- Added readable Chinese canvas node labels while retaining internal node IDs.
- Added CSS for template cards and field help text.

## Verification

Passed:

```powershell
cd frontend
npm.cmd run build
```

Passed:

```powershell
.\.venv\Scripts\python.exe -m unittest discover -s tests
```

Note: `python -m pytest` was not available because `pytest` is not installed in the local virtual environment. The existing tests are `unittest` tests and pass through the standard library runner.

## Known limits after this handoff

- The i18n layer is intentionally minimal. Some runtime status strings and event payloads still show provider/runtime English because they come from backend event types and schema fields.
- Provider settings UI is not implemented yet.
- ContextPacket data model/events are not implemented yet.
- Local `.md` / `.txt` knowledge base is not implemented yet.
- Restart-resume for blocked runs exists as persisted snapshots/listing, but full active resume after API process restart is still roadmap work.

## Recommended next direction

1. Add ContextPacket runtime model and event emission.
   - This is next in the active v0.2 order after i18n/template entry.
   - Emit inspectable context packet events before each agent call.
   - Keep packets compact and provenance-oriented.
2. Add a frontend ContextPacket viewer in the run event panel.
   - Chinese readable labels.
   - Show task, upstream artifacts, project context, knowledge chunks, allowed tools, token estimate, and output contract.
3. Add default coding workflow artifact schemas.
   - `plan_artifact`
   - `patch_artifact`
   - `review_artifact`
4. Then add provider settings UI for OpenAI/DeepSeek.
   - Do not store keys in workflow JSON.
   - Include mock mode and connection testing.

The next technically coherent PR should probably be "ContextPacket events and UI inspection", not provider settings. ContextPacket inspection is mandatory for trust/debuggability and is directly tied to the default multi-agent workflow.
