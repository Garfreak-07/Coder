# Codex-Style Timeline

Coder projects run events through:

```text
GET /api/v3/runs/{run_id}/timeline
```

The endpoint contract is:

```json
{
  "run_id": "run-id",
  "items": []
}
```

`items` is always an array. Empty or just-created runs return an empty array,
not `null`, and normal responses do not include alternate field names such as
`timeline`, raw event envelopes, or backend payload JSON.

The response contains stable public items:

- `planner_message`
- `reasoning_summary`
- `plan_update`
- `executor_step`
- `tool_call`
- `command_execution`
- `file_change`
- `approval`
- `verification`
- `final_summary`

The timeline is a public progress narrative. It shows what the Planner or
Executor did, what tools ran, what changed, what was checked, and what happened
next. It must not expose raw chain-of-thought.

The projector summarizes existing run events and report data. It does not
expose raw backend payloads, raw OpenHands JSON, or private chain-of-thought by
default. Debug replay remains available only in the React debug UI.

Frontend rendering lives in:

```text
frontend/src/features/work-timeline/WorkTimeline.tsx
frontend/src/features/work-timeline/timelineTypes.ts
frontend/src/features/work-timeline/timelineAdapter.ts
```

The timeline appears only after Start Work starts execution. Chat turns never
start runs and do not show an empty timeline. Raw event cards are not part of
the ordinary chat view.

The frontend renderer is compact by default. It should show short public
labels, clear completed/blocked/failed states, command/cwd/exit/duration
metadata, and changed file paths. Command stdout/stderr previews are
expandable; raw backend JSON, raw OpenHands payloads, and blob refs are not
shown in the normal timeline.

Final summaries are written as Planner-style public summaries grounded in the
run report. They must cover the request, completed work, changed files,
verification/checks, evidence availability, remaining blockers or risks, and
next steps. When live Planner polishing is unavailable, deterministic summaries
must say when evidence is missing rather than fabricating checks or changes.

Executor entries should reflect the public ReAct loop:

```text
backend selected -> Reasoning summary -> action selected -> tool started/completed -> observation -> next step
```

The public executor lifecycle event kinds are:

- `backend.selected`
- `backend.blocked`
- `executor.reasoning_summary`
- `executor.action_selected`
- `tool.started`
- `tool.completed`
- `observation.recorded`
- `executor.next_step`
- `executor.completed`
- `executor.blocked`
- `executor.failed`

Each event uses the RunStore event envelope for `run_id` and `timestamp`, and
the payload carries `workflow_id`, `node_id`, `agent_id`, `harness_id`,
`backend`, `step`, `summary`, status, action/tool names when applicable, and
evidence refs when available.

OpenHands is the required backend for full coding-agent Start Work tool loops.
Native Rust events are limited deterministic scaffolding for tests and local
smoke checks. If OpenHands is unavailable, the timeline must show `Executor
backend: blocked - OpenHands not reachable` rather than silently switching
runtimes.
