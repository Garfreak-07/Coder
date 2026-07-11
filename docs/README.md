# Docs

These are the maintained project docs. Historical audit notes, one-off
validation records, and superseded planning docs should not be kept in `docs/`
unless they still guide current maintenance.

## Start Here

- `ARCHITECTURE.md`: product path, crate boundaries, runtime responsibilities,
  and current maintainability notes.
- `PROVIDER_SETUP.md`: normal provider setup, proxy isolation, and developer
  fallback variables.
- `AGENT_HARNESS_DESIGN_AUDIT.md`: Claude Code comparison, implemented
  alignment, weak evidence, and remaining native gaps.

## Runtime Policy

- `CAPABILITY_BOUNDARY_MATRIX.md`: tool, permission, approval, evidence, and
  timeline boundaries.
- `LOCAL_CACHE_AND_RESOURCE_POLICY.md`: durable state vs disposable cache
  policy.
- `SESSION_PERSISTENCE.md`: `.coder/` layout and append-only persistence rules.
- `REVIEW_AND_UNDO.md`: Review Changes and conservative undo behavior.

## Engineering

- `distribution.md`: CLI release artifacts, installers, npm wrapper, and
  Homebrew template.

## Repository Hygiene

When the worktree is dirty during a refactor, classify changes before cleanup:

- Keep untracked source modules that are referenced by `mod` declarations.
- Keep deleted historical docs deleted when maintained docs preserve the active
  product rule.
- Remove generated cache/build artifacts only after previewing the exact paths.
- Do not use broad `git reset` or `git clean` commands to make the tree look
  tidy; split the work into reviewable source, docs, frontend, and regression
  probe commits instead.

When a new doc is added, prefer updating one of these documents first. Add a new
file only when it has a stable owner and a distinct long-term purpose.
