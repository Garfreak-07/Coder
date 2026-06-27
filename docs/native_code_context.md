# Native Code Context

Batch F adds a Coder-native context plane for current repository facts.

## Architecture

```text
ContextRetrievalRouter
  -> NativeRepoContextService
       -> RepoFileDiscoveryService
       -> RepoTextSearchService
       -> RepoReadService
       -> RepoEvidenceStore
  -> Hybrid RAG / memory as knowledge hints
```

The native repo services are scoped to `repo_root` and optional `scope_paths`.
They reject traversal, sensitive credential paths, binary reads, and ignored
directories such as `.git`, `.coder`, `node_modules`, `.venv`, `dist`, and
`build` by default.

## Services

`RepoFileDiscoveryService` provides fd-like discovery. It tries `git ls-files`,
then `rg --files`, then `fd`, then a Python walk fallback.

`RepoTextSearchService` provides rg-like search. It uses `rg --json` when
available and a bounded text-file fallback otherwise.

`RepoReadService` reads bounded file ranges with 1-based line refs.

`NativeRepoContextService` combines those operations and writes evidence refs
under:

```text
.coder/runs/{run_id}/repo_evidence/
```

## Context Packets

Harness context packets now separate:

- `warm.repo_evidence`
- `warm.knowledge_hints`
- `cold_refs.repo_evidence`
- existing `cold_refs.knowledge` and `cold_refs.memory`

Repo evidence is the only model-facing context class that can support current
code facts. Knowledge hints are planning context and must be verified when they
make code-like claims.

## OpenHands Tools

OpenHands receives these read-only tools:

- `coder_repo_find_files`
- `coder_repo_search_text`
- `coder_repo_read_file`
- `coder_hybrid_rag_search`

Coder binds `repo_root`, `coder_store_root`, `run_id`, and `scope_paths`.
Models cannot claim their own role, scope, or repository root through tool
arguments.

Task Execution still keeps Terminal/FileEditor/TaskTracker permissions. The
native repo tools provide safer, bounded evidence refs before or during edits.

## Non-Goals

Batch F does not implement:

- full Obsidian vault sync
- an MCP server
- raw Chroma/BM25 endpoints
- a global QueryEngine
- new executor write permissions
- a required LangChain dependency
