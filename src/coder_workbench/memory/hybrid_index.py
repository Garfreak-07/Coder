from __future__ import annotations

import json
from pathlib import Path

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.memory.bm25_index import BM25Index
from coder_workbench.memory.chroma_index import ChromaVectorIndex
from coder_workbench.memory.embeddings import EmbeddingProvider
from coder_workbench.memory.store import AgentScopedMemoryStore, KnowledgeStore


class HybridIndexStatus(BaseModel):
    model_config = ConfigDict(extra="forbid")

    bm25_available: bool
    chroma_available: bool
    bm25_indexed: int
    chroma_indexed: int
    warnings: list[str] = Field(default_factory=list)


class HybridIndexManager:
    def __init__(
        self,
        root: str | Path,
        *,
        embedding_provider: EmbeddingProvider | None = None,
    ) -> None:
        self.root = Path(root)
        self.embedding_provider = embedding_provider
        self.memory_store = AgentScopedMemoryStore(self.root)
        self.knowledge_store = KnowledgeStore(self.root)
        self.bm25_index = BM25Index(self.root)
        self.chroma_index = ChromaVectorIndex(self.root, embedding_provider=embedding_provider)

    def rebuild(self) -> HybridIndexStatus:
        warnings: list[str] = []
        records = self.memory_store.list_records(status="active")
        chunks = self.knowledge_store.list_chunks()

        bm25_available = BM25Index.is_available()
        if not bm25_available:
            warnings.append("rank_bm25 is not installed")
        self.bm25_index.rebuild(memory_records=records, knowledge_chunks=chunks)
        bm25_indexed = len(self.bm25_index.documents())

        chroma_available = ChromaVectorIndex.is_available()
        chroma_indexed = 0
        if chroma_available:
            try:
                self.chroma_index.upsert_chunks(chunks)
                chroma_indexed = len([chunk for chunk in chunks if chunk.sensitivity != "secret" and chunk.acl.sensitivity != "secret"])
            except Exception as exc:
                warnings.append(f"chroma rebuild failed: {exc}")
        else:
            warnings.append("chromadb is not installed")

        return HybridIndexStatus(
            bm25_available=bm25_available,
            chroma_available=chroma_available,
            bm25_indexed=bm25_indexed,
            chroma_indexed=chroma_indexed,
            warnings=warnings,
        )

    def status(self) -> HybridIndexStatus:
        warnings: list[str] = []
        bm25_available = BM25Index.is_available()
        chroma_available = ChromaVectorIndex.is_available()
        if not bm25_available:
            warnings.append("rank_bm25 is not installed")
        if not chroma_available:
            warnings.append("chromadb is not installed")
        return HybridIndexStatus(
            bm25_available=bm25_available,
            chroma_available=chroma_available,
            bm25_indexed=_bm25_manifest_count(self.bm25_index.manifest_path),
            chroma_indexed=0,
            warnings=warnings,
        )


def _bm25_manifest_count(path: Path) -> int:
    if not path.exists():
        return 0
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return 0
    value = data.get("document_count")
    return value if isinstance(value, int) and value >= 0 else 0
