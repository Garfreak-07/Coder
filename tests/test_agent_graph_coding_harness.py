from __future__ import annotations

import tempfile
import unittest
from typing import Any

from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, ExecutionRecord, PlannerInputBundle, PlannerOrder, WorkItem
from coder_workbench.core import default_planner_led_agent_workflow


class AgentGraphCodingHarnessTests(unittest.TestCase):
    def test_failed_execution_verification_creates_debug_finding_effect(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=FailedTestExecutor(),
            ).run("Fix failing test.", tmp)

        self.assertEqual(result.status, "completed")
        self.assertIn("debug_findings", result.data)
        self.assertTrue(result.data["debug_findings"][0]["failure_summary"])
        effects = result.data["planner_input_bundle"]["effects"]
        self.assertTrue(any(effect.get("effect_type") == "debug_finding" for effect in effects))
        self.assertIn("coding_eval", result.data)


class FailedTestExecutor:
    def create_planner_order(self, request: str, *, emit=None, **kwargs: Any) -> PlannerOrder:
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
                            "task_summary": "Create failing evidence.",
                            "depends_on": [],
                        }
                    ]
                },
            }
        )

    def create_execution_result(self, *, item: WorkItem, envelope: AgentTaskEnvelope, emit=None) -> ExecutionRecord:
        artifact = {
            "artifact_type": "execution_result",
            "round": envelope.round,
            "work_item_id": item.work_item_id,
            "merge_index": item.merge_index,
            "agent_id": item.assignee_agent_id,
            "status": "blocked",
            "summary": "AssertionError: expected valid user.",
            "unexpected_issues": ["verification_failed"],
            "remaining_work": ["Fix failing assertion."],
            "needs_planner_decision": True,
            "blocker_type": "verification_failed",
            "continue_without_human_possible": True,
            "verification": {
                "status": "fail",
                "checks_run": [
                    {
                        "check_id": "unit",
                        "kind": "command",
                        "command": "python -m unittest discover -s tests",
                        "status": "fail",
                        "summary": "AssertionError: expected valid user.",
                        "output_ref": "check_output_round_1",
                        "evidence_refs": ["check_output_round_1"],
                    }
                ],
                "evidence_refs": ["check_output_round_1"],
                "confidence": "high",
                "remaining_work": ["Fix failing assertion."],
                "no_check_rationale": None,
                "repair_attempted": True,
                "repair_summary": "Verification failed after repair.",
            },
        }
        return ExecutionRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status="blocked",
            execution_summary=artifact["summary"],
            execution_result_ref="execution_result_executor-work",
            artifact_payload=artifact,
        )

    def create_planner_decision(
        self,
        *,
        bundle: PlannerInputBundle,
        emit=None,
    ) -> dict[str, Any]:
        return {
            "artifact_type": "planner_decision",
            "round": bundle.round,
            "task_done": True,
            "next_action": "finish",
            "reason": "Debug finding was recorded.",
        }


if __name__ == "__main__":
    unittest.main()
