# Product Direction

Coder is not trying to be a smarter black-box coding agent than mature products like Codex, Cursor, Copilot, or Claude Code.

Its value is a small, transparent, local-first workflow that helps people use coding agents with control:

```text
Understand → Plan → Human approval → Patch → Check → Review → Retry or stop
```

## Target users

- Individual developers who want AI help but do not trust fully automatic edits.
- Small teams that need a reviewable workflow around AI-generated code changes.
- Users who want to choose their own model provider, including local or OpenAI-compatible models.
- Windows users who prefer a lightweight local tool over a heavy platform.

## Non-goals

- Do not compete with full IDE products at the start.
- Do not hide important decisions behind a black box.
- Do not auto-edit broad project areas by default.
- Do not require Docker or complex infrastructure for the first usable version.

## Product principles

- Trust before power.
- Small before complete.
- Explicit approval before mutation.
- Deterministic checks before LLM judgment when possible.
- Multi-model by design, but dependency-light.
- Local-first, with clear paths and visible commands.

## Differentiation

Mature coding agents are strong because they combine model quality, tools, execution, context management, and product polish.

Coder should differentiate by being:

- transparent;
- configurable;
- model-provider friendly;
- scope-limited;
- auditable;
- easy to understand;
- safe by default.

The product should feel less like a magic developer replacement and more like a careful AI coding workflow runner.

