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
  repo-index/
  plugin-cache/
  skill-cache/
  tmp/
```

## Rules

- Session and run events are append-only JSONL.
- Payloads are redacted before persistence.
- Large payloads should be stored as blobs with bounded previews.
- Run metadata and config snapshots are immutable evidence for later review.
- Background command output is tailed and bounded.
- Subagents keep sidechain state under the run.
- Checkpoints store resumable Task and compaction state.
- Conversation turns live in a bounded in-memory cache. Each redacted user or
  assistant message is appended as one session JSONL record and can be
  recovered after restart.
- Task state is independent from Conversation state and is persisted under its
  run identifier.

## Bounds

- Durable reads reject files over 50MiB.
- JSONL page and tail reads are capped at 1000 records.
- Cache usage scans are capped at 1000 filesystem entries.
- Provider response bodies and pending stream lines are capped at 2MiB.
- Conversation sessions retain at most 64 turns in memory, send at most 20
  recent turns to the provider, and the live session cache retains at most 200
  sessions. Active sessions are not evicted.
- Transcript compaction operates on bounded event windows and records its own
  outcome.

Transcripts and write queues are incremental and bounded so a long session does
not grow memory without limit.

## Secrets

Session records reject secret-like keys and redact secret-like payloads. Do not
persist provider API keys, passwords, private keys, or raw authorization
headers in session records. Provider API keys use the OS credential store;
`settings/providers.json` contains only public settings and configured-provider
references.

## Task Compaction

Run transcripts can be compacted. Compaction records:

- contract version
- source window
- token/size estimate
- replacement entries
- circuit breaker state
- final status

After compaction, only bounded file and skill context is restored into the next
Task turn. Conversation history is independently bounded and reconstructed
from incremental session records after a server restart.
