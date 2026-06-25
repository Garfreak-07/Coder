from __future__ import annotations

import tempfile
import unittest
from typing import Any

from coder_workbench.agent_graph.executor import AgentGraphExecutor
from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_graph.schema import (
    AgentTaskEnvelope,
    ExecutionRecord,
    PlannerInputBundle,
    PlannerOrder,
    WorkItem,
)
from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.server.settings import ProviderSettings


class AgentGraphInterventionTests(unittest.TestCase):
    def test_worker_blocker_interrupt_stops_next_wave(self) -> None:
        executor = InterventionExecutor(
            work_items=[
                _work_item("A", 1),
                _work_item("B", 2, depends_on=["A"]),
            ],
            execution_status_by_id={"A": "blocked"},
        )

        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=executor,
            ).run("Run interrupted graph.", tmp)

        bundle = result.data["planner_input_bundle"]
        item_status = {item["work_item_id"]: item["execution_status"] for item in bundle["items"]}

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.status_code, "planner_blocked")
        self.assertEqual(result.data["planner_decision"]["next_action"], "finish")
        self.assertEqual(result.data["planner_decision"]["final_status"], "blocked")
        self.assertEqual(executor.execution_calls, ["A"])
        self.assertEqual(bundle["plan_status"], "interrupted")
        self.assertEqual(bundle["interrupts"][0]["work_item_id"], "A")
        self.assertEqual(item_status["B"], "not_started")
        self.assertIn("agent_graph.interrupt.requested", {event.type for event in result.events})
        self.assertIn("agent_graph.interrupt.captured", {event.type for event in result.events})

    def test_interrupt_waits_for_current_wave_to_finish(self) -> None:
        executor = InterventionExecutor(
            work_items=[
                _work_item("A", 1),
                _work_item("B", 2),
                _work_item("C", 3, depends_on=["A"]),
            ],
            execution_status_by_id={"A": "blocked", "B": "completed"},
        )

        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=executor,
            ).run("Run interrupted graph.", tmp)

        bundle = result.data["planner_input_bundle"]
        item_status = {item["work_item_id"]: item["execution_status"] for item in bundle["items"]}

        self.assertEqual(set(executor.execution_calls), {"A", "B"})
        self.assertNotIn("C", executor.execution_calls)
        self.assertEqual(bundle["plan_status"], "interrupted")
        self.assertEqual(item_status["A"], "blocked")
        self.assertEqual(item_status["B"], "completed")
        self.assertEqual(item_status["C"], "not_started")

    def test_mock_planner_decision_continues_when_interrupt_is_auto_resolvable(self) -> None:
        executor = AgentGraphExecutor(
            default_planner_led_agent_workflow(),
            runtime_settings=ProviderSettings(),
            model_factory=lambda config: None,
        )

        decision = executor.create_planner_decision(
            bundle=_bundle_with_interrupt(continue_without_human_possible=True),
        )

        self.assertEqual(decision["next_action"], "continue")
        self.assertFalse(decision["requires_human_confirmation"])

    def test_mock_planner_decision_finishes_blocked_when_interrupt_needs_user(self) -> None:
        executor = AgentGraphExecutor(
            default_planner_led_agent_workflow(),
            runtime_settings=ProviderSettings(),
            model_factory=lambda config: None,
        )

        decision = executor.create_planner_decision(
            bundle=_bundle_with_interrupt(continue_without_human_possible=False),
        )

        self.assertEqual(decision["next_action"], "finish")
        self.assertEqual(decision["final_status"], "blocked")
        self.assertFalse(decision["requires_human_confirmation"])

    def test_planner_decision_runtime_input_receives_interrupts(self) -> None:
        model = FakeChatModel(
            [
                (
                    '{"artifact_type":"planner_decision","round":1,"task_done":false,'
                    '"next_action":"continue","reason":"Use safer local option.",'
                    '"next_round_goal":"Resolve blocked work."}'
                )
            ]
        )
        executor = AgentGraphExecutor(
            default_planner_led_agent_workflow(),
            runtime_settings=ProviderSettings(api_keys={"openai": "test-key"}, mock_mode=False),
            model_factory=lambda config: model,
        )
        calls = _capture_workflow_supervisor_calls(executor)
        bundle = _bundle_with_interrupt(continue_without_human_possible=True)

        decision = executor.create_planner_decision(bundle=bundle)

        self.assertEqual(decision["next_action"], "continue")
        self.assertEqual(model.prompts, [])
        self.assertEqual(calls[0]["profile_id"], "openhands-workflow-supervisor-default")
        self.assertIs(calls[0]["input_artifacts"]["legacy_kwargs"]["bundle"], bundle)
        self.assertEqual(calls[0]["input_artifacts"]["legacy_kwargs"]["bundle"].interrupts[0].work_item_id, "A")

    def test_planner_continue_runs_second_round(self) -> None:
        executor = MultiRoundExecutor()

        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=executor,
            ).run("Run a replan.", tmp)

        round_events = [event for event in result.events if event.type == "agent_graph.round.started"]

        self.assertEqual(result.status, "completed")
        self.assertEqual([event.payload["round"] for event in round_events], [1, 2])
        self.assertEqual([call["round"] for call in executor.planner_order_calls], [1, 2])
        self.assertIsNotNone(executor.planner_order_calls[1]["previous_bundle"])
        self.assertEqual(result.data["planner_input_bundle"]["round"], 2)
        self.assertEqual([entry["round"] for entry in result.data["rounds"]], [1, 2])

    def test_max_auto_rounds_stops_loop(self) -> None:
        executor = AlwaysContinueExecutor()

        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=executor,
            ).run("Keep continuing.", tmp, initial_data={"max_auto_rounds": 2})

        round_events = [event for event in result.events if event.type == "agent_graph.round.started"]

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.status_code, "max_auto_rounds_reached")
        self.assertEqual([event.payload["round"] for event in round_events], [1, 2])

    def test_legacy_planner_response_checkpoint_is_not_created(self) -> None:
        executor = ResumeThroughPlannerExecutor()

        with tempfile.TemporaryDirectory() as tmp:
            blocked = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=executor,
            ).run("Pause for user.", tmp)

        self.assertEqual(blocked.status, "blocked")
        self.assertEqual(blocked.status_code, "planner_blocked")
        self.assertIsNone(blocked.resume_checkpoint)
        self.assertEqual(executor.execution_calls.count("blocked-work"), 1)
        self.assertNotIn("fix-work", executor.execution_calls)
        self.assertEqual(blocked.data["planner_decision"]["next_action"], "finish")
        self.assertEqual(blocked.data["planner_decision"]["final_status"], "blocked")


class InterventionExecutor:
    def __init__(
        self,
        *,
        work_items: list[dict[str, Any]],
        execution_status_by_id: dict[str, str],
    ) -> None:
        self.work_items = work_items
        self.execution_status_by_id = execution_status_by_id
        self.execution_calls: list[str] = []

    def create_planner_order(self, request: str, *, emit=None) -> PlannerOrder:
        return PlannerOrder.model_validate(
            {
                "artifact_type": "planner_order",
                "round": 1,
                "round_goal": request,
                "plan_graph": {"work_items": self.work_items},
            }
        )

    def create_execution_result(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        emit=None,
    ) -> ExecutionRecord:
        self.execution_calls.append(item.work_item_id)
        status = self.execution_status_by_id.get(item.work_item_id, "completed")
        artifact = {
            "artifact_type": "execution_result",
            "round": envelope.round,
            "work_item_id": item.work_item_id,
            "merge_index": item.merge_index,
            "agent_id": item.assignee_agent_id,
            "status": status,
            "summary": f"{item.work_item_id} {status}.",
        }
        if status == "blocked":
            artifact.update(
                {
                    "needs_planner_decision": True,
                    "blocker_type": "scope_boundary",
                    "planner_question": "Can Planner choose a safer local option?",
                    "continue_without_human_possible": False,
                }
            )
        return ExecutionRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status=status,
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
            "next_action": "ask_human",
            "risk_level": "medium",
            "requires_human_confirmation": True,
            "reason": "Executor requested Planner intervention.",
            "human_message": "Planner needs a user decision.",
        }


class MultiRoundExecutor:
    def __init__(self) -> None:
        self.planner_order_calls: list[dict[str, Any]] = []

    def create_planner_order(
        self,
        request: str,
        *,
        previous_bundle: PlannerInputBundle | None = None,
        previous_round_summary: dict[str, Any] | None = None,
        round_number: int = 1,
        emit=None,
    ) -> PlannerOrder:
        self.planner_order_calls.append(
            {
                "round": round_number,
                "previous_bundle": previous_bundle,
                "previous_round_summary": previous_round_summary,
            }
        )
        work_items = [_work_item("blocked-work", 1)] if round_number == 1 else [
            _work_item("fix-work", 1),
            _work_item("follow-up", 2, depends_on=["fix-work"]),
        ]
        return PlannerOrder.model_validate(
            {
                "artifact_type": "planner_order",
                "round": round_number,
                "round_goal": request,
                "plan_graph": {"work_items": work_items},
            }
        )

    def create_execution_result(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        emit=None,
    ) -> ExecutionRecord:
        status = "blocked" if envelope.round == 1 else "completed"
        artifact = {
            "artifact_type": "execution_result",
            "round": envelope.round,
            "work_item_id": item.work_item_id,
            "merge_index": item.merge_index,
            "agent_id": item.assignee_agent_id,
            "status": status,
            "summary": f"{item.work_item_id} {status}.",
        }
        if status == "blocked":
            artifact.update(
                {
                    "needs_planner_decision": True,
                    "blocker_type": "scope_boundary",
                    "planner_question": "Can Planner choose the local fix?",
                    "continue_without_human_possible": True,
                }
            )
        return ExecutionRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status=status,
            execution_summary=artifact["summary"],
            execution_result_ref=f"execution_result_{envelope.round}_{item.work_item_id}",
            artifact_payload=artifact,
        )

    def create_planner_decision(
        self,
        *,
        bundle: PlannerInputBundle,
        emit=None,
    ) -> dict[str, Any]:
        if bundle.interrupts:
            return {
                "artifact_type": "planner_decision",
                "round": bundle.round,
                "task_done": False,
                "next_action": "continue",
                "risk_level": "medium",
                "reason": "Continue with a safer local fix.",
                "next_round_goal": "Resolve blocked-work locally.",
                "remaining_auto_rounds": 1,
            }
        return {
            "artifact_type": "planner_decision",
            "round": bundle.round,
            "task_done": True,
            "next_action": "finish",
            "reason": "Second round finished.",
        }


class AlwaysContinueExecutor(MultiRoundExecutor):
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
            "summary": f"{item.work_item_id} completed.",
        }
        return ExecutionRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status="completed",
            execution_summary=artifact["summary"],
            execution_result_ref=f"execution_result_{envelope.round}_{item.work_item_id}",
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
            "reason": "Keep going.",
            "next_round_goal": "Plan another round.",
        }


class ResumeThroughPlannerExecutor(MultiRoundExecutor):
    def __init__(self) -> None:
        super().__init__()
        self.execution_calls: list[str] = []

    def create_execution_result(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        emit=None,
    ) -> ExecutionRecord:
        self.execution_calls.append(item.work_item_id)
        status = "blocked" if envelope.round == 1 else "completed"
        artifact = {
            "artifact_type": "execution_result",
            "round": envelope.round,
            "work_item_id": item.work_item_id,
            "merge_index": item.merge_index,
            "agent_id": item.assignee_agent_id,
            "status": status,
            "summary": f"{item.work_item_id} {status}.",
        }
        if status == "blocked":
            artifact.update(
                {
                    "needs_planner_decision": True,
                    "blocker_type": "scope_boundary",
                    "planner_question": "Does the user allow this local fix?",
                    "continue_without_human_possible": False,
                }
            )
        return ExecutionRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status=status,
            execution_summary=artifact["summary"],
            execution_result_ref=f"execution_result_{envelope.round}_{item.work_item_id}",
            artifact_payload=artifact,
        )

    def create_planner_decision(
        self,
        *,
        bundle: PlannerInputBundle,
        emit=None,
    ) -> dict[str, Any]:
        if bundle.interrupts:
            return {
                "artifact_type": "planner_decision",
                "round": bundle.round,
                "task_done": False,
                "next_action": "ask_human",
                "risk_level": "medium",
                "requires_human_confirmation": True,
                "reason": "User must confirm scope.",
                "human_message": "Can the executor use the local fix?",
            }
        return {
            "artifact_type": "planner_decision",
            "round": bundle.round,
            "task_done": True,
            "next_action": "finish",
            "reason": "Resumed round finished.",
        }


class FakeResponse:
    def __init__(self, content: str) -> None:
        self.content = content


class FakeChatModel:
    def __init__(self, responses: list[str]) -> None:
        self.responses = list(responses)
        self.prompts: list[str] = []

    def invoke(self, prompt: str) -> FakeResponse:
        self.prompts.append(prompt)
        return FakeResponse(self.responses[0])


def _work_item(work_item_id: str, merge_index: int, *, depends_on: list[str] | None = None) -> dict[str, Any]:
    return {
        "work_item_id": work_item_id,
        "merge_index": merge_index,
        "assignee_agent_id": "executor",
        "task_summary": f"Run {work_item_id}.",
        "depends_on": depends_on or [],
    }


def _bundle_with_interrupt(*, continue_without_human_possible: bool) -> PlannerInputBundle:
    return PlannerInputBundle.model_validate(
        {
            "artifact_type": "planner_input_bundle",
            "round": 1,
            "planner_order_ref": "planner_order_round_1",
            "plan_status": "interrupted",
            "items": [],
            "interrupts": [
                {
                    "round": 1,
                    "work_item_id": "A",
                    "merge_index": 1,
                    "agent_id": "executor",
                    "blocker_type": "scope_boundary",
                    "reason": "A blocked.",
                    "planner_question": "Can Planner continue?",
                    "continue_without_human_possible": continue_without_human_possible,
                    "candidate_options": [],
                    "artifact_ref": "execution_result_A",
                }
            ],
        }
    )


def _capture_workflow_supervisor_calls(executor: AgentGraphExecutor) -> list[dict[str, Any]]:
    calls: list[dict[str, Any]] = []
    original = executor.agent_run.harness_runtime_manager.run_workflow_supervisor

    def tracking_run_workflow_supervisor(**kwargs: Any):
        calls.append(kwargs)
        return original(**kwargs)

    executor.agent_run.harness_runtime_manager.run_workflow_supervisor = tracking_run_workflow_supervisor
    return calls


if __name__ == "__main__":
    unittest.main()
