# Distribution Baseline

This is the release baseline for the Rust-first Coder track. It documents the
intended artifact shape without making installer polish a blocker for runtime
parity.

## CLI Surface

The Rust binary is `coder-rust`. The required baseline commands are:

- `coder-rust doctor`
- `coder-rust config validate`
- `coder-rust workflow validate`
- `coder-rust workflow preview`
- `coder-rust workflow run`
- `coder-rust runs list`
- `coder-rust runs show`
- `coder-rust server`
- `coder-rust openhands doctor`
- `coder-rust tools ...`

The command surface is covered by Rust CLI tests so these entrypoints do not
disappear accidentally.

## Release Artifacts

Initial GitHub releases should publish compressed binaries for:

- Windows x86_64: `coder-rust-x86_64-pc-windows-msvc.zip`
- macOS Apple Silicon: `coder-rust-aarch64-apple-darwin.tar.gz`
- macOS Intel: `coder-rust-x86_64-apple-darwin.tar.gz`
- Linux x86_64 GNU: `coder-rust-x86_64-unknown-linux-gnu.tar.gz`

Each archive should contain:

- `coder-rust` binary, or `coder-rust.exe` on Windows
- `README.md`
- `LICENSE`
- `examples/coder.yaml`

## Deferred Installers

Do not block the migration on package-manager polish. These should come after
the Rust v3 product path is validated:

- PowerShell install script
- POSIX install script
- npm wrapper package
- Homebrew tap
- signed Windows/macOS artifacts

## Release Checks

Before publishing release artifacts, run the final verification set from the
master migration plan:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
.\.venv\Scripts\python.exe -m pip install -e ".[openhands]"
.\.venv\Scripts\python.exe -m unittest discover -s tests
.\.venv\Scripts\python.exe -m compileall src tests
cd frontend
npm.cmd ci
npm.cmd run test
npm.cmd run build
```

Live OpenHands tests remain optional and must stay environment-gated.
