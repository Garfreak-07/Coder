# Packaging Baseline

This directory contains publishable source templates for package-manager
channels. It is intentionally separate from release publishing credentials.

## npm Wrapper

`packaging/npm` provides a thin `coder-rust` bin wrapper. It checks for a
vendored binary under `packaging/npm/vendor/` first, then falls back to
`coder-rust` on `PATH`.

Dry-run:

```powershell
node packaging/npm/bin/coder-rust.js --dry-run
```

The package does not download code in `postinstall`. Release automation can add
platform binaries later, or users can install from GitHub releases with the
scripts in `scripts/`.

## Homebrew Formula

`packaging/homebrew/coder-rust.rb` is a formula template. After creating a
GitHub release, replace the placeholder SHA256 values with archive checksums
before publishing to a tap.

## Install Scripts

Local installer dry-runs:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -DryRun
bash ./scripts/install.sh --dry-run
```

The dry-runs do not download or install anything. Non-dry-run mode downloads the
matching GitHub release archive, verifies that it contains the `coder-rust`
binary, and installs it into a user-local bin directory.
Use `-TempDir`, `--temp-dir`, or `CODER_INSTALL_TMPDIR` when installer
download/extract scratch space should live on a non-system drive.
