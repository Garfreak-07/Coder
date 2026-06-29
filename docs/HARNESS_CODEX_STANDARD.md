# Harness Codex Standard

A product-grade harness must make agent work traceable, permission-bound, and
evidence-backed. "Codex-grade" means the agent can do real work appropriate to
its role; it does not mean every role can use every tool.

## Minimum Standard

Context and repo evidence:

- list files
- search text
- read safe full files
- read bounded file ranges
- capture git status and git diff
- bound large output and store refs

Patch and edit capability:

- preview patch effects
- apply patches only when policy permits
- request approval before writes when policy says `ask`
- report changed files from runtime evidence, not model claims
- store patch refs and failure evidence

Command capability:

- execute argv-only commands
- enforce cwd inside repo boundary
- apply timeout and output bounds
- request approval before commands when policy says `ask`
- record command evidence

Permission and approval:

- read-only harnesses cannot write
- review-only harnesses cannot run commands
- denied actions produce blockers
- ask-mode actions emit approval requests
- secrets are redacted from events, refs, and reports

Memory and knowledge:

- read only allowed scopes
- write only allowed scopes
- long-term memory proposals and confirmations are Planner Chat only
- workflow supervisor and task execution roles can write only run/workflow memory
- knowledge hints are not current-code evidence
- repo evidence overrides stale memory

Events and reports:

- tool calls emit started/completed/failed/blocked-style events
- large raw outputs use blob or evidence refs
- final reports are built from events, repo evidence, patches, checks, and raw
  backend refs
- changed files, patch refs, and checks cannot come from model claims alone

## Native Rust Backend

`NativeRustBackend` is the offline/local fallback. It supports repo evidence,
command preview/run, patch preview/apply, approval requests, bounded output,
and evidence-backed reports.

It is not a hidden second full coding-agent runtime competing with OpenHands.
Its purpose is deterministic local work, policy preflight, evidence capture,
and development without a live OpenHands server.

## Planner Model Backend

`planner-model` is a read-only Planner Conversation Harness backend. It is not a
coding-agent runtime and does not edit files, run commands, publish, deploy,
commit, push, or write long-term memory. Product Planner Chat resolves it from
the selected workflow's planner node and uses its AgentSpec, HarnessSpec, memory
scopes, read-only tools, and model profile as the conversation boundary.

When a confirmed Work run enters the workflow graph, `planner-model` records
planner boundary events and returns a `ready` transition so the executor can run
through the configured executor harness.

## OpenHands Backend

For `backend = openhands`, OpenHands owns the coding-agent loop and runtime
capabilities. Coder owns orchestration and policy around that runtime:

- build OpenHands conversation payload from AgentSpec, HarnessSpec, workflow,
  model, permissions, memory, verification, and plan context
- trigger or attach to OpenHands conversations
- stream or poll raw events
- store raw events as blob refs
- normalize OpenHands events into Coder events
- build an evidence-backed final report

Coder must not duplicate OpenHands terminal, file editing, browser/computer-use,
or task-runtime capabilities when OpenHands already provides them.

## Test And Mock Boundary

`NativeMockBackend`, `/api/v3/runs/mock`, and mock MCP operations exist for
deterministic CI, smoke tests, and development. They must not be described as
real product execution capabilities.
