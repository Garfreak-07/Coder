# Architecture

## Product Boundary

Coder exposes two isolated runtimes through one `SessionHost`:

1. Conversation keeps a bounded user-facing chat history and calls the selected
   provider without repository or execution tools.
2. `CodeTaskRuntime` executes a task profile through one Harness and the
   open-ended model/tool loop.

The host owns session identity, output fan-out, task creation, permissions,
budgets, cancellation, and durable evidence. Conversation is not a task
authority and does not transform user messages into execution plans.

## Runtime Flows

```text
Conversation API
  -> SessionHost
  -> ConversationRuntime
  -> one active turn per session
  -> interrupt watch channel / queued steer input
  -> bounded provider context
  -> OpenAI-compatible provider stream
  -> incremental session records
  -> OutputHub -> SSE -> text / speech / avatar sinks
```

```text
Task API / CLI
  -> SessionHost
  -> CapabilityRegistry (`code`)
  -> CodeTaskRuntime resolves TaskProfile
  -> Harness + Model
  -> shared model/tool loop
  -> built-in tools, Skills, subagents, frozen stdio MCP snapshot
  -> append-only events and repository evidence
  -> optional session-bound CodeEvent output
  -> final report and reviewable changes
```

Quality iteration belongs inside the model/tool loop. Verification is evidence
policy and tool output, not a separate permanent role.

## Core Types

`ProjectConfig` contains models, harnesses, and task profiles. A `TaskProfile`
directly contains its model and Harness IDs, instructions, optional tool
filters, runtime policy, and optional token budget. There is no permanent
Agent configuration layer or task binding indirection.

`SessionHost` owns bounded or short-lived state:

- bounded Conversation sessions and active-turn identity
- a bounded per-session `OutputHub` with monotonic sequence numbers
- capability routing
- pause, resume, and cancellation signals
- token budget accounting
- active/inactive lifecycle

`RunStore` owns durable state: incremental Conversation message records,
immutable Task config snapshots, JSONL events, reports, blobs, artifacts,
checkpoints, repository evidence, changesets, and compacted transcript
records.

Conversation messages are redacted before they are appended to session JSONL.
The in-memory session cache can recover from those records after restart.
Active sessions are not selected for cache eviction.

## Output Protocol

`coder-events` defines the transport-independent `OutputEnvelope`. It contains
session and optional turn identity, a per-session sequence, source, priority,
and one typed output event. Current events cover:

- session and turn lifecycle
- text start, delta, and completion
- speech intent start, token, end, and cancellation
- avatar emotion and motion cues
- nested durable `CodeEvent` records
- recoverable output errors

`OutputHub` uses a bounded broadcast channel. Slow consumers receive an
explicit lag notification instead of causing unbounded memory growth. SSE is
only the current external adapter. The default channel retains 1024 envelopes
per session.

The React client treats text, speech, avatar, and code activity as independent
sinks. Browser speech follows AIRI's `queue`, `interrupt`, and `replace`
priority semantics. `AvatarDriverHub` is renderer-neutral so a future Live2D,
VRM, or other renderer can register without changing Conversation Runtime.

## Capability Extension

`CodeTaskRuntime` remains independent from Conversation. A Task may optionally
carry a `session_id`; when present, its durable `CodeEvent` records are also
published to that session's Output Hub.

A future deterministic game Runtime should follow the same boundary:

1. Keep real-time game state and decisions inside the domain Runtime without
   model calls.
2. Expose controlled operations through a local MCP adapter when tool access
   is useful.
3. Publish user-facing output through `SessionHost` rather than through chat
   history.
4. Use a coding Task only to inspect evidence and improve the Runtime or
   strategy implementation.

No game-specific profile, protocol, or permanent Agent is registered until a
concrete Runtime exists.

## Crates

- `coder-config`: configuration, model limits, tool resolution, and validation.
- `coder-core`: identifiers, run state, final reports, and shared contracts.
- `coder-events`: append-only event records.
- `coder-tools`: repository, git, command, patch, and evidence operations.
- `coder-harness`: backend and execution boundary contracts.
- `coder-workflow`: coding task runner and shared model/tool loop.
- `coder-memory`: project memory and knowledge retrieval.
- `coder-extensions`: Skills, plugins, and local stdio MCP support.
- `coder-store`: durable local storage and bounded cache operations.
- `coder-server`: `SessionHost`, runtime routing, APIs, provider transport, and
  tool dispatch.
- `coder-cli`: server, configuration, task, run, and tool commands.

## Permissions And Tools

Each model-visible Task tool call passes through the host:

1. Resolve the frozen tool snapshot.
2. Validate arguments and repository scope.
3. Apply Harness and session permission policy.
4. Request approval when policy says `ask`.
5. Execute and persist bounded output and evidence.
6. Return the observation to the model.

Creating a Task is not approval for side effects. Model-supplied approval and
sandbox flags are never authority. Provider secrets are owned by provider
transport, persist in the OS credential store, and are not inherited by model
commands. The Coder store contains only non-secret provider settings and
configured-provider references.

MCP support is local stdio. Tool discovery is frozen at Task start.

## Context And Cost

Conversation and Task use separate histories. Conversation retains at most 64
turns and sends at most 20 recent turns to the provider. Task context,
compaction, and token limits are resolved from the selected model and task
profile.

Task execution has a profile-resolved provider-turn limit. Near the boundary,
the Runtime inserts one completion reminder so the model can stop optional
work and return a terminal status. Reaching the hard limit without that status
is blocked even when files were written; file changes alone are not proof of
task completion.

Conversation provider responses are capped at 2 MiB. Provider stream idle
timeout is 300 seconds, matching the Codex transport default used by Coder.

## API Surface

- `/api/v3/conversations`
- `/api/v3/runs`
- `/api/v3/tools`
- `/api/v3/providers`
- `/api/v3/mcp`
- `/api/v3/extensions`, `/api/v3/plugins`, and `/api/v3/skills`
- `/api/v3/memory` and `/api/v3/knowledge`
- `/api/v3/cache`

Task preview and execution use `/api/v3/runs/preview` and `/api/v3/runs`.
Conversation output uses
`/api/v3/conversations/{session_id}/events`; turn control uses scoped
`interrupt` and `steer` endpoints under the active turn ID.
