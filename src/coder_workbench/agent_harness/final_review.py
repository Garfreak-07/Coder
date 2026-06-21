from __future__ import annotations

from coder_workbench.agent_graph.schema import FinalTestRecord, PlannerInputBundle

from .base import AgentHarness
from .policies import final_review_policy


class FinalReviewHarness(AgentHarness):
    def __init__(self) -> None:
        super().__init__(policy=final_review_policy())

    def create_final_test_result(self, *, bundle: PlannerInputBundle, final_tester_agent_id: str) -> FinalTestRecord:
        failed = [
            item
            for item in bundle.items
            if item.execution_status in {"failed", "blocked"} or item.test_status in {"fail", "blocked"}
        ]
        status = "pass" if not failed else "fail"
        return FinalTestRecord(
            round=bundle.round,
            final_tester_agent_id=final_tester_agent_id,
            status=status,
            summary=f"FinalReviewHarness aggregate status is {status} for {len(bundle.items)} work item(s).",
            final_test_result_ref=f"test_result_final_{final_tester_agent_id}",
            artifact_payload={
                "artifact_type": "test_result",
                "round": bundle.round,
                "tester_agent_id": final_tester_agent_id,
                "status": status,
                "summary": f"FinalReviewHarness aggregate status is {status}.",
                "remaining_work": [item.task_summary for item in failed],
                "confidence": "medium",
            },
        )
