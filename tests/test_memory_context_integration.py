from __future__ import annotations

import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

from coder_workbench.agent_graph.agent_run import AgentRun
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, WorkItem
from coder_workbench.context import build_harness_context_packet
from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.memory.models import KnowledgeChunk, MemoryAcl, MemoryRecord
from coder_workbench.memory.retriever import MemoryRetrievalRequest, MemoryRetriever
from coder_workbench.memory.store import AgentScopedMemoryStore, KnowledgeStore


class MemoryContextIntegrationTests(unittest.TestCase):
    def test_planning_chat_packet_contains_allowed_memory_cards(self) -> None:
        packet = build_harness_context_packet(
            mode="planning_chat",
            user_goal="Plan memory.",
            workflow_id="workflow",
            agent_id="planner",
            memory_cards=[
                {
                    "id": "memory-1",
                    "title": "Planner decision",
                    "summary": "Use Planner-led execution.",
                    "scope": "project",
                    "purpose": ["planning_context"],
                    "token_estimate": 12,
                    "score": 2.0,
                    "card_type": "memory_record",
                }
            ],
            knowledge_hits=[
                {
                    "id": "chunk-1",
                    "title": "SDK note",
                    "summary": "Use scoped context.",
                    "scope": "knowledge_source",
                    "purpose": ["coding_knowledge"],
                    "token_estimate": 10,
                    "score": 1.0,
                    "card_type": "knowledge_chunk",
                }
            ],
            memory_token_budget={"limit": 4000, "used": 22},
        )

        self.assertEqual(packet["warm"]["memory_cards"][0]["id"], "memory-1")
        self.assertEqual(packet["warm"]["knowledge_hits"][0]["id"], "chunk-1")
        self.assertEqual(packet["warm"]["memory_token_budget"], {"limit": 4000, "used": 22})
        self.assertIn({"ref_type": "memory", "refs": ["memory-1"]}, packet["cold_refs"])
        self.assertIn({"ref_type": "knowledge", "refs": ["chunk-1"]}, packet["cold_refs"])
        self.assertNotIn("content", str(packet))

    def test_workflow_supervisor_packet_excludes_persona_and_user_private_memory(self) -> None:
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
            memory_store.append_record(
                _record(
                    "rules-1",
                    scope="project",
                    purpose=["project_rules"],
                    acl=MemoryAcl(
                        allowed_agents=["workflow_supervisor"],
                        allowed_contexts=["workflow_supervision"],
                    ),
                )
            )
            cards = MemoryRetriever(memory_store=memory_store, knowledge_store=knowledge_store).retrieve(
                MemoryRetrievalRequest(
                    role="workflow_supervisor",
                    requested_context="workflow_supervision",
                    query="rules style",
                    project_id="project",
                )
            )

            packet = build_harness_context_packet(
                mode="workflow_supervisor",
                user_goal="Run workflow.",
                workflow_id="workflow",
                agent_id="planner",
                memory_cards=cards,
            )

            self.assertEqual([card["id"] for card in packet["warm"]["memory_cards"]], ["rules-1"])
            self.assertNotIn("style-1", str(packet))

    def test_task_execution_packet_excludes_user_and_project_planning_memory(self) -> None:
        with _stores() as stores:
            memory_store, knowledge_store = stores
            memory_store.append_record(
                _record(
                    "project-plan",
                    scope="project",
                    purpose=["planning_context"],
                    acl=MemoryAcl(
                        allowed_agents=["planning_chat"],
                        allowed_contexts=["assistant_message"],
                    ),
                )
            )
            knowledge_store.append_chunk(_chunk("chunk-1", text="Use apply_patch for code edits."))

            cards = MemoryRetriever(memory_store=memory_store, knowledge_store=knowledge_store).retrieve(
                MemoryRetrievalRequest(
                    role="task_execution",
                    requested_context="execution_prompt",
                    query="apply_patch planning",
                    project_id="project",
                )
            )
            packet = build_harness_context_packet(
                mode="task_execution",
                user_goal="Edit code.",
                workflow_id="workflow",
                agent_id="executor",
                work_item={"work_item_id": "work", "task_summary": "Edit code."},
                task_envelope={"round": 1, "work_item_id": "work", "task_summary": "Edit code.", "planner_order_ref": "order"},
                memory_cards=cards,
            )

            self.assertNotIn("memory_cards", packet["warm"])
            self.assertEqual(packet["warm"]["knowledge_hits"][0]["id"], "chunk-1")
            self.assertNotIn("project-plan", str(packet))

    def test_token_budget_is_enforced_before_packet_injection(self) -> None:
        with _stores() as stores:
            memory_store, knowledge_store = stores
            knowledge_store.append_chunk(_chunk("large", text="large context", token_estimate=1500))
            knowledge_store.append_chunk(_chunk("small", text="small context", token_estimate=500))

            cards = MemoryRetriever(memory_store=memory_store, knowledge_store=knowledge_store).retrieve(
                MemoryRetrievalRequest(
                    role="task_execution",
                    requested_context="execution_prompt",
                    query="context",
                    token_budget=1000,
                )
            )
            packet = build_harness_context_packet(
                mode="task_execution",
                user_goal="Use context.",
                workflow_id="workflow",
                agent_id="executor",
                task_envelope={"round": 1, "work_item_id": "work", "task_summary": "Use context.", "planner_order_ref": "order"},
                memory_cards=cards,
                memory_token_budget={"limit": 1000, "used": sum(card.token_estimate for card in cards)},
            )

            self.assertEqual([card["id"] for card in packet["warm"]["knowledge_hits"]], ["small"])
            self.assertEqual(packet["warm"]["memory_token_budget"], {"limit": 1000, "used": 500})

    def test_packet_omits_empty_memory_fields(self) -> None:
        packet = build_harness_context_packet(
            mode="planning_chat",
            user_goal="No memory.",
            workflow_id="workflow",
            agent_id="planner",
            memory_cards=[],
            knowledge_hits=[],
            memory_token_budget=None,
        )

        self.assertNotIn("memory_cards", packet["warm"])
        self.assertNotIn("knowledge_hits", packet["warm"])
        self.assertNotIn("memory_token_budget", packet["warm"])

    def test_agent_run_task_execution_context_loads_scoped_knowledge(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / ".coder"
            KnowledgeStore(root).append_chunk(_chunk("chunk-1", text="Use apply_patch for small edits."))
            workflow = default_planner_led_agent_workflow()
            item = WorkItem(
                work_item_id="executor-work",
                merge_index=1,
                assignee_agent_id="executor",
                task_summary="Use apply_patch.",
                depends_on=[],
            )
            envelope = AgentTaskEnvelope(
                round=1,
                work_item_id=item.work_item_id,
                merge_index=item.merge_index,
                assigned_agent_id=item.assignee_agent_id,
                task_summary=item.task_summary,
                planner_order_ref="planner-order-ref",
            )

            context = AgentRun(
                workflow,
                initial_data={"repo_root": tmp, "skill_store_root": str(root), "request": "Use apply_patch."},
            )._harness_context(
                agent_id="executor",
                harness_id="task-execution-harness",
                mode="task_execution",
                profile_id="internal-fallback-task-executor",
                round_number=1,
                state_view={},
                capability_set={},
                work_item=item,
                task_envelope=envelope,
            )

            self.assertEqual(context.context_packet["warm"]["knowledge_hits"][0]["id"], "chunk-1")


class _stores:
    def __enter__(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name) / ".coder"
        self.memory_store = AgentScopedMemoryStore(root)
        self.knowledge_store = KnowledgeStore(root)
        return self.memory_store, self.knowledge_store

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


def _chunk(chunk_id: str, *, text: str, token_estimate: int = 20) -> KnowledgeChunk:
    return KnowledgeChunk(
        chunk_id=chunk_id,
        source_id="source-1",
        title=f"Chunk {chunk_id}",
        text=text,
        summary=text,
        purpose=["coding_knowledge"],
        acl=MemoryAcl(
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
