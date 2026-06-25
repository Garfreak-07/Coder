from __future__ import annotations

import tempfile
import unittest
from typing import Any

from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, ExecutionRecord, PlannerInputBundle, PlannerOrder, WorkItem
from coder_workbench.core import default_planner_led_agent_workflow


class ExecutionResultConsistencyTests(unittest.TestCase):
    def test_blocked_execution_result_is_normalized_across_run_surfaces(self) -> None:
        executor = MinimalBlockedExecutor()

        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=executor,
            ).run("Exercise blocked result consistency.", tmp)

        work_item_id = "blocked-work"
        artifact_ref = "execution_result_blocked-work"
        artifact = result.artifacts[artifact_ref]
        cache_payload = result.data["graph_run_cache"]["execution_cache"][work_item_id]["artifact_payload"]
        state = result.data["shared_run_state"]
        state_item = state["work_items"][work_item_id]
        bundle_item = result.data["planner_input_bundle"]["items"][0]
        summary_item = result.data["round_summary"]["ordered_state"][0]
        final_report = result.data["final_report"]

        self.assertEqual(result.status, "blocked")
        self.assertEqual(final_report["status"], "blocked")
        for key in [
            "artifact_id",
            "work_item_id",
            "status",
            "blocker_type",
            "blocker_reason",
            "planner_recommendation",
        ]:
            with self.subTest(key=key):
                self.assertEqual(artifact.get(key), cache_payload.get(key))
        self.assertEqual(artifact["verification"]["status"], cache_payload["verification"]["status"])
        self.assertEqual(artifact["artifact_id"], artifact_ref)
        self.assertEqual(artifact["blocker_type"], "unknown_error")
        self.assertEqual(artifact["blocker_reason"], "Blocked before verification fields existed.")
        self.assertEqual(artifact["planner_recommendation"], "finish")

        self.assertEqual(state_item["execution_result_ref"], artifact_ref)
        self.assertEqual(state_item["status"], artifact["status"])
        self.assertEqual(state_item["blocked_reason"], artifact["blocker_reason"])
        self.assertIn(artifact_ref, bundle_item["refs"])
        self.assertEqual(bundle_item["verification_status"], artifact["verification"]["status"])
        self.assertIn(artifact_ref, summary_item["refs"])
        self.assertEqual(summary_item["status"], "blocked")
        self.assertIn(artifact_ref, final_report["evidence_refs"])


class MinimalBlockedExecutor:
    def create_planner_order(
        self,
        request: str,
        *,
        previous_bundle: PlannerInputBundle | None = None,
        previous_round_summary: dict[str, Any] | None = None,
        round_number: int = 1,
        emit=None,
    ) -> PlannerOrder:
        return PlannerOrder.model_validate(
            {
                "artifact_type": "planner_order",
                "round": round_number,
                "round_goal": request,
                "plan_graph": {
                    "work_items": [
                        {
                            "work_item_id": "blocked-work",
                            "merge_index": 1,
                            "assignee_agent_id": "executor",
                            "task_summary": "Produce a minimal blocked result.",
                            "depends_on": [],
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
        artifact_ref = f"execution_result_{item.work_item_id}"
        artifact = {
            "artifact_type": "execution_result",
            "round": envelope.round,
            "work_item_id": item.work_item_id,
            "merge_index": item.merge_index,
            "agent_id": item.assignee_agent_id,
            "status": "blocked",
            "summary": "Blocked before verification fields existed.",
            "blocker_type": "technical_blocker",
            "evidence_refs": [artifact_ref],
        }
        return ExecutionRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status="blocked",
            execution_summary=artifact["summary"],
            execution_result_ref=artifact_ref,
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
            "next_action": "finish",
            "final_status": "blocked",
            "reason": "Blocked result should surface consistently.",
        }


if __name__ == "__main__":
    unittest.main()
