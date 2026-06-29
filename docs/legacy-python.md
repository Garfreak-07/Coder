# Historical Python/FastAPI v2 Path

Current `main` is Rust-only.

The previous Python/FastAPI v2 compatibility implementation was removed from
`main` after the Rust-only migration. It remains available in git history at
tag `pre-rust-only-legacy-v2`.

Use that tag only when you need the final pre-Rust-only compatibility state:

```powershell
git checkout pre-rust-only-legacy-v2
```

No Python/FastAPI v2 server, package, CI job, or frontend API switch is
maintained in current `main`.
