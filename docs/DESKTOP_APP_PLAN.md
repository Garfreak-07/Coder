# Desktop App Plan

Coder's first desktop app should be a thin local shell around the existing
Planner/Executor product path. Coder is Codex split into Planner and Executor:
Planner owns user conversation and public summaries, while Executor performs
ReAct work through harnesses. The target is Tauri because Coder is Rust-first
and already has a React frontend plus Rust API v3 runtime.

## Target Architecture

The desktop app should keep the existing product architecture:

- Tauri launches the native app window.
- React remains the only ordinary UI surface.
- Rust API v3 is bundled with the app as an embedded service or sidecar.
- The UI connects to the local Rust API automatically.
- Runtime state stays local under the selected workspace's `.coder/` data
  directory.
- Provider API keys move from the current in-memory development store to OS
  keychain or an equivalent local secret store before public desktop release.
- OpenHands remains the required Start Work executor. Desktop may connect to an
  already-running local or remote OpenHands Agent Server first, then later add a
  managed helper that launches it for the user.

Normal users should not run `cargo`, run `npm`, or set environment variables.
Opening the desktop app should start the Rust runtime automatically, load the
React UI, and keep the same core flow:

```text
Provider Settings
-> LLM-backed Planner Chat
-> Start Work
-> Executor ReAct loop through OpenHands
-> Work Timeline
-> Review Changes / Undo
-> Final Summary
```

Chat turns must remain side-effect free. Start Work is the only execution
boundary.

## First Milestone

The first milestone is intentionally small:

- Tauri opens a desktop window using the existing React production build.
- Rust API v3 runs as either a sidecar process or an embedded local service.
- React discovers the local API endpoint without manual user configuration.
- Users can open the app and use Planner Chat without running `cargo` or
  `npm`.

This milestone should prove packaging and process orchestration only. It should
not redesign the Planner Chat flow, workflow graph, provider settings, timeline,
review changes, or OpenHands harness boundaries.

## Dev Mode vs App Mode

Current web development mode remains supported and should not be replaced:

```powershell
cargo run -p coder-cli --bin coder-rust -- server --host 127.0.0.1 --port 8876

cd frontend
npm.cmd run dev
```

In app mode, Tauri owns the orchestration:

```text
Tauri window
-> bundled React assets
-> local Rust API v3 sidecar on 127.0.0.1
-> repository workspace and .coder/ data
```

The first app-mode implementation may keep the localhost HTTP boundary. This
avoids rewriting every HTTP API into Tauri commands before the product loop is
stable. A future version may replace selected HTTP calls with direct Tauri
commands when there is a clear reliability or packaging benefit.

## OpenHands Connection Options

Desktop should expose the same OpenHands boundary as the web app:

- External OpenHands server: connect to a user-provided local or remote URL.
- Later managed OpenHands helper: launch or discover a local OpenHands service
  automatically, but only after the external-server path is stable.

OpenHands should not look optional in the product UI. CI can still use fake
OpenHands event shapes and native Rust scaffolding for deterministic plumbing,
but product Start Work must block with a clear message when OpenHands is not
reachable.

## Local Data

Desktop data should live under a local `.coder/` directory owned by the app and
the selected workspace. The expected layout is:

```text
.coder/
  runs/
  sessions/
  timeline/
  blobs/
  artifacts/
  checkpoints/
  changesets/
  repo-index/
  plugin-cache/
  skill-cache/
  openhands-events/
  logs/
  tmp/
```

Runtime data must remain disposable where possible. Reports, artifacts,
changesets, checkpoints, and evidence refs are durable user-facing records.
Temporary files, cache data, and transient OpenHands payloads can be rebuilt or
pruned.

## Secrets

Desktop release is blocked until provider API keys use OS keychain or an
equivalent local secret store. The current server-memory MVP is acceptable for
development and local smoke tests only.

Rules:

- Do not write plaintext API keys into `.coder/`.
- Do not include keys in run events, timeline items, evidence, logs, reports,
  debug exports, screenshots, or crash reports.
- Environment variables remain developer/headless fallback only.
- Provider Settings is the normal user path.

## Packaging Plan

Use staged packaging so the current web/dev workflow stays stable:

1. Add a minimal `src-tauri/` skeleton that points to the React production build
   and can open the Coder window.
2. Add desktop-only scripts such as `desktop:dev` and `desktop:build` without
   changing the existing `frontend` dev, test, or build scripts.
3. Bundle the Rust API v3 binary as a sidecar first. Keep sidecar startup,
   port selection, health checks, shutdown, and logs explicit.
4. Build unsigned local artifacts for Windows first, then add macOS/Linux once
   the sidecar and data-dir behavior are stable.
5. Add signing, notarization, installer metadata, update channels, and release
   automation only after local artifacts are reliable.
6. Keep desktop build failures out of the main release gate until the skeleton
   is stable on supported platforms.

The package should include React assets, the Rust runtime, license metadata,
and default configuration. It must not package local `.coder/` state, API keys,
workspace files, debug logs, or OpenHands secrets.

The current proof-of-concept commands are:

```powershell
npm run desktop:dev
npm run desktop:build
```

These commands use the root `package.json` and Tauri CLI. They are opt-in and
do not replace the existing Rust server plus Vite workflow.

## What Not To Do Yet

In scope for the first desktop path:

- start and stop local Rust runtime automatically
- load bundled React assets
- configure provider and OpenHands connection
- store Planner sessions and runs under `.coder/`
- preserve current localhost web dev mode

Out of scope for the first desktop path:

- broad plugin marketplace UI
- remote account sync
- cloud auth
- replacing all HTTP APIs with Tauri commands
- requiring GPU or local model acceleration
- changing the Planner-led product flow
- making desktop packaging the only supported development workflow
- silently replacing OpenHands with native fallback for product Start Work
- storing plaintext provider keys in app config, `.coder/`, logs, or reports

Mock workflows and native fallback are useful for deterministic plumbing tests.
Desktop product confidence still requires live LLM and OpenHands smoke paths
when provider credentials and an OpenHands Agent Server are available.
