from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

from coder_workbench.agent_engine import default_agent_engine_registry
from coder_workbench.agent_graph.prompts import build_planner_decision_prompt, build_planner_order_prompt
from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, ExecutionRecord, PlannerInputBundle, PlannerOrder, TestRecord, WorkItem
from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.coding import load_coding_task


class DebugFindingReplanTests(unittest.TestCase):
    def test_planner_prompts_include_debug_finding_guidance(self) -> None:
        workflow = default_planner_led_agent_workflow()
        bundle = _bundle_with_debug_finding()

        order_prompt = build_planner_order_prompt(
            request="Fix the failing check.",
            agent_workflow=workflow,
            previous_bundle=bundle,
        )
        decision_prompt = build_planner_decision_prompt(
            planner=workflow.agents[0],
            bundle=bundle,
        )

        self.assertIn("Debug findings from previous round:", order_prompt)
        self.assertIn("work_item_id=executor-work", order_prompt)
        self.assertIn("raw_output_ref=check_output_1", order_prompt)
        self.assertIn("prefer continue/replan", decision_prompt)
        self.assertIn("same error repeated", decision_prompt)

    def test_planner_engine_mock_continues_on_debug_finding(self) -> None:
        workflow = default_planner_led_agent_workflow()

        decision = default_agent_engine_registry().planner().run_planner_decision(
            agent_workflow=workflow,
            bundle=_bundle_with_debug_finding(),
        )

        self.assertEqual(decision["next_action"], "continue")
        self.assertIn("DebugFinding", decision["reason"])

    def test_coding_auto_loop_runs_two_rounds_without_human_prompt(self) -> None:
        fixture = load_coding_task(
            Path(__file__).parent / "fixtures" / "coding_tasks" / "python_bugfix_autoloop_001.json"
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repo = root / "repo"
            sandbox = root / "sandbox"
            repo.mkdir()
            sandbox.mkdir()
            (repo / "sample.py").write_text("value = 1\n", encoding="utf-8")
            (sandbox / "sample.py").write_text("value = 1\n", encoding="utf-8")

            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=AutoLoopExecutor(),
            ).run(
                fixture.request,
                str(repo),
                initial_data={"sandbox_root": str(sandbox), "max_auto_rounds": 2},
            )

            self.assertEqual((repo / "sample.py").read_text(encoding="utf-8"), "value = 1\n")

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.data["coding_eval"]["planner_rounds"], 2)
        self.assertEqual(result.data["coding_eval"]["human_prompt_rate"], 0.0)
        self.assertGreaterEqual(result.data["coding_eval"]["details"]["debug_findings"], 1)
        self.assertTrue(result.data["coding_eval"]["details"]["sandbox_checks_passed"])
        self.assertFalse(any(event.type == "planner.human_prompt" for event in result.events))


class AutoLoopExecutor:
    def create_planner_order(
        self,
        request: str,
        *,
        previous_bundle: PlannerInputBundle | None = None,
        previous_round_summary: dict[str, Any] | None = None,
        planner_human_response: dict[str, Any] | None = None,
        round_number: int = 1,
        emit=None,
        **kwargs: Any,
    ) -> PlannerOrder:
        return PlannerOrder.model_validate(
            {
                "artifact_type": "planner_order",
                "round": round_number,
                "round_goal": request,
                "plan_graph": {
                    "work_items": [
                        {
                            "work_item_id": "executor-work",
                            "merge_index": 1,
                            "assignee_agent_id": "executor",
                            "task_summary": "Patch sample.py and verify it.",
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
        content = "value = 1\n" if envelope.round == 1 else "value = 2\n"
        before = "value = 1\n"
        artifact = {
            "artifact_type": "execution_result",
            "round": envelope.round,
            "work_item_id": item.work_item_id,
            "merge_index": item.merge_index,
            "agent_id": item.assignee_agent_id,
            "status": "completed",
            "summary": f"Round {envelope.round} proposed a patch.",
            "proposed_changes": [
                {
                    "path": "sample.py",
                    "action": "update",
                    "expected_before": before,
                    "content": content,
                }
            ],
        }
        return ExecutionRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status="completed",
            execution_summary=artifact["summary"],
            execution_result_ref=f"execution_result_round_{envelope.round}",
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
        round_number = int(execution_artifact.get("round") or 1)
        artifact = {
            "artifact_type": "test_result",
            "round": round_number,
            "work_item_id": item.work_item_id,
            "merge_index": item.merge_index,
            "tester_agent_id": tester_agent_id,
            "status": "pass",
            "summary": "Tester requested sandbox verification.",
            "check_commands": [
                f'"{sys.executable}" -c "from pathlib import Path; ns={{}}; exec(Path(\'sample.py\').read_text(), ns); assert ns[\'value\'] == 2"'
            ],
        }
        return TestRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            tester_agent_id=tester_agent_id,
            status="pass",
            test_summary=artifact["summary"],
            test_result_ref=f"test_result_round_{round_number}",
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
            "task_done": bundle.round >= 2,
            "next_action": "finish" if bundle.round >= 2 else "continue",
            "reason": "Continue after debug finding." if bundle.round == 1 else "Sandbox check passed.",
            "next_round_goal": "Fix the failed sandbox check." if bundle.round == 1 else "",
        }


def _bundle_with_debug_finding() -> PlannerInputBundle:
    return PlannerInputBundle(
        round=1,
        planner_order_ref="planner_order_round_1",
        plan_status="partial_failed",
        items=[],
        effects=[
            {
                "effect_type": "debug_finding",
                "status": "created",
                "work_item_id": "executor-work",
                "failure_summary": "AssertionError: value was 0",
                "likely_files": ["sample.py"],
                "raw_output_ref": "check_output_1",
            }
        ],
    )


if __name__ == "__main__":
    unittest.main()
