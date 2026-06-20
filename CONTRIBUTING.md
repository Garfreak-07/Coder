# Contributing

Thanks for considering a contribution.

Coder aims to stay small, safe, Planner-led, and token-efficient. Contributions should preserve that direction.

## Principles

- Keep the ordinary workflow surface agent-only.
- Prefer simple deterministic runtime code over extra exposed nodes.
- Keep prompts short and artifact-shaped.
- Avoid broad filesystem writes.
- Never require secrets in committed files.
- Keep Planner as the only human communication and subjective decision point.

## Before submitting changes

Run:

```powershell
python -m unittest discover -s tests
python -m compileall src tests
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
