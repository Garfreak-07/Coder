from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from coder_workbench.memory.hybrid_index import HybridIndexManager
from coder_workbench.memory.knowledge_import import import_text_knowledge_source
from coder_workbench.memory.store import KnowledgeStore


class HybridIndexManagerTests(unittest.TestCase):
    def test_rebuild_succeeds_without_optional_dependencies(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            import_text_knowledge_source(KnowledgeStore(tmp), _request())

            with (
                patch("coder_workbench.memory.hybrid_index.BM25Index.is_available", return_value=False),
                patch("coder_workbench.memory.hybrid_index.ChromaVectorIndex.is_available", return_value=False),
            ):
                status = HybridIndexManager(tmp).rebuild()

            self.assertFalse(status.bm25_available)
            self.assertFalse(status.chroma_available)
            self.assertIn("rank_bm25 is not installed", status.warnings)
            self.assertIn("chromadb is not installed", status.warnings)

    def test_rebuild_writes_bm25_documents(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            import_text_knowledge_source(KnowledgeStore(tmp), _request(text="# Runtime\n\nFunction PlannerTaskState controls readiness."))

            status = HybridIndexManager(tmp).rebuild()

            self.assertGreaterEqual(status.bm25_indexed, 1)
            self.assertTrue((Path(tmp) / "indexes" / "bm25" / "documents.jsonl").exists())

    def test_status_reads_bm25_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            import_text_knowledge_source(KnowledgeStore(tmp), _request())
            manager = HybridIndexManager(tmp)
            manager.rebuild()

            status = manager.status()

            self.assertGreaterEqual(status.bm25_indexed, 1)


def _request(**overrides):
    values = {
        "title": "OpenHands SDK Notes",
        "text": "# Runtime\n\nUse scoped context.",
        "owner_scope": "project",
        "tags": ["openhands", "sdk"],
        "allowed_agents": ["planning_chat", "workflow_supervisor", "task_execution"],
        "purpose": ["coding_knowledge"],
    }
    values.update(overrides)
    return values


if __name__ == "__main__":
    unittest.main()
