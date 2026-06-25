from __future__ import annotations

import tempfile
import unittest
from typing import Any

from coder_workbench.agent_graph.agent_run import AgentRun
from coder_workbench.agent_graph.planner_strategy import PlannerStrategyContext, SimplePlannerStrategy
from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, ExecutionRecord, PlannerInputBundle, PlannerOrder, WorkItem
from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.harness_runtime.runtime_context import HarnessRunResult


class WorkflowSupervisorDecisionTests(unittest.TestCase):
    def test_failed_verification_causes_continue_retry_when_allowed(self) -> None:
        decision = _simple_decision(
            PlannerInputBundle(
                round=1,
                planner_order_ref="planner_order_round_1",
                plan_status="blocked",
                items=[
                    {
                        "work_item_id": "executor-work",
                        "merge_index": 1,
                        "task_summary": "Fix tests.",
                        "execution_status": "blocked",
                        "execution_summary": "Implemented code.",
                        "verification_status": "fail",
                        "verification_summary": "Unit tests failed.",
                        "refs": ["execution_result_executor-work"],
                    }
                ],
            )
        )

        self.assertEqual(decision["next_action"], "continue")
        self.assertIn("retry", decision["reason"])
        self.assertIn("Fix failed execution verification", decision["next_round_goal"])

    def test_completed_verification_causes_finish_completed(self) -> None:
        decision = _simple_decision(
            PlannerInputBundle(
                round=1,
                planner_order_ref="planner_order_round_1",
                plan_status="completed",
                items=[
                    {
                        "work_item_id": "executor-work",
                        "merge_index": 1,
                        "task_summary": "Finish work.",
                        "execution_status": "completed",
                        "execution_summary": "Done.",
                        "verification_status": "pass",
                        "verification_summary": "Checks passed.",
                        "refs": ["execution_result_executor-work"],
                    }
                ],
            )
        )

        self.assertEqual(decision["next_action"], "finish")
        self.assertTrue(decision["task_done"])
        self.assertEqual(decision["final_status"], "completed")

    def test_policy_blocker_finishes_blocked_even_when_auto_resolvable(self) -> None:
        decision = _simple_decision(
            PlannerInputBundle(
                round=1,
                planner_order_ref="planner_order_round_1",
                plan_status="interrupted",
                items=[],
                interrupts=[
                    {
                        "round": 1,
                        "work_item_id": "executor-work",
                        "merge_index": 1,
                        "agent_id": "executor",
                        "blocker_type": "scope_violation",
                        "reason": "Requested write is outside the sandbox.",
                        "continue_without_human_possible": True,
                        "artifact_ref": "execution_result_executor-work",
                    }
                ],
            )
        )

        self.assertEqual(decision["next_action"], "finish")
        self.assertEqual(decision["final_status"], "blocked")
        self.assertIn("policy blocked", decision["reason"])

    def test_repeated_blocker_causes_finish_blocked(self) -> None:
        executor = BlockingProgressExecutor(
            summaries=["Dependency command is missing.", "Dependency command is missing."],
            evidence_refs_by_round=[["round-1-evidence"], ["round-2-evidence"]],
        )

        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow(), executor=executor).run(
                "Repair dependency failure.",
                tmp,
                initial_data={"max_auto_rounds": 3},
            )

        self.assertEqual(result.status, "blocked")
        self.assertEqual(executor.execution_calls, 2)
        self.assertEqual(result.data["planner_decision"]["final_status"], "blocked")
        self.assertIn("same blocker repeated", result.data["planner_decision"]["reason"])

    def test_no_new_evidence_causes_finish_blocked(self) -> None:
        executor = BlockingProgressExecutor(
            summaries=["First diagnosis.", "Different diagnosis without new evidence."],
            evidence_refs_by_round=[["same-evidence-ref"], ["same-evidence-ref"]],
        )

        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow(), executor=executor).run(
                "Repair without new evidence.",
                tmp,
                initial_data={"max_auto_rounds": 3},
            )

        self.assertEqual(result.status, "blocked")
        self.assertEqual(executor.execution_calls, 2)
        self.assertEqual(result.data["planner_decision"]["final_status"], "blocked")
        self.assertIn("no new diff or evidence", result.data["planner_decision"]["reason"])

    def test_workflow_supervisor_context_includes_summaries_and_runtime_refs(self) -> None:
        manager = CapturingHarnessManager()
        agent_run = AgentRun(
            default_planner_led_agent_workflow(),
            initial_data={
                "request": "Decide next step.",
                "round_summary": {
                    "artifact_type": "round_summary",
                    "round": 1,
                    "planner_order_ref": "planner_order_round_1",
                    "plan_status": "blocked",
                },
                "graph_run_cache": {
                    "execution_cache": {
                        "executor-work": {
                            "artifact_payload": _execution_artifact(
                                round_number=1,
                                status="blocked",
                                summary="Tests failed.",
                                evidence_refs=["execution-evidence"],
                            )
                        }
                    },
                    "native_runtime_refs": {"executor-work": ["native-ref"]},
                    "diff_refs": {"executor-work": ["diff-ref"]},
                    "log_refs": {"executor-work": ["log-ref"]},
                },
            },
        )
        agent_run.harness_runtime_manager = manager

        decision = agent_run.run_planner_decision(
            bundle=PlannerInputBundle(
                round=1,
                planner_order_ref="planner_order_round_1",
                plan_status="blocked",
                items=[
                    {
                        "work_item_id": "executor-work",
                        "merge_index": 1,
                        "task_summary": "Fix tests.",
                        "execution_status": "blocked",
                        "execution_summary": "Tests failed.",
                        "verification_status": "blocked",
                        "verification_summary": "Tests failed.",
                        "refs": ["execution_result_executor-work"],
                    }
                ],
            )
        )

        packet = manager.context.context_packet
        self.assertEqual(decision["next_action"], "finish")
        self.assertEqual(packet["warm"]["round_summary"]["artifact_type"], "round_summary")
        self.assertEqual(packet["warm"]["execution_result_summaries"][0]["artifact_id"], "execution_result_executor-work")
        self.assertEqual(packet["warm"]["verification_summaries"][0]["evidence_refs"], ["execution-evidence"])
        self.assertIn({"ref_type": "native_runtime", "refs": ["native-ref"]}, packet["cold_refs"])
        self.assertIn({"ref_type": "diff", "refs": ["diff-ref"]}, packet["cold_refs"])
        self.assertIn({"ref_type": "log", "refs": ["log-ref"]}, packet["cold_refs"])
        self.assertNotIn("raw_runtime_json", str(packet))

    def test_final_report_includes_execution_evidence_refs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=CompletedEvidenceExecutor(),
            ).run("Finish with evidence.", tmp)

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.data["final_report"]["status"], "completed")
        self.assertIn("execution-evidence", result.data["final_report"]["evidence_refs"])
        self.assertIn("diff-ref", result.data["final_report"]["evidence_refs"])
        self.assertIn("check-output", result.data["final_report"]["evidence_refs"])


class BlockingProgressExecutor:
    def __init__(self, *, summaries: list[str], evidence_refs_by_round: list[list[str]]) -> None:
        self.summaries = summaries
        self.evidence_refs_by_round = evidence_refs_by_round
        self.execution_calls = 0

    def create_planner_order(
        self,
        request: str,
        *,
        round_number: int = 1,
        emit=None,
        **kwargs: Any,
    ) -> PlannerOrder:
        return _planner_order(round_number=round_number, task_summary=request)

    def create_execution_result(self, *, item: WorkItem, envelope: AgentTaskEnvelope, emit=None) -> ExecutionRecord:
        self.execution_calls += 1
        index = min(self.execution_calls - 1, len(self.summaries) - 1)
        artifact = _execution_artifact(
            round_number=envelope.round,
            status="blocked",
            summary=self.summaries[index],
            evidence_refs=self.evidence_refs_by_round[index],
        )
        artifact_ref = f"execution_result_{item.work_item_id}_round_{envelope.round}"
        artifact["artifact_id"] = artifact_ref
        return ExecutionRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status="blocked",
            execution_summary=artifact["summary"],
            execution_result_ref=artifact_ref,
            artifact_payload=artifact,
        )

    def create_planner_decision(self, *, bundle: PlannerInputBundle, emit=None) -> dict[str, Any]:
        return {
            "artifact_type": "planner_decision",
            "round": bundle.round,
            "task_done": False,
            "next_action": "continue",
            "reason": "Retry blocked work.",
            "next_round_goal": "Retry with blocker evidence.",
            "remaining_auto_rounds": 2,
        }


class CompletedEvidenceExecutor:
    def create_planner_order(
        self,
        request: str,
        *,
        round_number: int = 1,
        emit=None,
        **kwargs: Any,
    ) -> PlannerOrder:
        return _planner_order(round_number=round_number, task_summary=request)

    def create_execution_result(self, *, item: WorkItem, envelope: AgentTaskEnvelope, emit=None) -> ExecutionRecord:
        artifact = _execution_artifact(
            round_number=envelope.round,
            status="completed",
            summary="Implemented with evidence.",
            evidence_refs=["execution-evidence"],
        )
        artifact.update(
            {
                "changed_files": ["src/app.py"],
                "patch_refs": ["diff-ref"],
                "verification": {
                    "status": "pass",
                    "checks_run": [
                        {
                            "check_id": "unit",
                            "kind": "command",
                            "command": "python -m unittest",
                            "status": "pass",
                            "summary": "Tests passed.",
                            "output_ref": "check-output",
                            "evidence_refs": ["check-output"],
                        }
                    ],
                    "evidence_refs": ["execution-evidence"],
                    "confidence": "high",
                    "remaining_work": [],
                    "no_check_rationale": None,
                    "repair_attempted": False,
                    "repair_summary": None,
                },
            }
        )
        return ExecutionRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status="completed",
            execution_summary=artifact["summary"],
            execution_result_ref=f"execution_result_{item.work_item_id}",
            artifact_payload=artifact,
        )

    def create_planner_decision(self, *, bundle: PlannerInputBundle, emit=None) -> dict[str, Any]:
        return {
            "artifact_type": "planner_decision",
            "round": bundle.round,
            "task_done": True,
            "next_action": "finish",
            "final_status": "completed",
            "reason": "Evidence is complete.",
            "remaining_auto_rounds": 0,
        }


class CapturingHarnessManager:
    def run_workflow_supervisor(
        self,
        *,
        context: Any,
        input_artifacts: dict[str, Any] | None = None,
        request_id: str | None = None,
        profile_id: str = "internal-fallback-workflow-supervisor",
        emit: Any | None = None,
    ) -> HarnessRunResult:
        self.context = context
        return HarnessRunResult(
            status="completed",
            artifact_type="planner_decision",
            artifact={
                "artifact_type": "planner_decision",
                "round": 1,
                "task_done": True,
                "next_action": "finish",
                "final_status": "completed",
                "reason": "Captured context.",
            },
        )


def _simple_decision(bundle: PlannerInputBundle) -> dict[str, Any]:
    decision = SimplePlannerStrategy().create_decision(
        PlannerStrategyContext(
            agent_workflow=default_planner_led_agent_workflow(),
            round_number=bundle.round,
            bundle=bundle,
        )
    )
    assert decision is not None
    return decision


def _planner_order(*, round_number: int, task_summary: str) -> PlannerOrder:
    return PlannerOrder.model_validate(
        {
            "artifact_type": "planner_order",
            "round": round_number,
            "round_goal": task_summary,
            "plan_graph": {
                "work_items": [
                    {
                        "work_item_id": "executor-work",
                        "merge_index": 1,
                        "assignee_agent_id": "executor",
                        "task_summary": task_summary,
                        "depends_on": [],
                    }
                ]
            },
        }
    )


def _execution_artifact(
    *,
    round_number: int,
    status: str,
    summary: str,
    evidence_refs: list[str],
) -> dict[str, Any]:
    artifact = {
        "artifact_type": "execution_result",
        "artifact_id": "execution_result_executor-work",
        "round": round_number,
        "work_item_id": "executor-work",
        "merge_index": 1,
        "agent_id": "executor",
        "status": status,
        "summary": summary,
        "evidence_refs": evidence_refs,
    }
    if status == "blocked":
        artifact.update(
            {
                "unexpected_issues": ["missing_dependency"],
                "remaining_work": [summary],
                "needs_planner_decision": True,
                "blocker_type": "missing_dependency",
                "blocker_reason": summary,
                "continue_without_human_possible": True,
                "verification": {
                    "status": "blocked",
                    "checks_run": [],
                    "evidence_refs": evidence_refs,
                    "confidence": "low",
                    "remaining_work": [summary],
                    "no_check_rationale": None,
                    "repair_attempted": False,
                    "repair_summary": None,
                },
            }
        )
    return artifact


if __name__ == "__main__":
    unittest.main()
