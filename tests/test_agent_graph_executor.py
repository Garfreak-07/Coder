from __future__ import annotations

import tempfile
import unittest
from typing import Any

from coder_workbench.agent_graph.executor import AgentGraphExecutor
from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, PlannerOrder, WorkItem
from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.server.settings import ProviderSettings


class FakeResponse:
    def __init__(self, content: str) -> None:
        self.content = content


class FakeChatModel:
    def __init__(self, responses: list[str]) -> None:
        self.responses = list(responses)
        self.prompts: list[str] = []

    def invoke(self, prompt: str) -> FakeResponse:
        self.prompts.append(prompt)
        if len(self.responses) > 1:
            return FakeResponse(self.responses.pop(0))
        return FakeResponse(self.responses[0])


class BadPlannerExecutor:
    def create_planner_order(self, request: str, *, emit=None) -> PlannerOrder:
        return PlannerOrder.model_validate(
            {
                "artifact_type": "planner_order",
                "round": 1,
                "round_goal": request,
                "plan_graph": {
                    "work_items": [
                        {
                            "work_item_id": "bad",
                            "merge_index": 1,
                            "assignee_agent_id": "missing-agent",
                            "task_summary": "Should not validate.",
                            "depends_on": [],
                            "tester_agent_ids": [],
                        }
                    ]
                },
            }
        )

    def create_execution_result(self, **kwargs):  # pragma: no cover - should not be reached
        raise AssertionError("execution should not run")

    def create_test_result(self, **kwargs):  # pragma: no cover - should not be reached
        raise AssertionError("test should not run")

    def create_planner_decision(self, **kwargs):  # pragma: no cover - should not be reached
        raise AssertionError("decision should not run")


class AgentGraphExecutorTests(unittest.TestCase):
    def test_valid_planner_order_json_becomes_plan_graph(self) -> None:
        model = FakeChatModel(
            [
                (
                    '{"artifact_type":"planner_order","round":1,"round_goal":"Plan it",'
                    '"plan_graph":{"work_items":[{"work_item_id":"executor-work","merge_index":1,'
                    '"assignee_agent_id":"executor","task_summary":"Do it","depends_on":[],'
                    '"tester_agent_ids":["tester"]}]}}'
                )
            ]
        )
        executor = _executor(model)

        order = executor.create_planner_order("Plan it")

        self.assertEqual(order.plan_graph.work_items[0].work_item_id, "executor-work")
        self.assertEqual(order.plan_graph.work_items[0].tester_agent_ids, ["tester"])

    def test_valid_worker_json_becomes_execution_record(self) -> None:
        model = FakeChatModel(
            [
                (
                    '{"artifact_type":"execution_result","status":"completed",'
                    '"summary":"Implemented the task.",'
                    '"proposed_changes":[{"path":"src/app.py","action":"update","patch_ref":"patch_1"}]}'
                )
            ]
        )
        events: list[dict[str, Any]] = []
        executor = _executor(model)

        record = executor.create_execution_result(
            item=_item(),
            envelope=_envelope(),
            emit=lambda event_type, message, **payload: events.append({"type": event_type, **payload}),
        )

        self.assertEqual(record.status, "completed")
        self.assertEqual(record.work_item_id, "executor-work")
        self.assertEqual(record.merge_index, 1)
        self.assertEqual(record.agent_id, "executor")
        self.assertEqual(record.execution_summary, "Implemented the task.")
        self.assertEqual(record.artifact_payload["proposed_changes"][0]["path"], "src/app.py")
        self.assertEqual(
            [event["type"] for event in events],
            ["agent_graph.agent_call.started", "agent_graph.agent_call.completed"],
        )

    def test_invalid_worker_json_repairs_once(self) -> None:
        model = FakeChatModel(
            [
                "not json",
                '{"artifact_type":"execution_result","status":"completed","summary":"Repaired."}',
            ]
        )
        events: list[dict[str, Any]] = []
        executor = _executor(model)

        record = executor.create_execution_result(
            item=_item(),
            envelope=_envelope(),
            emit=lambda event_type, message, **payload: events.append({"type": event_type, **payload}),
        )

        self.assertEqual(record.status, "completed")
        self.assertEqual(record.execution_summary, "Repaired.")
        self.assertEqual(len(model.prompts), 2)
        self.assertIn("agent_graph.agent_call.schema_failed", [event["type"] for event in events])
        self.assertIn("agent_graph.agent_call.repair_completed", [event["type"] for event in events])

    def test_invalid_worker_json_failed_repair_returns_blocked_record(self) -> None:
        model = FakeChatModel(["not json", "still not json"])
        events: list[dict[str, Any]] = []
        executor = _executor(model)

        record = executor.create_execution_result(
            item=_item(),
            envelope=_envelope(),
            emit=lambda event_type, message, **payload: events.append({"type": event_type, **payload}),
        )

        self.assertEqual(record.status, "blocked")
        self.assertIn("did not match execution_result schema", record.execution_summary)
        self.assertIn("agent_graph.agent_call.repair_failed", [event["type"] for event in events])

    def test_valid_tester_json_becomes_test_record(self) -> None:
        model = FakeChatModel(['{"artifact_type":"test_result","status":"pass","summary":"Looks good."}'])
        executor = _executor(model)

        record = executor.create_test_result(
            item=_item(),
            execution_artifact={
                "artifact_type": "execution_result",
                "round": 1,
                "artifact_id": "execution_result_executor-work",
                "status": "completed",
                "summary": "Done.",
            },
            tester_agent_id="tester",
        )

        self.assertEqual(record.status, "pass")
        self.assertEqual(record.tester_agent_id, "tester")
        self.assertEqual(record.test_summary, "Looks good.")

    def test_valid_planner_decision_json_is_validated(self) -> None:
        model = FakeChatModel(
            [
                (
                    '{"artifact_type":"planner_decision","round":1,"task_done":true,'
                    '"next_action":"finish","reason":"All work is done."}'
                )
            ]
        )
        executor = _executor(model)

        decision = executor.create_planner_decision(bundle=_planner_bundle())

        self.assertEqual(decision["next_action"], "finish")
        self.assertEqual(decision["reason"], "All work is done.")

    def test_valid_final_tester_json_becomes_final_test_record(self) -> None:
        model = FakeChatModel(['{"artifact_type":"test_result","status":"pass","summary":"Aggregate passed."}'])
        executor = _executor(model)

        record = executor.create_final_test_result(
            bundle=_planner_bundle(),
            final_tester_agent_id="tester",
        )

        self.assertEqual(record.status, "pass")
        self.assertEqual(record.final_tester_agent_id, "tester")
        self.assertEqual(record.summary, "Aggregate passed.")
        self.assertEqual(record.final_test_result_ref, "test_result_final_tester")

    def test_planner_order_graph_validation_failure_stops_runner(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=BadPlannerExecutor(),
            ).run("Use an invalid assignee.", tmp)

        self.assertEqual(result.status, "failed")
        self.assertEqual(result.status_code, "planner_order_validation_failed")
        self.assertIn("PlannerOrder graph validation failed", result.status_reason)


def _executor(model: FakeChatModel) -> AgentGraphExecutor:
    settings = ProviderSettings(
        default_provider="openai",
        default_model="fake-model",
        api_keys={"openai": "test-key"},
        mock_mode=False,
    )
    return AgentGraphExecutor(
        default_planner_led_agent_workflow(),
        runtime_settings=settings,
        model_factory=lambda config: model,
    )


def _item() -> WorkItem:
    return WorkItem(
        work_item_id="executor-work",
        merge_index=1,
        assignee_agent_id="executor",
        task_summary="Do the work.",
        depends_on=[],
        tester_agent_ids=["tester"],
    )


def _envelope() -> AgentTaskEnvelope:
    return AgentTaskEnvelope(
        round=1,
        work_item_id="executor-work",
        merge_index=1,
        assigned_agent_id="executor",
        task_summary="Do the work.",
        planner_order_ref="planner_order_round_1",
    )


def _planner_bundle():
    from coder_workbench.agent_graph.schema import PlannerInputBundle

    return PlannerInputBundle(
        round=1,
        planner_order_ref="planner_order_round_1",
        plan_status="completed",
        items=[],
    )


if __name__ == "__main__":
    unittest.main()
