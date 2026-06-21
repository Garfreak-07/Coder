from __future__ import annotations

import unittest

from coder_workbench.agent_graph.schema import AgentTaskEnvelope, WorkItem
from coder_workbench.agent_harness import CodeWorkerHarness, HarnessAction, HarnessPermissionPolicy, code_worker_policy, planner_policy


class FakeResponse:
    def __init__(self, content: str) -> None:
        self.content = content


class FakeModel:
    def __init__(self, responses: list[str]) -> None:
        self.responses = list(responses)

    def invoke(self, prompt: str) -> FakeResponse:
        if len(self.responses) > 1:
            return FakeResponse(self.responses.pop(0))
        return FakeResponse(self.responses[0])


class AgentHarnessTests(unittest.TestCase):
    def test_policy_boundaries_allow_only_planner_to_ask_human(self) -> None:
        permissions = HarnessPermissionPolicy()

        self.assertTrue(permissions.allow(HarnessAction(type="ask_human"), planner_policy()))
        self.assertFalse(permissions.allow(HarnessAction(type="ask_human"), code_worker_policy()))

    def test_code_worker_mock_outputs_valid_execution_result(self) -> None:
        record = CodeWorkerHarness().create_execution_result(
            item=_item(),
            envelope=_envelope(),
            coding_context_packet={"included_files": ["src/app.py"]},
        )

        self.assertEqual(record.status, "completed")
        self.assertEqual(record.artifact_payload["artifact_type"], "execution_result")

    def test_code_worker_repairs_invalid_json_once(self) -> None:
        worker = CodeWorkerHarness(
            model=FakeModel(
                [
                    "not json",
                    '{"artifact_type":"execution_result","status":"completed","summary":"Repaired."}',
                ]
            )
        )

        record = worker.create_execution_result(item=_item(), envelope=_envelope())

        self.assertEqual(record.status, "completed")
        self.assertEqual(record.execution_summary, "Repaired.")

    def test_code_worker_blocks_when_context_is_missing(self) -> None:
        record = CodeWorkerHarness().create_execution_result(
            item=_item(),
            envelope=_envelope(),
            coding_context_packet={"included_files": []},
        )

        self.assertEqual(record.status, "blocked")
        self.assertTrue(record.artifact_payload["needs_planner_decision"])
        self.assertEqual(record.artifact_payload["blocker_type"], "context_missing")


def _item() -> WorkItem:
    return WorkItem(
        work_item_id="executor-work",
        merge_index=1,
        assignee_agent_id="executor",
        task_summary="Fix src/app.py.",
        tester_agent_ids=["tester"],
    )


def _envelope() -> AgentTaskEnvelope:
    return AgentTaskEnvelope(
        round=1,
        work_item_id="executor-work",
        merge_index=1,
        assigned_agent_id="executor",
        task_summary="Fix src/app.py.",
        planner_order_ref="planner_order_round_1",
    )


if __name__ == "__main__":
    unittest.main()
