# Hybrid RAG Tool

Batch E adds a rebuildable hybrid retrieval layer over Coder's Batch D memory
plane. Batch F treats its results as knowledge hints, not current-code
evidence. It combines local Chroma dense vector retrieval with BM25 sparse
lexical retrieval, fuses candidates with weighted reciprocal rank fusion, then
applies Coder memory ACLs as the final authority.

## Components

- `KnowledgeStore` and `AgentScopedMemoryStore` remain the source of truth under
  `.coder/memory/`.
- `BM25Index` writes sparse documents to `.coder/indexes/bm25/`.
- `ChromaVectorIndex` writes local persistent Chroma data to
  `.coder/indexes/chroma/` when `chromadb` is installed.
- `HybridRagRetriever` combines dense and sparse candidates, dedupes by stable
  memory ids, applies Batch D policies and ACLs, and returns compact
  `HybridRagResult` cards marked `evidence_kind="knowledge_hint"`.
- `CoderHybridRagSearchTool` exposes read-only search to OpenHands agents.

## Optional Dependencies

Base installs do not require a vector database or BM25 package.

```powershell
pip install -e .[rag]
```

The `rag` extra installs:

- `chromadb>=0.5.0`
- `rank-bm25>=0.2.2`

If `rank_bm25` is absent, Coder reports that dependency as unavailable and uses
a local lexical fallback for safe development tests. If `chromadb` is absent,
dense retrieval is skipped and reindex reports a warning.

## Import And Reindex

Text knowledge import remains the source-facing API:

```text
POST /api/v2/knowledge-sources/import-text
GET  /api/v2/knowledge-sources
GET  /api/v2/knowledge-sources/{source_id}/chunks
```

`import-text` returns `index_dirty: true`. Rebuild indexes explicitly:

```text
POST /api/v2/rag/reindex
GET  /api/v2/rag/status
```

Missing optional dependencies do not fail `POST /api/v2/rag/reindex`; the
response includes availability booleans and warnings.

## Retrieval Semantics

Chroma indexes `KnowledgeChunk` content only. BM25 indexes `KnowledgeChunk`
content and compact, safe `MemoryRecord` fields. The hybrid retriever uses
weighted reciprocal rank fusion:

```text
dense_weight / (dense_rank + 60) + bm25_weight / (bm25_rank + 60)
```

Default weights are `0.60 dense / 0.40 BM25`. Code-like queries shift to
`0.45 dense / 0.55 BM25`.

Hybrid RAG results are hints. If a result mentions code, Coder marks it
`requires_repo_verification=true`; agents must verify the claim with native repo
search/read before relying on it for current code state.

Indexes are candidate sources only. Final filtering always enforces:

- role and requested context ACLs
- allowed scopes and purposes from `AgentMemoryPolicy`
- non-secret sensitivity
- project, session, and run identity matching
- Task Execution cannot retrieve user or persona/style memory
- Workflow Supervisor cannot retrieve persona/style memory

## OpenHands Tool Contract

The OpenHands action schema exposes only:

```text
query
top_k
tags
include_content
```

Coder binds `memory_root`, `role`, `requested_context`, project/session/run ids,
scope paths, and token budget when the provider creates the tool. The model
cannot choose its role or bypass ACLs through tool arguments.

Tool output is compact by default: id, title, summary, evidence kind,
verification requirement, refs, ranks, score, and channels. `include_content=true`
returns bounded previews only. Raw logs, raw diffs, raw prompts, model outputs,
chain-of-thought, full documents, and secret-like markers are not returned.

For remote OpenHands agent servers, the package containing
`coder_workbench.openhands_tools.hybrid_rag_search` must be importable in the
server Python environment. Expose the package path with `OH_EXTRA_PYTHON_PATH`
or include Coder in the server image.
