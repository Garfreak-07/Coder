# Session Persistence

Coder uses a local append-only store. The default root is `.coder` in the
workspace unless `--store` is passed.

## Layout

```text
.coder/
  sessions/
    <session_id>.jsonl
    <session_id>.seq
  runs/
    <run_id>/
      metadata.json
      project-config.snapshot.json
      events.jsonl
      report.json
      content-replacements.seq
      repo_evidence/
      artifacts/
      checkpoints/
      subagents/
  background-tasks/
    commands/
    subagents/
  blobs/
  settings/
    permissions/
  checkpoints/
    compaction/
    goals/
  changesets/
  repo-index/
  plugin-cache/
  skill-cache/
  logs/
  tmp/
```

## Rules

- Session and run events are append-only JSONL.
- Payloads are redacted before persistence.
- Large payloads should be stored as blobs with bounded previews.
- Run metadata and config snapshots are immutable evidence for later review.
- Background command output is tailed and bounded.
- Subagents keep sidechain state under the run.
- Checkpoints store resumable state such as compaction and goals.
- Planner session revisions merge workflow completion into the latest session
  instead of replacing turns that arrived while work was active.
- `work_in_progress`, `active_run_id`, and `latest_run_id` expose the run
  boundary without copying the workflow transcript into Planner history.
- Local status/cancel/confirmation/guidance turns are persisted separately from
  provider-backed Planner turns.

## Bounds

- Durable reads reject files over 50MiB.
- JSONL page and tail reads are capped at 1000 records.
- Cache usage scans are capped at 1000 filesystem entries.
- Provider response bodies and pending stream lines are capped at 2MiB.
- Planner sessions retain at most 64 turns in memory and the live session cache
  retains at most 200 sessions.
- Transcript compaction operates on bounded event windows and records its own
  outcome.

These bounds follow the same first-principles rule used in Claude Code:
transcripts and write queues must be durable, incremental, and bounded so a long
session does not grow memory without limit.

## Secrets

Session records reject secret-like keys and redact secret-like payloads. Do not
persist provider API keys, passwords, private keys, or raw authorization
headers.

## Resume And Compaction

Planner history and run transcripts can be compacted. Compaction should record:

- contract version
- source window
- token/size estimate
- replacement entries
- circuit breaker state
- final status

After compaction, only bounded file and skill context is restored into the next
turn.

Provider-backed Planner turn records include transport, fallback status,
provider request count, estimated tokens, reported input/output/total tokens,
and cache-read tokens. Local control turns intentionally have no provider
trace.
