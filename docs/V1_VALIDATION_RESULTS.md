# V1 Validation Results

Recorded: 2026-07-01 02:08:25 +08:00

Validation base commit before this results record: `6fc62728`.

## Required Local Commands

| Command | Result | Notes |
| --- | --- | --- |
| `cargo fmt --all --check` | Passed | Re-run after clippy cleanup. |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed | Initial run found two clippy warnings; both were fixed, then clippy passed. |
| `cargo test --workspace` | Passed | Workspace tests passed after clippy cleanup. |
| `cd frontend; npm.cmd ci` | Passed | First attempts were blocked by Windows file locks on esbuild/Rollup files. After stopping the project-local locking processes, `npm ci` passed. |
| `cd frontend; npm.cmd run test` | Passed | Frontend product-surface tests passed. |
| `cd frontend; npm.cmd run build` | Passed | TypeScript and Vite production build passed. |
| `powershell -ExecutionPolicy Bypass -File .\scripts\smoke-rust-v3.ps1 -Store .tmp\smoke-rust-v3` | Passed | Returned `status: ok`, `validation: plumbing`, `provider_test: mock`, 4 turns, completed run, 37 timeline items, completed report, 1 review changeset, and `undo_status: undone`. |
| `powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -DryRun` | Passed | Windows target resolved to `x86_64-pc-windows-msvc`; no download/install performed. |
| `node packaging/npm/bin/coder-rust.js --dry-run` | Passed | Resolved local packaged binary and PATH fallback. |
| `git diff --check` | Passed | No whitespace errors. |

## Conditional Local Commands

| Command | Result | Notes |
| --- | --- | --- |
| `bash ./scripts/install.sh --dry-run` | Not run locally | `where.exe bash` reported no local bash executable on this Windows host. This remains covered by the Ubuntu `installer-dry-run` CI job. |
| `node scripts/check-rust-only-main.js` | Passed | Rust-only guard passed. |

## GitHub Actions

Latest visible `main` branch Actions result from `gh run list --repo Garfreak-07/Coder --branch main --limit 5`:

```text
completed success Record focused product validation CI main push 28403441290 1m19s 2026-06-29T21:20:25Z
```

This was the latest visible remote `main` run at validation time. The local
commits after that run had not yet been pushed when this file was written.

## Optional Live Tests

| Live test | Result | Notes |
| --- | --- | --- |
| DeepSeek live smoke | Passed | Ran with `.local-env.ps1`, `DEEPSEEK_API_KEY`, and proxy `http://127.0.0.1:7890`. Result: `status: ok`, provider `deepseek`, model `deepseek-v4-flash`, provider test `live`, 4 Planner turns, Start Work returned `needs_clarification` without starting a run. |
| DeepSeek Planner challenge | Passed | Recorded 2026-07-01 23:27:48 +08:00 using the user-specified medicine-theft ethics prompt through Coder Planner Chat `discuss` mode. Result: provider `deepseek`, model `deepseek-v4-flash`, provider test `live`, session `pcs_cfddc6c1-5ca4-4bd9-bb61-c800ef34319b`, `should_start_workflow: false`, and `answer_chars: 4374`. |
| OpenHands live smoke | Passed | Recorded 2026-07-01 20:47:45 +08:00 on local base commit `27ab5509`. Command used `OPENHANDS_LIVE_SMOKE=1`, local Agent Server `http://127.0.0.1:8000`, OpenAI-compatible DeepSeek model `deepseek-v4-flash`, and local proxy bypass `NO_PROXY=127.0.0.1,localhost,::1`. Result: `status: ok`, run `2718536d-950b-4415-970d-20f50844ecf2`, final report `Status: completed`, `backend_selected: 1`, `timeline_items: 77`, `timeline_react_items: 64`, `react_events: 63`, `raw_openhands_events: 31`, `result_doc_changed: 1`, `review_changes: 1`, `undo_status: undone`, and `secrets_check: passed`. No API key was written to the recorded events, report, timeline, or changes output. |
| Full path DeepSeek + OpenHands smoke | Passed | Recorded 2026-07-01 23:27:48 +08:00 after the Planner Chat DeepSeek request-body fix. Command used `scripts/live-full-path-smoke.ps1 -Live -LoadLocalEnv`, local Agent Server `http://127.0.0.1:8000`, provider `deepseek`, model `deepseek-v4-flash`, and proxy `http://127.0.0.1:7890` with local bypass `NO_PROXY=127.0.0.1,localhost,::1`. Result: `status: ok`, Planner session `pcs_c7bc9624-ea17-4bd1-8c0b-c8c280a4d445`, run `b4a4b08a-eea9-4409-9e9f-4604add1266f`, Start Work `completed`, `events: 281`, `timeline_items: 199`, `timeline_react_items: 137`, `final_summary_items: 1`, final report `completed`, `result_doc_changed: 1`, `review_changes: 1`, `undo_status: undone`, and `secrets_check: passed`. |
| Full path DeepSeek + OpenHands smoke after OpenHands-required enforcement | Passed | Recorded 2026-07-02 00:52:20 +08:00 after removing user-facing OpenHands disable/fallback controls. Command used `scripts/live-full-path-smoke.ps1 -Live -LoadLocalEnv`, local Agent Server `http://127.0.0.1:8000`, provider `deepseek`, model `deepseek-v4-flash`, and local proxy bypass for loopback traffic. Result: `status: ok`, OpenHands `connected`, Planner session `pcs_e10d554e-90b8-4aed-8805-894bf31af9df`, run `657c6116-758b-4859-9ea7-2fcfd673a4a3`, Start Work `completed`, `events: 127`, `timeline_items: 87`, `timeline_backend_items: 1`, `timeline_react_items: 58`, `final_summary_items: 1`, final report `completed`, `result_doc_changed: 1`, `review_changes: 1`, `undo_status: undone`, and `secrets_check: passed`. |

### OpenHands Live Smoke Notes

The successful live smoke used the current OpenHands Agent Server API shape:

- conversation payload includes `workspace.kind=LocalWorkspace` and local `working_dir`
- agent payload uses `kind=Agent`
- tools are mapped to Agent Canvas names: `terminal`, `file_editor`, `task_tracker`
- OpenAI-compatible DeepSeek credentials are injected through the OpenHands agent `llm` payload from environment variables and are not written to Coder metadata
- Planner Chat live DeepSeek requests disable provider thinking and set a bounded `max_tokens` value so longer non-English answers remain usable through the provider/proxy path
- OpenHands finish-tool events are recognized as `executor.completed`
- the smoke workflow is single-round so `executor.completed` ends the run instead of starting a second executor pass
- event polling uses `max_events: 100`; `limit=200` was observed to trigger HTTP 500 on this local OpenHands server
- the full path smoke drives Planner Chat in `work` mode, then starts the configured workflow and verifies Timeline, Review Changes, Undo, and secret redaction from the Coder API surface

## npm Audit Note

`npm.cmd ci` reported `1 low severity vulnerability` and suggested
`npm audit fix`. This did not block frontend tests or build.

## Known Blockers

No required local validation blocker remains after the clippy cleanup and npm
file-lock retry.

Known non-blocking items remain tracked in `docs/NON_BLOCKING_ENHANCEMENTS.md`.
