from __future__ import annotations

import os
import tempfile
import unittest
from typing import Any

from coder_workbench.agent_graph.agent_run import AgentRun
from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_graph.schema import (
    AgentTaskEnvelope,
    ExecutionRecord,
    PlannerInputBundle,
    PlannerOrder,
    WorkItem,
)
from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.harness_runtime import HarnessRuntimeManager
from coder_workbench.harness_runtime.profiles import INTERNAL_FALLBACK_PROVIDER_ID, OPENHANDS_PROVIDER_ID
from coder_workbench.harness_runtime.runtime_context import HarnessRunRequest, HarnessRunResult


class OpenHandsAgentGraphIntegrationTests(unittest.TestCase):
    def test_run_execution_adapts_openhands_execution_result_to_execution_record(self) -> None:
        provider = FakeOpenHandsProvider()
        agent_run = _agent_run(provider)
        item = _work_item()
        envelope = _task_envelope(item)

        with _env("CODER_ENABLE_OPENHANDS_RUNTIME", "1"):
            execution = agent_run.run_execution(item=item, envelope=envelope)

        self.assertIsInstance(execution, ExecutionRecord)
        self.assertEqual(execution.status, "completed")
        self.assertEqual(execution.work_item_id, item.work_item_id)
        self.assertEqual(execution.execution_result_ref, f"execution_result_{item.work_item_id}")
        self.assertEqual(execution.artifact_payload["artifact_type"], "execution_result")
        self.assertEqual(execution.artifact_payload["verification"]["status"], "skipped")
        self.assertTrue(execution.artifact_payload["verification"]["no_check_rationale"])
        self.assertEqual(provider.calls_by_target["execution_result"], 1)
        self.assertEqual(agent_run.harness_runtime_manager.providers[INTERNAL_FALLBACK_PROVIDER_ID].calls, 0)

    def test_run_planner_order_requests_and_consumes_openhands_planner_order(self) -> None:
        provider = FakeOpenHandsProvider()
        agent_run = _agent_run(provider)

        with _env("CODER_ENABLE_OPENHANDS_RUNTIME", "1"):
            order = agent_run.run_planner_order("Create one executor task.")

        self.assertIsInstance(order, PlannerOrder)
        self.assertEqual(order.artifact_type, "planner_order")
        self.assertEqual(order.plan_graph.work_items[0].work_item_id, "executor-work")
        self.assertEqual(provider.requests[0].input_artifacts["requested_artifact_type"], "planner_order")
        self.assertEqual(provider.calls_by_target["planner_order"], 1)
        self.assertEqual(agent_run.harness_runtime_manager.providers[INTERNAL_FALLBACK_PROVIDER_ID].calls, 0)

    def test_planner_task_state_is_available_to_workflow_supervisor(self) -> None:
        provider = FakeOpenHandsProvider()
        agent_run = _agent_run(
            provider,
            initial_data={
                "repo_root": ".",
                "planner_task_state": {
                    "goal": "Implement the planned task.",
                    "success_criteria": ["Planner state is visible to the supervisor."],
                    "readiness": "ready_to_execute",
                },
            },
        )

        with _env("CODER_ENABLE_OPENHANDS_RUNTIME", "1"):
            agent_run.run_planner_order("Create one executor task.")

        request = provider.requests[0]
        self.assertEqual(request.input_artifacts["planner_task_state"]["goal"], "Implement the planned task.")
        self.assertEqual(
            request.context.context_packet["warm"]["planner_task_state"]["readiness"],
            "ready_to_execute",
        )

    def test_run_planner_decision_requests_and_consumes_openhands_planner_decision(self) -> None:
        provider = FakeOpenHandsProvider()
        agent_run = _agent_run(provider)

        with _env("CODER_ENABLE_OPENHANDS_RUNTIME", "1"):
            decision = agent_run.run_planner_decision(bundle=_planner_input_bundle())

        self.assertEqual(decision["artifact_type"], "planner_decision")
        self.assertEqual(decision["next_action"], "finish")
        self.assertEqual(decision["final_status"], "completed")
        self.assertEqual(provider.requests[0].input_artifacts["requested_artifact_type"], "planner_decision")
        self.assertEqual(provider.calls_by_target["planner_decision"], 1)
        self.assertEqual(agent_run.harness_runtime_manager.providers[INTERNAL_FALLBACK_PROVIDER_ID].calls, 0)

    def test_minimal_agent_graph_run_uses_openhands_for_critical_path_without_fallback(self) -> None:
        provider = FakeOpenHandsProvider()
        agent_run = _agent_run(provider)

        with tempfile.TemporaryDirectory() as tmp:
            with _env("CODER_ENABLE_OPENHANDS_RUNTIME", "1"):
                result = AgentGraphRunner(default_planner_led_agent_workflow(), agent_run=agent_run).run(
                    "Complete the fake OpenHands path.",
                    tmp,
                    initial_data={"max_auto_rounds": 1},
                )

        self.assertEqual(result.status, "completed")
        self.assertEqual(provider.calls_by_target["planner_order"], 1)
        self.assertEqual(provider.calls_by_target["execution_result"], 1)
        self.assertEqual(provider.calls_by_target["planner_decision"], 1)
        self.assertEqual(agent_run.harness_runtime_manager.providers[INTERNAL_FALLBACK_PROVIDER_ID].calls, 0)
        self.assertEqual(
            [
                request.profile.tool_policy.get("write_files")
                for request in provider.requests
                if request.mode == "workflow_supervisor"
            ],
            [False, False],
        )
        task_request = next(request for request in provider.requests if request.mode == "task_execution")
        self.assertFalse(task_request.profile.tool_policy.get("ask_human"))
        self.assertFalse(task_request.profile.safety_policy.get("git_commit"))
        self.assertFalse(task_request.profile.safety_policy.get("git_push"))
        self.assertFalse(task_request.profile.safety_policy.get("deploy"))


class FakeOpenHandsProvider:
    provider_id = OPENHANDS_PROVIDER_ID

    def __init__(self) -> None:
        self.requests: list[HarnessRunRequest] = []
        self.calls_by_target = {
            "planner_order": 0,
            "execution_result": 0,
            "planner_decision": 0,
            "final_report": 0,
        }

    def is_available(self) -> bool:
        return True

    def run(self, request: HarnessRunRequest, *, emit: Any | None = None) -> HarnessRunResult:
        self.requests.append(request)
        target = str(request.input_artifacts.get("requested_artifact_type") or "")
        if not target:
            target = {
                "planner_order": "planner_order",
                "task_execution": "execution_result",
                "planner_decision": "planner_decision",
            }.get(str(request.input_artifacts.get("legacy_operation") or ""), "final_report")
        self.calls_by_target[target] += 1
        artifact = self._artifact_for_target(request, target)
        return HarnessRunResult(
            status="completed",
            artifact_type=target,
            artifact=artifact,
            native_event_refs=[f"native-{target}-{self.calls_by_target[target]}"],
            evidence_refs=[f"evidence-{target}-{self.calls_by_target[target]}"],
        )

    def _artifact_for_target(self, request: HarnessRunRequest, target: str) -> dict[str, Any]:
        if target == "planner_order":
            return {
                "artifact_type": "planner_order",
                "round": request.context.round or 1,
                "round_goal": "Fake OpenHands planner order.",
                "plan_graph": {
                    "work_items": [
                        {
                            "work_item_id": "executor-work",
                            "merge_index": 1,
                            "assignee_agent_id": "executor",
                            "task_summary": "Run fake OpenHands execution.",
                            "depends_on": [],
                        }
                    ]
                },
                "instructions_for_executor": ["Use the task_execution harness."],
                "allowed_actions": ["modify_files", "run_commands"],
                "forbidden_actions": ["commit", "push", "deploy"],
                "expected_outputs": ["execution_result"],
                "risk_level": "low",
            }
        if target == "execution_result":
            work_item = request.input_artifacts.get("work_item")
            merge_index = work_item.get("merge_index") if isinstance(work_item, dict) else 1
            work_item_id = str(request.input_artifacts.get("work_item_id") or "executor-work")
            return {
                "artifact_type": "execution_result",
                "round": request.context.round or 1,
                "work_item_id": work_item_id,
                "merge_index": int(merge_index or 1),
                "agent_id": request.context.agent_id,
                "status": "completed",
                "summary": "Fake OpenHands execution completed.",
                "changed_files": [],
                "created_files": [],
                "deleted_files": [],
                "patch_refs": [],
                "evidence_refs": ["fake-execution-evidence"],
                "no_op_rationale": "Fake OpenHands provider intentionally performed no file changes.",
                "verification": {
                    "status": "skipped",
                    "checks_run": [],
                    "evidence_refs": ["fake-execution-evidence"],
                    "confidence": "medium",
                    "no_check_rationale": "Fake OpenHands provider did not run external checks.",
                },
            }
        if target == "planner_decision":
            return {
                "artifact_type": "planner_decision",
                "round": request.context.round or 1,
                "task_done": True,
                "next_action": "finish",
                "final_status": "completed",
                "risk_level": "low",
                "requires_human_confirmation": False,
                "reason": "Fake OpenHands execution result is consumable by AgentGraph.",
                "next_round_goal": "",
                "remaining_auto_rounds": 0,
                "human_message": None,
            }
        return {
            "artifact_type": "final_report",
            "status": "completed",
            "summary": "Fake final report.",
            "checks": [],
            "completed": ["Fake path completed."],
            "blocked_by": [],
            "failed_by": [],
            "warnings": [],
            "next_steps": [],
            "evidence_refs": ["fake-final-evidence"],
        }


class FailingFallbackProvider:
    provider_id = INTERNAL_FALLBACK_PROVIDER_ID

    def __init__(self) -> None:
        self.calls = 0

    def is_available(self) -> bool:
        return True

    def run(self, request: HarnessRunRequest, *, emit: Any | None = None) -> HarnessRunResult:
        self.calls += 1
        return HarnessRunResult(status="failed", error={"code": "fallback_used", "message": "fallback"})


class _env:
    def __init__(self, key: str, value: str | None) -> None:
        self.key = key
        self.value = value
        self.old = os.environ.get(key)

    def __enter__(self) -> None:
        if self.value is None:
            os.environ.pop(self.key, None)
        else:
            os.environ[self.key] = self.value

    def __exit__(self, *_args: object) -> None:
        if self.old is None:
            os.environ.pop(self.key, None)
        else:
            os.environ[self.key] = self.old


def _agent_run(provider: FakeOpenHandsProvider, initial_data: dict[str, Any] | None = None) -> AgentRun:
    agent_run = AgentRun(default_planner_led_agent_workflow(), initial_data=initial_data or {"repo_root": "."})
    agent_run.harness_runtime_manager = HarnessRuntimeManager(
        providers=[provider, FailingFallbackProvider()]
    )
    return agent_run


def _work_item() -> WorkItem:
    return WorkItem(
        work_item_id="executor-work",
        merge_index=1,
        assignee_agent_id="executor",
        task_summary="Run fake OpenHands execution.",
        depends_on=[],
    )


def _task_envelope(item: WorkItem) -> AgentTaskEnvelope:
    return AgentTaskEnvelope(
        round=1,
        work_item_id=item.work_item_id,
        merge_index=item.merge_index,
        assigned_agent_id=item.assignee_agent_id,
        task_summary=item.task_summary,
        planner_order_ref="planner_order_round_1",
    )


def _planner_input_bundle() -> PlannerInputBundle:
    return PlannerInputBundle(
        round=1,
        planner_order_ref="planner_order_round_1",
        plan_status="completed",
        items=[
            {
                "work_item_id": "executor-work",
                "merge_index": 1,
                "task_summary": "Run fake OpenHands execution.",
                "execution_status": "completed",
                "execution_summary": "Fake OpenHands execution completed.",
                "verification_status": "skipped",
                "verification_summary": "Fake provider did not run external checks.",
                "refs": ["execution_result_executor-work"],
            }
        ],
    )


if __name__ == "__main__":
    unittest.main()
