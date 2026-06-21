from __future__ import annotations

import unittest

from coder_workbench.agent_graph.round_budget import evaluate_round_budget_preflight
from coder_workbench.agent_graph.schema import PlannerOrder
from coder_workbench.budget import BudgetBroker, BudgetLimit
from coder_workbench.runtime_kernel import RunController


class RoundBudgetTests(unittest.TestCase):
    def test_round_budget_preflight_helper_blocks_denied_round(self) -> None:
        broker = BudgetBroker(BudgetLimit(max_model_calls=0))
        controller = RunController()
        order = PlannerOrder.model_validate(
            {
                "artifact_type": "planner_order",
                "round": 1,
                "round_goal": "Stay inside budget.",
                "plan_graph": {
                    "work_items": [
                        {
                            "work_item_id": "work",
                            "merge_index": 1,
                            "assignee_agent_id": "executor",
                            "task_summary": "Do work.",
                            "depends_on": [],
                            "tester_agent_ids": [],
                        }
                    ]
                },
            }
        )

        report, decision = evaluate_round_budget_preflight(
            broker=broker,
            controller=controller,
            run_id="run",
            planner_order=order,
            estimated_model_calls=1,
            estimated_tool_calls=0,
            estimated_context_tokens_per_call=100,
        )

        self.assertFalse(report["approved"])
        self.assertEqual(report["reason"], "round_model_call_budget_exceeded")
        self.assertEqual(decision.action, "blocked")


if __name__ == "__main__":
    unittest.main()
