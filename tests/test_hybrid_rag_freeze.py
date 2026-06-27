from __future__ import annotations

import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

from pydantic import ValidationError

from coder_workbench.context import build_harness_context_packet
from coder_workbench.memory.bm25_index import BM25Index
from coder_workbench.memory.hybrid_retriever import HybridRagRetriever, weighted_rrf
from coder_workbench.memory.models import KnowledgeChunk, MemoryAcl, MemoryRecord
from coder_workbench.memory.rag_models import HybridRagRequest, RetrievalHit
from coder_workbench.memory.store import AgentScopedMemoryStore, KnowledgeStore
from coder_workbench.openhands_tools.hybrid_rag_search import CoderHybridRagSearchAction


class HybridRagFreezeTests(unittest.TestCase):
    def test_tool_action_schema_has_no_role_or_context_fields(self) -> None:
        schema = CoderHybridRagSearchAction.model_json_schema()["properties"]

        self.assertNotIn("role", schema)
        self.assertNotIn("requested_context", schema)
        self.assertNotIn("memory_root", schema)
        with self.assertRaises(ValidationError):
            CoderHybridRagSearchAction.model_validate({"query": "q", "role": "planning_chat"})

    def test_task_execution_cannot_retrieve_persona_or_user_memory(self) -> None:
        with _stores() as stores:
            memory_store, knowledge_store, root = stores
            persona = memory_store.append_record(
                _record(
                    "style-1",
                    scope="agent_style",
                    source_type="agent_style",
                    purpose=["persona_style"],
                    summary="Use concise wording.",
                    acl=MemoryAcl(allowed_agents=["planning_chat"], allowed_contexts=["assistant_message"]),
                )
            )
            user = memory_store.append_record(
                _record(
                    "user-1",
                    scope="user",
                    source_type="user_memory",
                    purpose=["coding_knowledge"],
                    summary="User coding preference.",
                    acl=MemoryAcl(allowed_agents=["planning_chat"], allowed_contexts=["assistant_message"]),
                )
            )
            index = BM25Index(root)
            index.rebuild(memory_records=[persona, user], knowledge_chunks=[])

            results = HybridRagRetriever(
                memory_store=memory_store,
                knowledge_store=knowledge_store,
                bm25_index=index,
            ).retrieve(
                HybridRagRequest(
                    role="task_execution",
                    requested_context="execution_prompt",
                    query="concise coding preference",
                    project_id="project",
                )
            )

            self.assertEqual(results, [])

    def test_planning_chat_can_retrieve_allowed_project_knowledge(self) -> None:
        with _stores() as stores:
            memory_store, knowledge_store, root = stores
            chunk = knowledge_store.append_chunk(
                _chunk(
                    "chunk-1",
                    text="Function PlannerTaskState controls readiness.",
                    acl=MemoryAcl(allowed_agents=["planning_chat"], allowed_contexts=["assistant_message"]),
                )
            )
            index = BM25Index(root)
            index.rebuild(memory_records=[], knowledge_chunks=[chunk])

            results = HybridRagRetriever(
                memory_store=memory_store,
                knowledge_store=knowledge_store,
                bm25_index=index,
            ).retrieve(
                HybridRagRequest(
                    role="planning_chat",
                    requested_context="assistant_message",
                    query="PlannerTaskState readiness",
                    project_id="project",
                )
            )

            self.assertEqual([result.id for result in results], ["chunk-1"])

    def test_rrf_fusion_dedupes_by_id(self) -> None:
        fused = weighted_rrf(
            [RetrievalHit(id="same", item_type="knowledge_chunk", rank=1, score=1.0, channel="dense")],
            [RetrievalHit(id="same", item_type="knowledge_chunk", rank=1, score=10.0, channel="bm25")],
            dense_weight=0.5,
            bm25_weight=0.5,
        )

        self.assertEqual(fused, [("same", fused[0][1])])

    def test_context_packet_does_not_inline_raw_content_preview(self) -> None:
        packet = build_harness_context_packet(
            mode="task_execution",
            user_goal="Use retrieved knowledge.",
            workflow_id="workflow-1",
            agent_id="executor",
            knowledge_hits=[
                {
                    "id": "chunk-1",
                    "title": "Chunk",
                    "summary": "Compact summary.",
                    "text_preview": "FULL RAW DOCUMENT BODY SHOULD NOT INLINE",
                    "card_type": "knowledge_chunk",
                    "token_estimate": 20,
                }
            ],
        )

        self.assertNotIn("FULL RAW DOCUMENT BODY", str(packet))
        self.assertIn("Compact summary.", str(packet))


class _stores:
    def __enter__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / ".coder"
        self.memory_store = AgentScopedMemoryStore(self.root)
        self.knowledge_store = KnowledgeStore(self.root)
        return self.memory_store, self.knowledge_store, self.root

    def __exit__(self, *_args):
        self.tmp.cleanup()


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
        "acl": MemoryAcl(allowed_agents=["planning_chat"], allowed_contexts=["assistant_message"]),
        "trust_level": "user_confirmed",
        "created_at": _now(),
        "updated_at": _now(),
        "token_estimate": 20,
    }
    values.update(overrides)
    return MemoryRecord(**values)


def _chunk(chunk_id: str, *, text: str, acl: MemoryAcl | None = None) -> KnowledgeChunk:
    return KnowledgeChunk(
        chunk_id=chunk_id,
        source_id="source-1",
        title=f"Chunk {chunk_id}",
        text=text,
        summary=text[:120],
        tags=["rag"],
        purpose=["coding_knowledge"],
        acl=acl
        or MemoryAcl(
            allowed_agents=["planning_chat", "workflow_supervisor", "task_execution"],
            allowed_contexts=["assistant_message", "workflow_supervision", "execution_prompt"],
        ),
        content_hash=f"sha256:{chunk_id}",
        token_estimate=20,
    )


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


if __name__ == "__main__":
    unittest.main()
