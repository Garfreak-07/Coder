from __future__ import annotations

import json
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

from coder_workbench.memory.bm25_index import BM25Index, tokenize_bm25
from coder_workbench.memory.models import KnowledgeChunk, MemoryAcl, MemoryRecord


class BM25IndexTests(unittest.TestCase):
    def test_tokenizer_handles_code_identifiers_and_paths(self) -> None:
        self.assertEqual(tokenize_bm25("PlannerTaskState"), ["plannertaskstate", "planner", "task", "state"])
        self.assertEqual(
            tokenize_bm25("src/coder_workbench/memory/run_memory.py"),
            ["src", "coder_workbench", "coder", "workbench", "memory", "run_memory", "run", "memory", "py"],
        )
        self.assertEqual(
            tokenize_bm25("insufficient_structured_planner_output"),
            ["insufficient_structured_planner_output", "insufficient", "structured", "planner", "output"],
        )

    def test_rebuild_writes_documents_and_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = BM25Index(Path(tmp) / ".coder")

            index.rebuild(memory_records=[], knowledge_chunks=[_chunk("chunk-1", text="PlannerTaskState readiness")])

            self.assertTrue(index.documents_path.exists())
            self.assertTrue(index.manifest_path.exists())
            manifest = json.loads(index.manifest_path.read_text(encoding="utf-8"))
            self.assertEqual(manifest["document_count"], 1)

    def test_exact_function_like_query_retrieves_hit(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = BM25Index(tmp)
            index.rebuild(
                memory_records=[],
                knowledge_chunks=[
                    _chunk("chunk-1", text="Function PlannerTaskState controls readiness."),
                    _chunk("chunk-2", text="Unrelated knowledge about task routing."),
                ],
            )

            hits = index.search("PlannerTaskState readiness", top_k=1)

            self.assertEqual(hits[0].id, "chunk-1")
            self.assertEqual(hits[0].item_type, "knowledge_chunk")

    def test_persona_style_memory_can_be_indexed_for_later_acl_filtering(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = BM25Index(tmp)
            index.rebuild(
                memory_records=[
                    _record(
                        "style-1",
                        scope="agent_style",
                        source_type="agent_style",
                        purpose=["persona_style"],
                        title="Style memory",
                        summary="Use concise wording.",
                        acl=MemoryAcl(allowed_agents=["planning_chat"], allowed_contexts=["assistant_message"]),
                    )
                ],
                knowledge_chunks=[],
            )

            self.assertEqual([document.id for document in index.documents()], ["style-1"])

    def test_secret_memory_is_not_indexed(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = BM25Index(tmp)
            index.rebuild(
                memory_records=[
                    _record(
                        "secret-1",
                        acl=MemoryAcl(allowed_agents=[], allowed_contexts=[], sensitivity="secret"),
                    )
                ],
                knowledge_chunks=[],
            )

            self.assertEqual(index.documents(), [])

    def test_module_imports_without_optional_dependency(self) -> None:
        self.assertIsInstance(BM25Index.is_available(), bool)


def _chunk(chunk_id: str, *, text: str) -> KnowledgeChunk:
    return KnowledgeChunk(
        chunk_id=chunk_id,
        source_id="source-1",
        title=f"Chunk {chunk_id}",
        text=text,
        summary=text,
        tags=["memory"],
        purpose=["coding_knowledge"],
        acl=MemoryAcl(
            allowed_agents=["planning_chat", "workflow_supervisor", "task_execution"],
            allowed_contexts=["assistant_message", "workflow_supervision", "execution_prompt"],
        ),
        content_hash=f"sha256:{chunk_id}",
        token_estimate=20,
    )


def _record(record_id: str, **overrides) -> MemoryRecord:
    values = {
        "id": record_id,
        "scope": "project",
        "source_type": "project_memory",
        "purpose": ["planning_context"],
        "title": "Project memory",
        "summary": "Planner memory.",
        "content": "Small safe content.",
        "project_id": "project",
        "acl": MemoryAcl(
            allowed_agents=["planning_chat"],
            allowed_contexts=["assistant_message"],
        ),
        "trust_level": "user_confirmed",
        "created_at": _now(),
        "updated_at": _now(),
        "token_estimate": 20,
    }
    values.update(overrides)
    return MemoryRecord(**values)


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


if __name__ == "__main__":
    unittest.main()
