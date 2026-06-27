# Retrieval Policy

Coder treats current repository facts and knowledge hints as different classes
of context.

## Core Rule

```text
RAG is a hint, not evidence.
Repo search/read/test/diff output is evidence.
```

Use native repository evidence for current code facts:

- file discovery
- text search
- bounded file reads with line ranges
- test or command output
- diffs

Use RAG and memory for knowledge hints:

- external docs
- project notes
- design decisions
- roadmaps
- prior run summaries
- user-maintained notes

If a RAG result mentions code, verify the claim with repo search/read before
relying on it.

## Evidence Classes

`repo_evidence` can support claims about current code.

`run_evidence` can support claims about commands, tests, runtime facts, logs,
or diffs produced by the current run.

`knowledge_hint` can guide planning and exploration, but it cannot prove the
current code state by itself.

Repo evidence overrides RAG, memory, and Obsidian notes when they disagree.

## Obsidian

Obsidian vaults are future user-managed knowledge sources. They can be useful
for architecture notes, roadmaps, decisions, and personal project notes, but
they are not code evidence. Full Obsidian sync belongs to a future Batch G.

## MCP

MCP is a future adapter layer, not the retrieval core. Future MCP tools should
call Coder retrieval services such as `ContextRetrievalRouter` and
`NativeRepoContextService`; they must not expose raw Chroma or BM25 endpoints
or bypass Coder ACLs. MCP adapter work belongs to a future Batch H.
