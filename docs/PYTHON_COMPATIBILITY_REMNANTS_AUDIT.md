# Python Compatibility Remnants Audit

Recorded: 2026-07-01

Decision: no obsolete Python/FastAPI/OpenHands compatibility runtime remains in
the main product path.

## Searches Run

```powershell
git grep -n "FastAPI"
git grep -n "legacy-python"
git grep -n "coder_workbench"
git grep -n "openhands_provider.py"
git grep -n "DEEPSEEK_API_KEY"
rg --files -g "*.py"
```

## Results

- `FastAPI`: no tracked matches.
- `coder_workbench`: no tracked matches.
- `openhands_provider.py`: no tracked matches.
- `rg --files -g "*.py"`: no Python files in the repository.
- `legacy-python`: only appears in `scripts/check-rust-only-main.js` as a guard
  asserting that removed legacy docs/implementation paths must not return.
- `DEEPSEEK_API_KEY`: retained only as a provider environment fallback name in
  Rust server settings, smoke scripts, and setup/validation documentation. No
  secret value is committed.

## Guard

`scripts/check-rust-only-main.js` enforces the Rust-only main path by rejecting:

- the removed `legacy-python` compatibility implementation path
- `docs/legacy-python.md`
- root `pyproject.toml`
- frontend references to old v2/Python API switches

## Conclusion

No file was deleted in this pass because the obsolete Python compatibility
runtime had already been removed. The remaining references are intentional:

- Rust provider fallback names such as `DEEPSEEK_API_KEY`
- live smoke script environment-variable names
- setup documentation examples using placeholder values only
- Rust-only guard assertions that prevent legacy paths from returning
