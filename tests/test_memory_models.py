from __future__ import annotations

import unittest
from datetime import datetime, timezone

from pydantic import ValidationError

from coder_workbench.memory.models import (
    KnowledgeChunk,
    MemoryAcl,
    MemoryRecord,
    validate_knowledge_chunk,
    validate_memory_record,
)


class MemoryModelTests(unittest.TestCase):
    def test_valid_coding_knowledge_chunk_for_task_execution(self) -> None:
        chunk = KnowledgeChunk(
            chunk_id="chunk-1",
            source_id="source-1",
            title="Filesystem notes",
            text="Use atomic writes for local files.",
            summary="Atomic writes are preferred.",
            purpose=["coding_knowledge"],
            acl=MemoryAcl(
                allowed_agents=["task_execution"],
                allowed_contexts=["execution_prompt"],
            ),
            content_hash="sha256:chunk",
        )

        self.assertEqual(validate_knowledge_chunk(chunk).chunk_id, "chunk-1")

    def test_valid_project_planning_memory_for_planning_chat(self) -> None:
        record = _record(
            scope="project",
            purpose=["planning_context"],
            acl=MemoryAcl(
                allowed_agents=["planning_chat"],
                allowed_contexts=["assistant_message", "planner_task_state"],
            ),
        )

        self.assertEqual(validate_memory_record(record).scope, "project")

    def test_persona_style_rejected_for_task_execution(self) -> None:
        record = _record(
            scope="agent_style",
            source_type="agent_style",
            purpose=["persona_style"],
            acl=MemoryAcl(
                allowed_agents=["task_execution"],
                allowed_contexts=["execution_prompt"],
            ),
        )

        with self.assertRaisesRegex(ValueError, "persona_style"):
            validate_memory_record(record)

    def test_user_memory_rejected_for_task_execution(self) -> None:
        record = _record(
            scope="user",
            source_type="user_memory",
            purpose=["coding_knowledge"],
            acl=MemoryAcl(
                allowed_agents=["task_execution"],
                allowed_contexts=["execution_prompt"],
            ),
        )

        with self.assertRaisesRegex(ValueError, "user scope"):
            validate_memory_record(record)

    def test_secret_sensitivity_not_retrievable(self) -> None:
        record = _record(
            acl=MemoryAcl(
                allowed_agents=["planning_chat"],
                allowed_contexts=["assistant_message"],
                sensitivity="secret",
            ),
        )

        with self.assertRaisesRegex(ValueError, "secret memory"):
            validate_memory_record(record)

    def test_unknown_extra_field_rejected(self) -> None:
        payload = _record().model_dump(mode="json")
        payload["unknown"] = True

        with self.assertRaises(ValidationError):
            MemoryRecord.model_validate(payload)

    def test_obvious_secret_content_rejected(self) -> None:
        record = _record(content="LLM_API_KEY=secret")

        with self.assertRaisesRegex(ValueError, "secret-like marker"):
            validate_memory_record(record)


def _record(**overrides):
    values = {
        "id": "memory-1",
        "scope": "project",
        "source_type": "project_memory",
        "purpose": ["planning_context"],
        "title": "Project plan",
        "summary": "Keep planner decisions scoped.",
        "content": None,
        "project_id": "project",
        "acl": MemoryAcl(
            allowed_agents=["planning_chat"],
            allowed_contexts=["assistant_message"],
        ),
        "trust_level": "user_confirmed",
        "created_at": datetime.now(timezone.utc).isoformat(),
        "updated_at": datetime.now(timezone.utc).isoformat(),
    }
    values.update(overrides)
    return MemoryRecord(**values)


if __name__ == "__main__":
    unittest.main()
