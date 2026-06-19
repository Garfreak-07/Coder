# Coder Handoff

## Read first

Start every new Codex conversation by reading:

1. [requirements.md](requirements.md)
2. this handoff

`requirements.md` is the canonical product, architecture, roadmap, and current
state document. This file is intentionally short and should only hold volatile
branch/process context.

## Current branch context

- Branch: `codex/v02-loop-context-packets`
- Current branch includes loop ContextPacket work, compact context budget
  enforcement, split stored run event logs, and stored ContextPacket
  externalization.
- Current `main` / `origin/main`: `92452c0 Add v0.2 template-first Chinese UI foundation (#3)`
- Current open PR context from the previous handoff: PR #5,
  `codex/v02-loop-context-packets` -> `main`

If the next task is unrelated to PR #5, start from updated `main` on a fresh
`codex/<short-task-name>` branch. If the next task depends on PR #5, say that
the work is stacked on PR #5 before making changes.

## Documentation cleanup status

The old split planning documents have been merged into
`docs/requirements.md`:

- `docs/product-vision.md`
- `docs/foundation-architecture.md`
- `docs/context-memory-rag.md`
- `docs/workflow-builder.md`
- `docs/mvp-v0.2.md`

The old v0.2 milestone is no longer the active product target. It should be
treated as historical implementation context. The active direction is now the
resource-conscious runtime foundation described in `requirements.md`:
ContextPackets, structured artifacts, Blob storage, compact events, lazy replay,
content deduplication, and enforced token/resource budgets.

## Latest completed implementation

The current feature branch implements the first-class loop node and the
loop-aware ContextPacket foundation:

- backend and frontend `loop` node type;
- loop config fields for retry/while/for_each modes;
- loop runtime state stored in blocked-run checkpoints;
- loop events;
- `agent.context_packet` events before agent calls;
- frontend loop node creation and inspector fields;
- readable loop canvas labels;
- ContextPacket cards in the run event panel;
- default coding workflow routing through a review retry loop;
- `tests/test_loop_context.py`;
- empty `input_keys` no longer includes all state by default;
- recursive context compaction and blocking token budget enforcement;
- compact `node.completed` events that carry result summaries, status, keys,
  and size instead of full node results;
- split stored run layout with `metadata.json`, `result.json`, and
  `events.jsonl`;
- paginated stored run events API and lazy stored event loading in the UI;
- stored ContextPackets externalized under per-run `contexts/`;
- compact ContextPacket event payloads with summary, size, and `packet_id`;
- `GET /api/v2/runs/{run_id}/context-packets/{packet_id}` for on-demand full
  packet loading;
- `tests/test_run_storage.py`.

## Previous verification

Latest local verification:

```powershell
cd frontend
npm.cmd run build
```

```powershell
.\.venv\Scripts\python.exe -m unittest discover -s tests
```

```powershell
.\.venv\Scripts\python.exe -m coder_workbench.cli --repo . --workflow examples\workflows\coding-workbench.json --request "smoke test loop context" --approve
```

`pytest` was not installed locally; the repository tests use `unittest`.

## Known limits

- Loop support is first-class but still narrow:
  - timeline grouping by iteration is not implemented;
  - `collect_key` and `summary_key` are schema fields but do not yet collect or
    summarize per-iteration outputs;
  - richer loop-back edge visualization is not implemented.
- ContextPacket events are visible and stored packets can be loaded on demand,
  but they do not yet include local knowledge chunk provenance, explicit
  artifact schema rendering, or compact prior-iteration summaries.
- Provider settings UI is not implemented.
- Local `.md` / `.txt` knowledge base is not implemented.
- Restart-resume from persisted blocked runs is not fully active after API
  process restart.
- Run storage separation has started for stored runs, but artifacts, blobs,
  snapshots, check logs, and active restart-resume state are not split into
  their final stores yet.

## Git process

Before new implementation work:

```powershell
git status --short
git branch --show-current
git log --oneline --decorate -6
git fetch origin
```

For unrelated work:

```powershell
git switch main
git pull --ff-only origin main
git switch -c codex/<short-task-name>
```

Before opening or updating a PR, verify that the branch contains only the
intended commits:

```powershell
git log --oneline origin/main..HEAD
```

If a PR branch accidentally includes old commits, repair it locally and push
with `--force-with-lease`, never plain `--force`.
