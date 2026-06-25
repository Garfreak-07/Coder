from __future__ import annotations

import tempfile
import unittest
from typing import Any

from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, ExecutionRecord, PlannerInputBundle, PlannerOrder, WorkItem
from coder_workbench.core import default_planner_led_agent_workflow


class DebugFindingReplanTests(unittest.TestCase):
    def test_planner_strategy_continues_after_failed_execution_verification(self) -> None:
        executor = FailedVerificationExecutor()

        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow(), executor=executor).run(
                "Fix failing check.",
                tmp,
                initial_data={"planner_mode": "simple"},
        )

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.status_code, "planner_blocked")
        self.assertGreaterEqual(executor.execution_calls, 2)
        self.assertTrue(result.data["blocked_recovery_used"])
        self.assertEqual(result.data["planner_decision"]["final_status"], "blocked")
        self.assertIn("debug_findings", result.data)


class FailedVerificationExecutor:
    def __init__(self) -> None:
        self.execution_calls = 0

    def create_planner_order(self, request: str, *, round_number: int = 1, emit=None, **kwargs: Any) -> PlannerOrder:
        return PlannerOrder.model_validate(
            {
                "artifact_type": "planner_order",
                "round": round_number,
                "round_goal": request,
                "plan_graph": {
                    "work_items": [
                        {
                            "work_item_id": f"executor-work-{round_number}",
                            "merge_index": 1,
                            "assignee_agent_id": "executor",
                            "task_summary": "Run failing check.",
                            "depends_on": [],
                        }
                    ]
                },
            }
        )

    def create_execution_result(self, *, item: WorkItem, envelope: AgentTaskEnvelope, emit=None) -> ExecutionRecord:
        self.execution_calls += 1
        artifact = {
            "artifact_type": "execution_result",
            "round": envelope.round,
            "work_item_id": item.work_item_id,
            "merge_index": item.merge_index,
            "agent_id": item.assignee_agent_id,
            "status": "blocked",
            "summary": "Command failed.",
            "unexpected_issues": ["verification_failed"],
            "remaining_work": ["Fix command failure."],
            "needs_planner_decision": True,
            "blocker_type": "verification_failed",
            "continue_without_human_possible": True,
            "verification": {
                "status": "fail",
                "checks_run": [
                    {
                        "check_id": "unit",
                        "kind": "command",
                        "command": "python -m unittest",
                        "status": "fail",
                        "summary": "Command failed.",
                        "output_ref": f"check_output_{envelope.round}",
                        "evidence_refs": [f"check_output_{envelope.round}"],
                    }
                ],
                "evidence_refs": [f"check_output_{envelope.round}"],
                "confidence": "high",
                "remaining_work": ["Fix command failure."],
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
            execution_result_ref=f"execution_result_{item.work_item_id}",
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
            "task_done": False,
            "next_action": "continue",
            "reason": "Replan after verification failure.",
            "next_round_goal": "Fix failed execution verification and rerun checks.",
            "remaining_auto_rounds": 1,
        }


if __name__ == "__main__":
    unittest.main()
