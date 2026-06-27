from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from coder_workbench.memory.chroma_index import ChromaVectorIndex
from coder_workbench.memory.embeddings import HashingEmbeddingProvider
from coder_workbench.memory.models import KnowledgeChunk, MemoryAcl


class ChromaVectorIndexTests(unittest.TestCase):
    def test_module_imports_without_chromadb(self) -> None:
        self.assertIsInstance(ChromaVectorIndex.is_available(), bool)

    def test_is_available_false_when_chromadb_missing(self) -> None:
        with patch("coder_workbench.memory.chroma_index.chromadb", None):
            self.assertFalse(ChromaVectorIndex.is_available())

    def test_search_returns_empty_when_chromadb_missing(self) -> None:
        with patch("coder_workbench.memory.chroma_index.chromadb", None):
            index = ChromaVectorIndex("unused")

            self.assertEqual(index.search("query", top_k=3), [])

    @unittest.skipUnless(ChromaVectorIndex.is_available(), "chromadb is not installed")
    def test_upsert_and_search_chunks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = ChromaVectorIndex(
                Path(tmp) / ".coder",
                embedding_provider=HashingEmbeddingProvider(dimensions=64),
            )

            index.upsert_chunks([_chunk("chunk-1", text="OpenHands custom tool registration")])
            hits = index.search("OpenHands tool", top_k=1)

            self.assertEqual(hits[0].id, "chunk-1")

    @unittest.skipUnless(ChromaVectorIndex.is_available(), "chromadb is not installed")
    def test_metadata_contains_acl_fields(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = ChromaVectorIndex(Path(tmp) / ".coder")
            chunk = _chunk("chunk-1", text="metadata test")

            index.upsert_chunks([chunk])
            collection = index._get_collection()
            item = collection.get(ids=["chunk-1"], include=["metadatas"])
            metadata = item["metadatas"][0]

            self.assertEqual(metadata["chunk_id"], "chunk-1")
            self.assertEqual(metadata["content_hash"], "sha256:chunk-1")
            self.assertIn("task_execution", metadata["allowed_agents"])

    @unittest.skipUnless(ChromaVectorIndex.is_available(), "chromadb is not installed")
    def test_re_upsert_is_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = ChromaVectorIndex(Path(tmp) / ".coder")
            chunk = _chunk("chunk-1", text="same content")

            index.upsert_chunks([chunk])
            index.upsert_chunks([chunk])

            self.assertEqual(index.search("same content", top_k=5)[0].id, "chunk-1")


def _chunk(chunk_id: str, *, text: str) -> KnowledgeChunk:
    return KnowledgeChunk(
        chunk_id=chunk_id,
        source_id="source-1",
        title=f"Chunk {chunk_id}",
        text=text,
        summary=text,
        tags=["openhands"],
        purpose=["coding_knowledge"],
        acl=MemoryAcl(
            allowed_agents=["planning_chat", "workflow_supervisor", "task_execution"],
            allowed_contexts=["assistant_message", "workflow_supervision", "execution_prompt"],
        ),
        content_hash=f"sha256:{chunk_id}",
        token_estimate=20,
    )


if __name__ == "__main__":
    unittest.main()
