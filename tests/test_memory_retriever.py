from __future__ import annotations

import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

from coder_workbench.memory.models import KnowledgeChunk, MemoryAcl, MemoryRecord
from coder_workbench.memory.retriever import MemoryRetrievalRequest, MemoryRetriever
from coder_workbench.memory.store import AgentScopedMemoryStore, KnowledgeStore


class MemoryRetrieverTests(unittest.TestCase):
    def test_planning_chat_can_retrieve_project_memory(self) -> None:
        with _stores() as stores:
            memory_store, knowledge_store = stores
            memory_store.append_record(
                _record(
                    "project-plan",
                    scope="project",
                    purpose=["planning_context"],
                    title="Roadmap decision",
                    summary="Use the existing Planner-led workflow.",
                    acl=MemoryAcl(
                        allowed_agents=["planning_chat"],
                        allowed_contexts=["assistant_message"],
                    ),
                )
            )

            cards = MemoryRetriever(memory_store=memory_store, knowledge_store=knowledge_store).retrieve(
                _request(role="planning_chat", requested_context="assistant_message", query="roadmap planner")
            )

            self.assertEqual([card.id for card in cards], ["project-plan"])

    def test_planning_chat_can_retrieve_agent_style_only_for_assistant_message(self) -> None:
        with _stores() as stores:
            memory_store, knowledge_store = stores
            memory_store.append_record(
                _record(
                    "style-1",
                    scope="agent_style",
                    source_type="agent_style",
                    purpose=["persona_style"],
                    title="Style",
                    summary="Use concise wording.",
                    acl=MemoryAcl(
                        allowed_agents=["planning_chat"],
                        allowed_contexts=["assistant_message"],
                    ),
                )
            )
            retriever = MemoryRetriever(memory_store=memory_store, knowledge_store=knowledge_store)

            assistant_cards = retriever.retrieve(
                _request(role="planning_chat", requested_context="assistant_message", query="concise")
            )
            planner_state_cards = retriever.retrieve(
                _request(role="planning_chat", requested_context="planner_task_state", query="concise")
            )

            self.assertEqual([card.id for card in assistant_cards], ["style-1"])
            self.assertEqual(planner_state_cards, [])

    def test_workflow_supervisor_cannot_retrieve_persona_style(self) -> None:
        with _stores() as stores:
            memory_store, knowledge_store = stores
            memory_store.append_record(
                _record(
                    "style-1",
                    scope="agent_style",
                    source_type="agent_style",
                    purpose=["persona_style"],
                    acl=MemoryAcl(
                        allowed_agents=["planning_chat"],
                        allowed_contexts=["assistant_message"],
                    ),
                )
            )

            cards = MemoryRetriever(memory_store=memory_store, knowledge_store=knowledge_store).retrieve(
                _request(role="workflow_supervisor", requested_context="workflow_supervision", query="style")
            )

            self.assertEqual(cards, [])

    def test_task_execution_cannot_retrieve_user_memory(self) -> None:
        with _stores() as stores:
            memory_store, knowledge_store = stores
            memory_store.append_record(
                _record(
                    "user-1",
                    scope="user",
                    source_type="user_memory",
                    purpose=["coding_knowledge"],
                    acl=MemoryAcl(
                        allowed_agents=["planning_chat"],
                        allowed_contexts=["assistant_message"],
                    ),
                )
            )

            cards = MemoryRetriever(memory_store=memory_store, knowledge_store=knowledge_store).retrieve(
                _request(role="task_execution", requested_context="execution_prompt", query="coding")
            )

            self.assertEqual(cards, [])

    def test_task_execution_can_retrieve_coding_knowledge(self) -> None:
        with _stores() as stores:
            memory_store, knowledge_store = stores
            knowledge_store.append_chunk(
                _chunk(
                    "chunk-1",
                    text="Use apply_patch for small code edits.",
                    tags=["editing"],
                    acl=MemoryAcl(
                        allowed_agents=["task_execution"],
                        allowed_contexts=["execution_prompt"],
                    ),
                )
            )

            cards = MemoryRetriever(memory_store=memory_store, knowledge_store=knowledge_store).retrieve(
                _request(role="task_execution", requested_context="execution_prompt", query="apply_patch", tags=["editing"])
            )

            self.assertEqual(cards[0].id, "chunk-1")
            self.assertEqual(cards[0].scope, "knowledge_source")
            self.assertEqual(cards[0].card_type, "knowledge_chunk")

    def test_token_budget_limits_results(self) -> None:
        with _stores() as stores:
            memory_store, knowledge_store = stores
            knowledge_store.append_chunk(_chunk("large", text="large context", token_estimate=1500))
            knowledge_store.append_chunk(_chunk("small", text="small context", token_estimate=500))

            cards = MemoryRetriever(memory_store=memory_store, knowledge_store=knowledge_store).retrieve(
                _request(
                    role="task_execution",
                    requested_context="execution_prompt",
                    query="context",
                    token_budget=1000,
                )
            )

            self.assertEqual([card.id for card in cards], ["small"])

    def test_higher_tag_match_ranks_first(self) -> None:
        with _stores() as stores:
            memory_store, knowledge_store = stores
            knowledge_store.append_chunk(_chunk("generic", text="Use context carefully.", tags=["general"]))
            knowledge_store.append_chunk(_chunk("tagged", text="Use context carefully.", tags=["openhands", "runtime"]))

            cards = MemoryRetriever(memory_store=memory_store, knowledge_store=knowledge_store).retrieve(
                _request(
                    role="task_execution",
                    requested_context="execution_prompt",
                    query="context",
                    tags=["runtime"],
                )
            )

            self.assertEqual(cards[0].id, "tagged")

    def test_secret_memory_is_never_returned(self) -> None:
        with _stores() as stores:
            memory_store, knowledge_store = stores
            memory_store.append_record(
                _record(
                    "secret-1",
                    acl=MemoryAcl(
                        allowed_agents=[],
                        allowed_contexts=[],
                        sensitivity="secret",
                    ),
                )
            )

            cards = MemoryRetriever(memory_store=memory_store, knowledge_store=knowledge_store).retrieve(
                _request(role="planning_chat", requested_context="assistant_message", query="memory")
            )

            self.assertEqual(cards, [])


class _stores:
    def __enter__(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name) / ".coder"
        self.memory_store = AgentScopedMemoryStore(root)
        self.knowledge_store = KnowledgeStore(root)
        return self.memory_store, self.knowledge_store

    def __exit__(self, *_args):
        self.tmp.cleanup()


def _request(**overrides) -> MemoryRetrievalRequest:
    values = {
        "role": "planning_chat",
        "query": "",
        "project_id": "project",
        "requested_context": "assistant_message",
    }
    values.update(overrides)
    return MemoryRetrievalRequest(**values)


def _record(record_id: str, **overrides) -> MemoryRecord:
    values = {
        "id": record_id,
        "scope": "project",
        "source_type": "project_memory",
        "purpose": ["planning_context"],
        "title": "Project memory",
        "summary": "Planner memory.",
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


def _chunk(
    chunk_id: str,
    *,
    text: str,
    tags: list[str] | None = None,
    acl: MemoryAcl | None = None,
    token_estimate: int = 20,
) -> KnowledgeChunk:
    return KnowledgeChunk(
        chunk_id=chunk_id,
        source_id="source-1",
        title=f"Chunk {chunk_id}",
        text=text,
        summary=text,
        tags=tags or [],
        purpose=["coding_knowledge"],
        acl=acl
        or MemoryAcl(
            allowed_agents=["task_execution"],
            allowed_contexts=["execution_prompt"],
        ),
        content_hash=f"sha256:{chunk_id}",
        token_estimate=token_estimate,
    )


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


if __name__ == "__main__":
    unittest.main()
