from __future__ import annotations

import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

from coder_workbench.memory.models import KnowledgeChunk, KnowledgeSource, MemoryAcl, MemoryRecord
from coder_workbench.memory.store import AgentScopedMemoryStore, KnowledgeStore, WorkflowMemoryStore


class AgentScopedMemoryStoreTests(unittest.TestCase):
    def test_append_get_list_memory_record(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store = AgentScopedMemoryStore(Path(tmp) / ".coder")
            record = _record("memory-1", title="Project rules")

            stored = store.append_record(record)

            self.assertEqual(stored.id, "memory-1")
            self.assertEqual(store.get_record("memory-1").title, "Project rules")
            self.assertEqual([item.id for item in store.list_records(scope="project")], ["memory-1"])
            self.assertTrue((Path(tmp) / ".coder" / "memory" / "records.jsonl").exists())

    def test_append_list_knowledge_source_and_chunk(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store = KnowledgeStore(Path(tmp) / ".coder")
            source = KnowledgeSource(
                source_id="source-1",
                kind="manual_note",
                uri="manual:source-1",
                title="SDK notes",
                content_hash="sha256:source",
                imported_at=_now(),
            )
            chunk = _chunk("chunk-1", source_id="source-1")

            store.append_source(source)
            store.append_chunk(chunk)

            self.assertEqual(store.list_sources()[0].source_id, "source-1")
            self.assertEqual(store.list_chunks(source_id="source-1")[0].chunk_id, "chunk-1")

    def test_supersede_old_record(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store = AgentScopedMemoryStore(tmp)
            store.append_record(_record("memory-old"))
            replacement = _record("memory-new", summary="Updated.")

            store.supersede_record("memory-old", replacement)

            self.assertEqual(store.get_record("memory-old").status, "superseded")
            active = store.list_records()
            self.assertEqual([record.id for record in active], ["memory-new"])
            self.assertEqual(active[0].supersedes, ["memory-old"])

    def test_soft_forget_hides_record_from_active_list(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store = AgentScopedMemoryStore(tmp)
            store.append_record(_record("memory-1"))

            store.forget_record("memory-1")

            self.assertEqual(store.list_records(), [])
            with self.assertRaises(KeyError):
                store.get_record("memory-1")

    def test_hard_forget_removes_record_and_writes_tombstone(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = AgentScopedMemoryStore(root)
            store.append_record(_record("memory-1"))

            store.forget_record("memory-1", hard=True)

            self.assertEqual(store.list_records(), [])
            self.assertIn("memory-1", (root / "memory" / "tombstones.jsonl").read_text(encoding="utf-8"))
            self.assertNotIn("memory-1", (root / "memory" / "records.jsonl").read_text(encoding="utf-8"))

    def test_store_root_is_created_automatically(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / ".coder"

            AgentScopedMemoryStore(root).append_record(_record("memory-1"))

            self.assertTrue((root / "memory").is_dir())

    def test_records_jsonl_does_not_allow_invalid_record(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "memory"
            root.mkdir()
            (root / "records.jsonl").write_text('{"id": "bad"}\n', encoding="utf-8")

            with self.assertRaises(ValueError):
                AgentScopedMemoryStore(root).list_records()

    def test_legacy_workflow_memory_store_still_uses_old_layout(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store = WorkflowMemoryStore(tmp)

            memory = store.append_entry(
                workflow_id="workflow",
                collection="planner_notes",
                entry={"note": "legacy behavior"},
            )

            self.assertEqual(memory.planner_notes[0]["note"], "legacy behavior")


def _record(record_id: str, **overrides) -> MemoryRecord:
    values = {
        "id": record_id,
        "scope": "project",
        "source_type": "project_memory",
        "purpose": ["planning_context"],
        "title": "Project memory",
        "summary": "Planner-safe memory.",
        "project_id": "project",
        "acl": MemoryAcl(
            allowed_agents=["planning_chat"],
            allowed_contexts=["assistant_message"],
        ),
        "trust_level": "user_confirmed",
        "created_at": _now(),
        "updated_at": _now(),
        "token_estimate": 10,
    }
    values.update(overrides)
    return MemoryRecord(**values)


def _chunk(chunk_id: str, *, source_id: str) -> KnowledgeChunk:
    return KnowledgeChunk(
        chunk_id=chunk_id,
        source_id=source_id,
        title="SDK chunk",
        text="Use scoped context for execution.",
        summary="Execution needs scoped context.",
        purpose=["coding_knowledge"],
        acl=MemoryAcl(
            allowed_agents=["task_execution"],
            allowed_contexts=["execution_prompt"],
        ),
        content_hash="sha256:chunk",
    )


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


if __name__ == "__main__":
    unittest.main()
