# Contributing

Thanks for considering a contribution.

Coder aims to stay small, safe, and token-efficient. Contributions should preserve that direction.

## Principles

- Keep Conversation tool-free and its context isolated from code tasks.
- Add capability runtimes only for real execution boundaries; do not add
  permanent role graphs.
- Prefer simple deterministic runtime code over extra exposed nodes.
- Keep prompts short and artifact-shaped.
- Avoid broad filesystem writes.
- Never require secrets in committed files.
- Keep permissions, budgets, cancellation, and evidence enforcement in the host.

## Before submitting changes

Run:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm --prefix frontend test
npm --prefix frontend run build
```

If you add behavior that can modify files, include:

- scope checks;
- human approval points;
- rollback or snapshot strategy;
- tests or clear verification steps.

## Commit hygiene

- Do not commit `.env`.
- Do not commit generated `outputs/`.
- Do not commit `.coder_history/`.
- Keep commits focused and easy to review.
