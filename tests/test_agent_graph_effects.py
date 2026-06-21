from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, ExecutionRecord, PlannerInputBundle, PlannerOrder, TestRecord, WorkItem
from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.server.storage import RunStore


class AgentGraphEffectsTests(unittest.TestCase):
    def test_unapproved_optional_check_command_requires_planner_confirmation(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            marker = repo / "created.txt"
            command = f'"{sys.executable}" -c "from pathlib import Path; Path(\'created.txt\').write_text(\'bad\')"'

            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=EffectSourceExecutor(check_commands=[command]),
            ).run(
                "Run hidden effect.",
                str(repo),
            )
            marker_exists = marker.exists()

        self.assertEqual(result.status, "completed")
        self.assertFalse(marker_exists)
        effect = result.data["planner_input_bundle"]["effects"][0]
        self.assertEqual(effect["effect_type"], "optional_check_command")
        self.assertEqual(effect["status"], "check_requires_planner_confirmation")
        self.assertIn("approval_key", effect)

    def test_preapproved_optional_check_command_records_output_ref(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            command = f'"{sys.executable}" -c "print(42)"'

            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=EffectSourceExecutor(check_commands=[command]),
            ).run(
                "Run hidden effect.",
                str(repo),
                initial_data={"preapprove_all": True},
            )

        effect = result.data["planner_input_bundle"]["effects"][0]
        self.assertEqual(effect["status"], "completed")
        self.assertEqual(effect["output_ref"], "check_output_1")
        output = result.data["graph_run_cache"]["hidden_effect_outputs"]["check_output_1"]
        self.assertIn("42", output["output"])
        tool_event = next(event for event in result.events if event.type == "tool.result")
        self.assertEqual(tool_event.payload["tool_result_id"], "check_output_1")

        with tempfile.TemporaryDirectory() as tmp:
            store = RunStore(Path(tmp) / ".coder")
            stored = store.save("agent-graph", tmp, "Run hidden effect.", result)
            loaded = store.get_tool_result(stored.id, "check_output_1")

        self.assertIn("42", loaded["output"])

    def test_modify_files_effect_creates_patch_preview_without_applying(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            src = repo / "src"
            src.mkdir()
            target = src / "example.txt"
            target.write_text("before\n", encoding="utf-8")

            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=EffectSourceExecutor(
                    proposed_changes=[
                        {
                            "path": "src/example.txt",
                            "action": "update",
                            "expected_before": "before\n",
                            "content": "after\n",
                        }
                    ],
                ),
            ).run(
                "Preview file changes.",
                str(repo),
                initial_data={"scopes": ["src"]},
            )
            current_content = target.read_text(encoding="utf-8")

        self.assertEqual(current_content, "before\n")
        effect = result.data["planner_input_bundle"]["effects"][0]
        self.assertEqual(effect["effect_type"], "modify_files")
        self.assertEqual(effect["status"], "patch_preview_created")
        patch_ref = effect["patch_ref"]
        self.assertTrue(patch_ref.startswith("patch_preview_"))
        preview = result.data["graph_run_cache"]["hidden_effect_outputs"][patch_ref]
        self.assertEqual(preview["status"], "proposed")
        self.assertTrue(preview["requires_approval"])
        self.assertIn("-before", preview["files"][0]["diff"])
        self.assertIn("+after", preview["files"][0]["diff"])

    def test_modify_files_effect_blocks_risk_paths(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            (repo / ".env").write_text("TOKEN=old\n", encoding="utf-8")

            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=EffectSourceExecutor(
                    proposed_changes=[
                        {
                            "path": ".env",
                            "action": "update",
                            "expected_before": "TOKEN=old\n",
                            "content": "TOKEN=new\n",
                        }
                    ],
                ),
            ).run("Preview risky file changes.", str(repo))

        effect = result.data["planner_input_bundle"]["effects"][0]
        self.assertEqual(effect["status"], "patch_preview_blocked")
        self.assertEqual(result.data["planner_input_bundle"]["plan_status"], "interrupted")
        self.assertEqual(result.data["planner_input_bundle"]["interrupts"][0]["blocker_type"], "risk_boundary")


class EffectSourceExecutor:
    def __init__(
        self,
        *,
        proposed_changes: list[dict[str, Any]] | None = None,
        check_commands: list[str] | None = None,
    ) -> None:
        self.proposed_changes = proposed_changes or []
        self.check_commands = check_commands or []

    def create_planner_order(self, request: str, *, emit=None) -> PlannerOrder:
        return PlannerOrder.model_validate(
            {
                "artifact_type": "planner_order",
                "round": 1,
                "round_goal": request,
                "plan_graph": {
                    "work_items": [
                        {
                            "work_item_id": "executor-work",
                            "merge_index": 1,
                            "assignee_agent_id": "executor",
                            "task_summary": "Run effect source work.",
                            "depends_on": [],
                            "tester_agent_ids": ["tester"],
                        }
                    ]
                },
            }
        )

    def create_execution_result(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        emit=None,
    ) -> ExecutionRecord:
        artifact = {
            "artifact_type": "execution_result",
            "round": envelope.round,
            "work_item_id": item.work_item_id,
            "merge_index": item.merge_index,
            "agent_id": item.assignee_agent_id,
            "status": "completed",
            "summary": "Execution produced validated effect inputs.",
            "proposed_changes": self.proposed_changes,
        }
        return ExecutionRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status="completed",
            execution_summary=artifact["summary"],
            execution_result_ref="execution_result_executor-work",
            artifact_payload=artifact,
        )

    def create_test_result(
        self,
        *,
        item: WorkItem,
        execution_artifact: dict[str, Any],
        tester_agent_id: str,
        emit=None,
    ) -> TestRecord:
        artifact = {
            "artifact_type": "test_result",
            "round": int(execution_artifact.get("round") or 1),
            "work_item_id": item.work_item_id,
            "merge_index": item.merge_index,
            "tester_agent_id": tester_agent_id,
            "status": "pass",
            "summary": "Tester produced validated check commands.",
            "check_commands": self.check_commands,
        }
        return TestRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            tester_agent_id=tester_agent_id,
            status="pass",
            test_summary=artifact["summary"],
            test_result_ref="test_result_executor-work_tester",
            artifact_payload=artifact,
        )

    def create_planner_decision(
        self,
        *,
        bundle: PlannerInputBundle,
        planner_human_response: dict[str, Any] | None = None,
        emit=None,
    ) -> dict[str, Any]:
        return {
            "artifact_type": "planner_decision",
            "round": bundle.round,
            "task_done": True,
            "next_action": "finish",
            "reason": "Effect source test completed.",
        }


if __name__ == "__main__":
    unittest.main()
