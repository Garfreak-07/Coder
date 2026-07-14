# Coder

Coder is a lightweight local runtime with a React client and a Rust backend.
One `SessionHost` exposes two isolated execution paths on the same provider and
storage stack:

- `ConversationRuntime` provides bounded, tool-free chat.
- `CodeTaskRuntime` runs an open-ended coding task through the existing
  model/tool loop.

## Architecture

```text
HTTP / CLI -> SessionHost
             |-> ConversationRuntime -> provider -> bounded chat history
             |                      `-> OutputHub -> text / TTS / avatar
             `-> CapabilityRegistry -> CodeTaskRuntime -> TaskProfile
                                                    -> Model + Harness
                                                    -> tools / Skills / stdio MCP
                                                    -> evidence / report / changes
                                                    `-> optional CodeEvent output
```

Conversation does not plan, approve, start, or supervise code tasks. Code tasks
are created directly through `/api/v3/runs` or the CLI. A task profile directly
selects its model, instructions, runtime limits, and Harness. Iteration happens
inside the model/tool loop.

- **SessionHost** owns session correlation, active task control, cancellation,
  pause, resume, token budgets, and bounded output fan-out.
- **CapabilityRegistry** routes the built-in `code` capability without a
  permanent Agent graph.
- **Harness** owns available tools, permissions, and evidence requirements.
- **RunStore** persists task events, reports, artifacts, checkpoints,
  repository evidence, and reviewable changes.

See [Architecture](docs/ARCHITECTURE.md) for crate and runtime boundaries.

The Conversation UI streams typed output over local SSE, supports interrupt
and steer against the active turn ID, restores bounded session history, and
provides browser TTS plus a renderer-neutral avatar cue interface. Coding Tasks
remain independent and may optionally publish `CodeEvent` activity to a
Conversation session.

## Install

Requirements: Rust, Node.js, and npm.

```sh
git clone https://github.com/Garfreak-07/Coder.git
cd Coder/frontend
npm install
```

## Run

Start the Rust API:

```sh
cargo run -p coder-cli --bin coder-rust -- server
```

Start the frontend:

```sh
cd frontend
npm run dev
```

Open `http://127.0.0.1:5173`.

The default durable store is `.coder` in the current workspace. Build output
uses Cargo's configured target directory; product runtime data does not use the
Cargo target directory.

## Configuration

The default configuration is [examples/coder.yaml](examples/coder.yaml):

- `models`: provider and model capability settings.
- `harnesses`: tools, permissions, and verification boundaries.
- `task_profiles`: model, Harness, instructions, tool filters, and runtime
  limits for an executable coding profile.

```sh
cargo run -p coder-cli --bin coder-rust -- config validate --path examples/coder.yaml
cargo run -p coder-cli --bin coder-rust -- task preview code "summarize this repo"
cargo run -p coder-cli --bin coder-rust -- task run --mock code "summarize this repo"
```

Provider setup and network modes are documented in
[Provider Setup](docs/PROVIDER_SETUP.md). The app stores API keys in the OS
credential store and keeps only non-secret provider settings in the Coder
store. Keys must never appear in source files.

## Test

```sh
cargo fmt --all --check
cargo test --workspace
cd frontend
npm ci
npm test
npm run build
```

## License

MIT. See [LICENSE](LICENSE).
