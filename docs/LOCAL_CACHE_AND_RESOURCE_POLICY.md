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

## Browser Verifier Runtime

Browser verification may need Node.js and Playwright. It must resolve them from
Coder-owned runtime paths or explicit overrides:

- `CODER_NODE_BIN`
- `CODER_PLAYWRIGHT_NODE_MODULES`
- `CODER_BROWSER_VERIFIER_RUNTIME_DIR`
- `CODER_RUNTIME_CACHE_DIR`
- `CODER_CACHE_DIR`
- store `tmp/runtime-cache/browser-verifier`

The verifier sets `PLAYWRIGHT_BROWSERS_PATH` for its subprocess so browser
downloads stay in the verifier runtime cache. It should not install Playwright
inside the target repo solely for Coder verification.

`scripts/prepare-browser-verifier-runtime.mjs` is a developer preparation
helper and live-test preflight. It installs only into Coder's owned runtime
root, never into a target repo. Normal product surfaces should report missing
runtime state through verifier/cache diagnostics rather than asking the model
to repair the user's project or silently downloading a browser.

## Build Artifacts

Rust build output can be large. The repository-level `.cargo/config.toml`
places it in the single shared `tmp/cargo-target` directory. Developer smoke
scripts inherit that setting instead of creating one target directory per
script. An explicit environment override remains available when needed:

```powershell
$env:CARGO_TARGET_DIR="F:\bbb\coder\tmp\cargo-target"
```

This is a developer workspace choice, not a Coder user workflow. Product code
should avoid creating large build/test caches on `C:` by default, and developer
validation should not duplicate the same Cargo artifacts across script-specific
directories.

## API Endpoints

Cache endpoints:

- `GET /api/v3/cache/status`
- `POST /api/v3/cache/clear`
- `POST /api/v3/cache/reindex`
- `GET /api/v3/cache/tasks`
- `POST /api/v3/cache/tasks/{task_id}/cancel`

`cache/status` reports bucket entries, bytes, scan count, scan cap, truncation,
and browser verifier runtime diagnostics.

## Cleanup Rule

Do not use broad cleanup commands for repository hygiene. Preview exact paths,
separate durable state from disposable cache, and delete only paths that are
clearly generated or obsolete.
