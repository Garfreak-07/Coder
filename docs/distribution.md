# Distribution

Distribution currently centers on the Rust CLI, React frontend, installer
scripts, and npm wrapper.

## CLI

Primary binary:

```sh
cargo run -p coder-cli --bin coder-rust -- doctor
```

Useful commands:

```sh
cargo run -p coder-cli --bin coder-rust -- config validate --path examples/coder.yaml
cargo run -p coder-cli --bin coder-rust -- workflow preview planner-led "summarize this repo"
cargo run -p coder-cli --bin coder-rust -- workflow run --mock planner-led "summarize this repo"
cargo run -p coder-cli --bin coder-rust -- runs list --store .coder
cargo run -p coder-cli --bin coder-rust -- server --host 127.0.0.1 --port 8876
```

## Frontend

Development:

```sh
cd frontend
npm install
npm run dev
```

Release gate:

```sh
cd frontend
npm ci
npm run test
npm run build
```

## Installers

Windows dry-run:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -DryRun
```

POSIX dry-run:

```bash
bash ./scripts/install.sh --dry-run
```

Installer scratch space can be moved off the system drive:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -TempDir F:\bbb\coder\tmp\installer
```

```bash
CODER_INSTALL_TMPDIR=/path/to/cache bash ./scripts/install.sh --dry-run
```

## npm Wrapper

The npm wrapper should launch the Rust binary and expose dry-run behavior for CI
or package verification:

```sh
node packaging/npm/bin/coder-rust.js --dry-run
```

## Release Checklist

- `cargo fmt --all --check`
- `cargo check --workspace`
- `cargo test --workspace`
- `cd frontend && npm ci && npm run test && npm run build`
- installer dry-runs on supported platforms
- provider live smoke when release notes claim provider/runtime behavior

Generated build artifacts should stay outside committed source. Large local
build caches should use an explicit target/cache directory.
