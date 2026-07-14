# Review And Undo

Review Changes is exposed through:

```text
GET  /api/v3/runs/{run_id}/changes
GET  /api/v3/runs/{run_id}/changes/{change_set_id}/diff
POST /api/v3/runs/{run_id}/changes/{change_set_id}/accept
POST /api/v3/runs/{run_id}/changes/{change_set_id}/undo
```

The list endpoint contract is:

```json
{
  "run_id": "run-id",
  "changes": []
}
```

`changes` is always an array. Runs with no reviewable diff return an empty
array, not `null`, and normal responses do not include alternate names such as
`change_sets` or raw backend payload JSON.

The current baseline builds a conservative changeset from the run's repo root,
final report, and current git diff. Opening the review records that diff in a
run artifact. Review Changes is hidden when a run has no actual changes.

Undo policy:

- Undo uses `git apply -R`.
- Undo first checks the reverse patch with `git apply -R --check`.
- Undo proceeds only if the current working-tree diff exactly matches the
  recorded review diff.
- If the diff changed after review, the endpoint returns `409 Conflict`.
- Conflict responses include a file-level summary of what changed between the
  recorded review diff and the current working-tree diff.
- Coder never silently discards unrelated user changes.

Binary and untracked file handling:

- Binary changes are treated as reviewable only when they appear in `git diff`.
  The review summary records the file path, but the ordinary diff preview may
  not contain text hunks.
- Untracked files are not undone by Review/Undo unless they are represented in
  the recorded diff. Undo never runs `git reset --hard`, `git clean`, or any
  equivalent broad discard operation.
- If a user edits, adds, removes, or otherwise changes files after the review
  diff is recorded, Undo refuses and keeps the current working tree intact.

Review and undo are exposed as backend API capabilities. API clients can build
their own review surface from the changeset and diff endpoints.

Review/undo is part of the Task result path. Conversation remains
side-effect free; chat turns never create reviewable changes. Accept records
user approval of a changeset and does not remove timeline or evidence records.
