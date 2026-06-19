# Coder Handoff

## Current branch

- Branch: `codex/v02-template-i18n`
- Base: `main`
- Reason: `docs/requirements.md` states PR #1 and PR #2 are merged and new unrelated work should start from updated `main` on a new branch.

## Default collaboration rules for future Codex conversations

- Start new implementation work from updated `main` on a new `codex/...` branch by default.
- Commit completed work to Git.
- Push the branch to GitHub by default.
- Try to create a draft PR. If automated PR creation is blocked by auth or app permissions, provide the GitHub PR creation link to the user.
- Update this handoff document by default before the final commit/push.
- Keep documenting verification commands and known limits in this file.

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
- Workflow loops are not a first-class node type yet.
  - Current runtime can technically revisit nodes through edges that point back to earlier nodes.
  - `EdgeSpec.max_traversals`, `WorkflowSpec.max_steps`, `max_agent_calls`, `max_tool_calls`, and token budget limits provide basic safety against infinite loops.
  - However, `NodeType` does not include `loop`, the frontend cannot create a loop node, and there is no dedicated loop state such as iteration index, loop item, break reason, or per-iteration artifact summary.
- Provider settings UI is not implemented yet.
- ContextPacket data model/events are not implemented yet.
- Local `.md` / `.txt` knowledge base is not implemented yet.
- Restart-resume for blocked runs exists as persisted snapshots/listing, but full active resume after API process restart is still roadmap work.

## Loop architecture decision

The product should support loop as an explicit v0.2 workflow capability, not only as a graph trick.

Current status:

- Supported today: conditional edges can route back to an earlier node, with traversal limits.
- Not supported today: an explicit `loop` node or structured loop contract.

Why edge-only looping is insufficient:

- It is hard for ordinary users to understand on the canvas.
- It does not expose iteration state cleanly in run events.
- It makes ContextPacket construction harder because the runtime cannot tell which artifacts belong to the current iteration.
- It is easy to create confusing retries without clear exit reasons.

Recommended loop design:

1. Add `loop` to backend and frontend `NodeType`.
2. Add loop node config fields:
   - `mode`: `while` | `for_each` | `retry_until`
   - `condition`: expression for `while` / `retry_until`
   - `items_key`: state key for `for_each`
   - `item_key`: current item output key
   - `iteration_key`: current iteration number output key
   - `max_iterations`: required, conservative default such as 3 or 5
   - `collect_key`: optional key for collected per-iteration outputs
   - `summary_key`: optional compact summary for ContextPacket reuse
3. Add runtime loop state:
   - iteration count by loop node ID;
   - current item;
   - collected outputs;
   - break reason: condition false, max iterations, approval rejected, error, or downstream block.
4. Add loop events:
   - `loop.started`
   - `loop.iteration.started`
   - `loop.iteration.completed`
   - `loop.completed`
   - `loop.blocked`
5. Add UI support:
   - loop node label and inspector fields;
   - visual indication of loop-back edge;
   - run timeline grouping by iteration;
   - ContextPacket viewer showing only current iteration plus compact prior iteration summaries.
6. Keep safety hard limits:
   - workflow `max_steps`;
   - loop `max_iterations`;
   - edge `max_traversals`;
   - agent/tool call budgets;
   - token budget.

For the default coding workflow, the useful loop is:

```text
Tester / Reviewer
  -> condition: review.status == "needs_changes"
  -> Human Approval
  -> Executor
  -> Patch Preview
  -> Patch Approval
  -> Patch Apply
  -> Check
  -> Tester / Reviewer
```

The first implementation can use conditional back edges plus `max_traversals` as an interim version, but the target architecture should still add first-class loop nodes before treating loop as complete.

## Recommended next direction

1. Add first-class loop architecture to the planning docs and implementation roadmap.
   - User explicitly wants loop support.
   - Current implementation only has limited back-edge looping, not a loop node.
2. Add ContextPacket runtime model and event emission.
   - ContextPacket should include loop iteration state from the start.
   - Emit inspectable context packet events before each agent call.
   - Keep packets compact and provenance-oriented.
3. Add a frontend ContextPacket viewer in the run event panel.
   - Chinese readable labels.
   - Show task, upstream artifacts, project context, knowledge chunks, allowed tools, token estimate, and output contract.
4. Add default coding workflow artifact schemas.
   - `plan_artifact`
   - `patch_artifact`
   - `review_artifact`
5. Then add provider settings UI for OpenAI/DeepSeek.
   - Do not store keys in workflow JSON.
   - Include mock mode and connection testing.

The next technically coherent PR should probably be "first-class loop node and loop-aware ContextPacket plan", then implement ContextPacket events/UI. ContextPacket inspection remains mandatory for trust/debuggability, but loop state should be included before the ContextPacket shape hardens.
