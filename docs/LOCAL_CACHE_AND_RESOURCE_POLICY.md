# Local Cache And Resource Policy

Coder should feel lightweight. Normal use must not silently fill the system
drive or install heavy verification/runtime dependencies into a user's target
project.

## Store Roots

The server and CLI default to a workspace-local `.coder` store:

```text
.coder/
  sessions/
  runs/
  background-tasks/
  timeline/
  blobs/
  artifacts/
  settings/
  checkpoints/
  changesets/
  repo-index/
  plugin-cache/
  skill-cache/
  logs/
  tmp/
```

Use `--store` to place durable state somewhere else. Developer scripts should
prefer a repo-local or F-drive path when large live tests are expected.

Runtime cache resolution:

- explicit tool/runtime override wins when available
- `CODER_RUNTIME_CACHE_DIR` is the shared runtime cache root
- `CODER_CACHE_DIR` is the broader Coder cache root
- otherwise Coder uses `tmp/runtime-cache` under the configured store

## Durable State

Keep these until the user deletes runs or store data:

- Planner sessions
- run metadata and events
- reports
- artifacts
- blobs
- checkpoints
- changesets
- background task records
- permission/provider settings

Durable reads are bounded. A single durable file over 50MiB should be rejected
instead of loaded into memory. JSONL pages and tails are capped at 1000 records.

## Disposable Cache

These are safe to clear from the cache endpoint or by deleting the exact
previewed paths:

- `repo-index/`
- `plugin-cache/`
- `skill-cache/`
- `tmp/`

Cache usage scans are capped at 1000 filesystem entries per bucket. When the
scan is truncated, API responses must report `truncated: true` rather than
walking an unbounded tree.

## Build Artifacts

Cargo uses its standard workspace-local `target/` directory, which Git ignores.
Developers can override it for a particular environment when needed:

```powershell
$env:CARGO_TARGET_DIR="F:\bbb\cargo-target"
```

This is a developer workspace choice, not a Coder user workflow. Product code
does not create Cargo build caches in users' target projects.

## Current Validation Snapshot

The 2026-07-12 Windows development baseline measured 0.08 GB under Coder `tmp`.
This is a workspace measurement, not a product requirement.

A local server process used 12.41 MiB working set before MCP registration and
16.35 MiB with one initialized stdio connection. Its Node test server used
37.84 MiB and no child remained after unregistering; the host settled at 16.64
MiB without an explicit allocator trim. A real `deepseek-v4-flash` MCP task
completed in five provider turns, wrote and verified the exact returned value,
and used 17,367 input tokens, 15,232 cache-read tokens, and 459 output tokens.
This validates lifecycle and accounting behavior for one tool; it is not a
stress-test claim.

## API Endpoints

Cache endpoints:

- `GET /api/v3/cache/status`
- `POST /api/v3/cache/clear`
- `POST /api/v3/cache/reindex`
- `GET /api/v3/cache/tasks`
- `POST /api/v3/cache/tasks/{task_id}/cancel`

`cache/status` reports bucket entries, bytes, scan count, scan cap, and
truncation.

## Cleanup Rule

Do not use broad cleanup commands for repository hygiene. Preview exact paths,
separate durable state from disposable cache, and delete only paths that are
clearly generated or obsolete.
