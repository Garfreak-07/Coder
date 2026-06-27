from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from fastapi.testclient import TestClient

from coder_workbench.memory.knowledge_import import KnowledgeTextImportRequest, import_text_knowledge_source
from coder_workbench.memory.retriever import MemoryRetrievalRequest, MemoryRetriever
from coder_workbench.memory.store import KnowledgeStore
from coder_workbench.server.app import create_app


class KnowledgeImportTests(unittest.TestCase):
    def test_import_text_source_creates_source_and_chunks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = import_text_knowledge_source(KnowledgeStore(tmp), _request())

            self.assertEqual(result.source.kind, "manual_note")
            self.assertEqual(len(result.chunks), 2)
            self.assertEqual(result.chunks[0].source_id, result.source.source_id)
            self.assertEqual(result.chunks[0].embedding_id, None)

    def test_content_hash_is_stable(self) -> None:
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            one = import_text_knowledge_source(KnowledgeStore(first), _request())
            two = import_text_knowledge_source(KnowledgeStore(second), _request())

            self.assertEqual(one.source.content_hash, two.source.content_hash)
            self.assertEqual([chunk.content_hash for chunk in one.chunks], [chunk.content_hash for chunk in two.chunks])

    def test_allowed_agents_are_respected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = import_text_knowledge_source(
                KnowledgeStore(tmp),
                _request(allowed_agents=["planning_chat"], purpose=["coding_knowledge"]),
            )

            self.assertEqual(result.chunks[0].acl.allowed_agents, ["planning_chat"])
            self.assertNotIn("task_execution", result.chunks[0].acl.allowed_agents)

    def test_task_execution_can_retrieve_allowed_coding_knowledge_chunk(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store = KnowledgeStore(tmp)
            import_text_knowledge_source(store, _request(text="# Editing\n\nUse apply_patch for code edits."))

            cards = MemoryRetriever(knowledge_store=store).retrieve(
                MemoryRetrievalRequest(
                    role="task_execution",
                    requested_context="execution_prompt",
                    query="apply_patch edits",
                )
            )

            self.assertEqual(cards[0].card_type, "knowledge_chunk")
            self.assertIn("apply_patch", cards[0].summary)

    def test_persona_style_chunk_cannot_be_retrieved_by_task_execution(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store = KnowledgeStore(tmp)
            import_text_knowledge_source(
                store,
                _request(
                    text="# Style\n\nUse concise assistant wording.",
                    allowed_agents=["planning_chat"],
                    purpose=["persona_style"],
                ),
            )

            cards = MemoryRetriever(knowledge_store=store).retrieve(
                MemoryRetrievalRequest(
                    role="task_execution",
                    requested_context="execution_prompt",
                    query="concise assistant wording",
                )
            )

            self.assertEqual(cards, [])

    def test_api_import_and_list_knowledge_sources(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            response = client.post(
                "/api/v2/knowledge-sources/import-text",
                json=_request().model_dump(mode="json"),
            )

            self.assertEqual(response.status_code, 200)
            self.assertTrue(response.json()["index_dirty"])
            source_id = response.json()["source"]["source_id"]
            sources = client.get("/api/v2/knowledge-sources").json()["sources"]
            chunks = client.get(f"/api/v2/knowledge-sources/{source_id}/chunks").json()["chunks"]

            self.assertEqual(sources[0]["source_id"], source_id)
            self.assertEqual(len(chunks), 2)

    def test_api_reindex_and_status_endpoints(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            client.post(
                "/api/v2/knowledge-sources/import-text",
                json=_request(text="# Runtime\n\nFunction PlannerTaskState controls readiness.").model_dump(mode="json"),
            )

            reindex = client.post("/api/v2/rag/reindex")
            status = client.get("/api/v2/rag/status")

            self.assertEqual(reindex.status_code, 200)
            self.assertEqual(reindex.json()["status"], "completed")
            self.assertGreaterEqual(reindex.json()["bm25_indexed"], 1)
            self.assertEqual(status.status_code, 200)
            self.assertGreaterEqual(status.json()["bm25_indexed"], 1)

    def test_chroma_unavailable_does_not_break_import(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))

            response = client.post(
                "/api/v2/knowledge-sources/import-text",
                json=_request().model_dump(mode="json"),
            )

            self.assertEqual(response.status_code, 200)
            self.assertTrue(response.json()["index_dirty"])

    def test_import_rejects_empty_text(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaisesRegex(ValueError, "text is required"):
                import_text_knowledge_source(KnowledgeStore(tmp), _request(text=" "))


def _request(**overrides) -> KnowledgeTextImportRequest:
    values = {
        "title": "OpenHands SDK Notes",
        "text": "# Runtime\n\nUse scoped context.\n\n# Editing\n\nUse apply_patch for edits.",
        "owner_scope": "project",
        "tags": ["openhands", "sdk"],
        "allowed_agents": ["planning_chat", "workflow_supervisor", "task_execution"],
        "purpose": ["coding_knowledge"],
    }
    values.update(overrides)
    return KnowledgeTextImportRequest(**values)


if __name__ == "__main__":
    unittest.main()
