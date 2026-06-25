from __future__ import annotations

import unittest

from coder_workbench.agent_graph.planner_strategy import PlannerStrategyContext, planner_strategy_for_mode
from coder_workbench.agent_graph.schema import PlannerInputBundle
from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.coding import build_run_coding_eval


class CodingEvalGateTests(unittest.TestCase):
    def test_coding_eval_reports_runtime_effect_categories(self) -> None:
        report = build_run_coding_eval(
            {
                "graph_run_cache": {
                    "round": 1,
                    "execution_cache": {"executor-work": {"status": "completed"}},
                    "test_cache": {"executor-work": [{"status": "pass"}]},
                    "hidden_effects": [
                        {"effect_type": "modify_files", "action_type": "propose_patch", "status": "patch_preview_created"},
                        {"effect_type": "sandbox_apply", "action_type": "apply_patch_sandbox", "status": "applied"},
                        {
                            "effect_type": "optional_check_command",
                            "action_type": "run_command_sandbox",
                            "status": "failed",
                            "passed": False,
                        },
                        {"effect_type": "debug_finding", "status": "created"},
                        {"effect_type": "runtime_action", "action_type": "call_plugin", "status": "blocked"},
                    ],
                    "interrupts": [],
                },
                "rounds": [{"round": 1}],
                "debug_findings": [{"artifact_type": "debug_finding"}],
            }
        )

        details = report["details"]

        self.assertEqual(report["task_pass_rate"], 0.0)
        self.assertEqual(details["patch_preview_count"], 1)
        self.assertEqual(details["sandbox_apply_count"], 1)
        self.assertEqual(details["check_result_count"], 1)
        self.assertEqual(details["failed_check_results"], 1)
        self.assertEqual(details["debug_finding_count"], 1)
        self.assertEqual(details["runtime_action_count"], 1)
        self.assertEqual(details["blocked_runtime_actions"], 1)

    def test_failed_check_effect_pushes_planner_replan(self) -> None:
        bundle = PlannerInputBundle(
            round=1,
            planner_order_ref="planner_order_round_1",
            plan_status="completed",
            items=[],
            effects=[
                {
                    "effect_type": "optional_check_command",
                    "action_type": "run_command_sandbox",
                    "status": "failed",
                    "passed": False,
                    "reason": "Unit test failed.",
                }
            ],
        )
        decision = planner_strategy_for_mode("simple").create_decision(
            PlannerStrategyContext(
                agent_workflow=default_planner_led_agent_workflow(),
                bundle=bundle,
            )
        )

        self.assertIsNotNone(decision)
        assert decision is not None
        self.assertEqual(decision["next_action"], "continue")
        self.assertFalse(decision["task_done"])
        self.assertIn("check", decision["reason"].lower())


if __name__ == "__main__":
    unittest.main()
