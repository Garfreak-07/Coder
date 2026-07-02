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
| Snake browser gameplay DeepSeek + managed OpenHands smoke | Passed | Recorded 2026-07-02 20:42:25 +08:00 after adding browser-level gameplay validation. Command used `scripts/live-snake-game-smoke.ps1 -Live -LoadLocalEnv -Force`, managed OpenHands runtime with internally generated executor runtime secret, provider `deepseek`, model `deepseek-v4-flash`. Result: `status: ok`, OpenHands `connected`, Planner session `pcs_a3a88493-7f06-4309-9779-aa7633005e71`, run `b47949a6-7744-4148-b3b2-fb4a2d76716e`, Start Work `completed`, `events: 134`, `timeline_items: 93`, `timeline_backend_items: 1`, `timeline_react_items: 89`, final report `completed`, final summary `147` words, `README.md`, `index.html`, `main.js`, and `style.css` created with no forbidden npm/test artifacts, `node --check main.js` passed, Playwright launched `msedge`, direction keys moved the game, no immediate Game Over occurred, opposite reversal was prevented, Restart worked, Review Changes returned 1 changeset, and `secrets_check: passed`. |
| Coder self-test suite through Planner Chat and managed OpenHands | Passed | Recorded 2026-07-02 +08:00 after the Snake E2E fix. Command used `scripts/live-coder-selftest-suite.ps1 -Live -LoadLocalEnv -Force`, managed OpenHands runtime, provider `deepseek`, model `deepseek-v4-flash`. Easy case `coder-selftest-easy-note` wrote `F:\ccc\coder-selftest-easy-note\README.md`, Planner session `pcs_7d9241c6-0764-463d-88f8-264fd5f23090`, run `ac754977-ea4a-43c2-8d91-d38040e57fc5`, Start Work `completed`, `timeline_items: 65`, Review Changes `1`, final summary `157` words. Medium case `coder-selftest-medium-js` wrote `F:\ccc\coder-selftest-medium-js\README.md` and `math.js`, Planner session `pcs_abc31123-f1d8-4c97-a241-178ee4abaa69`, run `c5e08fab-874e-4243-83d4-066ade694fba`, Start Work `completed`, `timeline_items: 150`, Review Changes `1`, final summary `145` words, and `node --check math.js` passed. Both cases kept execution behind Start Work, selected OpenHands, and passed `secrets_check`. |

### Snake Browser Gameplay Gap

Earlier Snake E2E verified file creation and JS syntax only. It did not
validate playable browser behavior. Browser interaction validation is now
required.

The Snake product smoke now opens `F:\ccc\coder-snake-game\index.html` in a
browser, waits for the canvas, presses direction keys, verifies movement
continues over multiple ticks, confirms Game Over does not appear immediately,
verifies Restart, and keeps Timeline, Review Changes, Final Summary, and
secret-redaction checks.

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
