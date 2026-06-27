from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from coder_workbench.core.artifacts import supported_artifact_types, validate_artifact
from coder_workbench.memory.planner_file_memory import (
    PlannerFileMemoryCommitter,
    PlannerMemoryWriteProposal,
    validate_planner_memory_write_proposal,
)


class PlannerFileMemoryTests(unittest.TestCase):
    def test_valid_project_proposal_created(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            committer = PlannerFileMemoryCommitter(tmp)
            record = committer.propose(
                _proposal(target_scope="project", target_file="roadmap.md"),
                role="planning_chat",
                project_id="project",
            )

            self.assertEqual(record["status"], "proposed")
            self.assertTrue(record["proposal"]["requires_user_confirmation"])
            self.assertTrue((Path(tmp) / "memory" / "planner" / "proposals" / f"{record['proposal_id']}.json").exists())

    def test_user_proposal_requires_confirmation(self) -> None:
        proposal = _proposal(target_scope="user", target_file="preferences.md", requires_user_confirmation=False)

        with self.assertRaisesRegex(ValueError, "user scope"):
            validate_planner_memory_write_proposal(proposal, role="planning_chat")

    def test_secret_content_rejected(self) -> None:
        proposal = _proposal(content="api_key=secret")

        with self.assertRaisesRegex(ValueError, "unsafe marker"):
            validate_planner_memory_write_proposal(proposal, role="planning_chat")

    def test_task_execution_cannot_produce_proposal(self) -> None:
        with self.assertRaisesRegex(ValueError, "only planning_chat"):
            validate_planner_memory_write_proposal(_proposal(), role="task_execution")

    def test_workflow_supervisor_cannot_produce_proposal(self) -> None:
        with self.assertRaisesRegex(ValueError, "only planning_chat"):
            validate_planner_memory_write_proposal(_proposal(), role="workflow_supervisor")

    def test_commit_approved_writes_markdown_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            committer = PlannerFileMemoryCommitter(tmp)
            record = committer.propose(_proposal(title="Decision", content="Use scoped memory."), project_id="project")

            result = committer.commit(record["proposal_id"], approved=True)

            self.assertEqual(result["status"], "committed")
            text = Path(result["target_path"]).read_text(encoding="utf-8")
            self.assertIn("## Decision", text)
            self.assertIn("Use scoped memory.", text)

    def test_commit_rejected_stores_rejection_without_writing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            committer = PlannerFileMemoryCommitter(tmp)
            record = committer.propose(_proposal(), project_id="project")

            result = committer.commit(record["proposal_id"], approved=False)

            self.assertEqual(result["status"], "rejected")
            stored = json.loads((Path(tmp) / "memory" / "planner" / "proposals" / f"{record['proposal_id']}.json").read_text(encoding="utf-8"))
            self.assertEqual(stored["status"], "rejected")
            self.assertFalse((Path(tmp) / "memory" / "planner" / "projects" / "project" / "roadmap.md").exists())

    def test_replace_section_does_not_wipe_unrelated_sections(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "memory" / "planner" / "projects" / "project" / "roadmap.md"
            target.parent.mkdir(parents=True)
            target.write_text("## Keep\n\nDo not remove.\n\n## Replace Me\n\nOld text.\n\n## Also Keep\n\nStill here.\n", encoding="utf-8")
            committer = PlannerFileMemoryCommitter(tmp)
            record = committer.propose(
                _proposal(operation="replace_section", title="Replace Me", content="New text."),
                project_id="project",
            )

            committer.commit(record["proposal_id"], approved=True)

            text = target.read_text(encoding="utf-8")
            self.assertIn("## Keep\n\nDo not remove.", text)
            self.assertIn("## Replace Me\n\nNew text.", text)
            self.assertIn("## Also Keep\n\nStill here.", text)
            self.assertNotIn("Old text.", text)

    def test_planner_memory_write_proposal_artifact_validates(self) -> None:
        artifact = validate_artifact(_proposal().model_dump(mode="json"), expected_type="planner_memory_write_proposal")

        self.assertEqual(artifact["artifact_type"], "planner_memory_write_proposal")
        self.assertIn("planner_memory_write_proposal", supported_artifact_types())


def _proposal(**overrides) -> PlannerMemoryWriteProposal:
    values = {
        "target_scope": "project",
        "target_file": "roadmap.md",
        "operation": "append",
        "title": "Memory update",
        "content": "Remember the scoped memory boundary.",
        "reason": "Batch D requires planner-controlled file memory.",
        "evidence_refs": ["planner-turn-1"],
        "confidence": "medium",
        "requires_user_confirmation": True,
    }
    values.update(overrides)
    return PlannerMemoryWriteProposal(**values)


if __name__ == "__main__":
    unittest.main()
