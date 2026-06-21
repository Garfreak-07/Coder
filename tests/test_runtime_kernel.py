from __future__ import annotations

import unittest

from coder_workbench.agent_graph.schema import PlannerOrder
from coder_workbench.runtime_kernel import RunController, RunGuard, fingerprint_planner_order


def _planner_order(goal: str = "Do work.") -> PlannerOrder:
    return PlannerOrder.model_validate(
        {
            "round_goal": goal,
            "plan_graph": {
                "work_items": [
                    {
                        "work_item_id": "work",
                        "merge_index": 1,
                        "assignee_agent_id": "executor",
                        "task_summary": "Implement the change.",
                        "depends_on": [],
                        "tester_agent_ids": ["tester"],
                    }
                ]
            },
        }
    )


class RunControllerTests(unittest.TestCase):
    def test_finish_and_ask_human_actions_pass_through(self) -> None:
        controller = RunController(guard=RunGuard(max_rounds=2))

        self.assertEqual(controller.evaluate_planner_decision({"next_action": "finish"}).action, "finish")
        decision = controller.evaluate_planner_decision({"next_action": "ask_human", "reason": "Need input."})

        self.assertEqual(decision.action, "ask_human")
        self.assertEqual(decision.status_code, "planner_ask_human")

    def test_continue_within_max_rounds_passes(self) -> None:
        controller = RunController(guard=RunGuard(max_rounds=2))
        controller.record_round(round_number=1, planner_order=_planner_order())

        decision = controller.evaluate_planner_decision({"next_action": "continue"}, round_number=1)

        self.assertEqual(decision.action, "continue")

    def test_continue_over_max_rounds_blocks(self) -> None:
        controller = RunController(guard=RunGuard(max_rounds=1))
        controller.record_round(round_number=1, planner_order=_planner_order())

        decision = controller.evaluate_planner_decision({"next_action": "continue"}, round_number=1)

        self.assertEqual(decision.action, "blocked")
        self.assertEqual(decision.status_code, "max_auto_rounds_reached")

    def test_plan_fingerprint_ignores_round_number(self) -> None:
        first = _planner_order()
        second = first.model_copy(update={"round": 2})

        self.assertEqual(fingerprint_planner_order(first), fingerprint_planner_order(second))

    def test_repeated_plan_over_threshold_blocks(self) -> None:
        controller = RunController(guard=RunGuard(max_rounds=5, max_same_plan_repeats=2))
        order = _planner_order()
        controller.record_round(round_number=1, planner_order=order)
        controller.record_round(round_number=2, planner_order=order)
        under = controller.evaluate_planner_decision({"next_action": "continue"}, round_number=2)
        controller.record_round(round_number=3, planner_order=order)

        over = controller.evaluate_planner_decision({"next_action": "continue"}, round_number=3)

        self.assertEqual(under.action, "continue")
        self.assertEqual(over.action, "blocked")
        self.assertEqual(over.status_code, "repeated_plan_fingerprint")

    def test_diagnostics_include_guard_rounds_and_counters(self) -> None:
        controller = RunController(guard=RunGuard(max_rounds=3))
        controller.record_round(
            round_number=1,
            planner_order=_planner_order(),
            agent_calls=2,
            tool_calls=1,
            estimated_tokens=30,
        )

        diagnostics = controller.diagnostics()

        self.assertEqual(diagnostics["guard"]["max_rounds"], 3)
        self.assertEqual(diagnostics["agent_calls"], 2)
        self.assertEqual(diagnostics["tool_calls"], 1)
        self.assertEqual(diagnostics["estimated_tokens"], 30)
        self.assertEqual(diagnostics["rounds"][0]["round"], 1)
        self.assertTrue(diagnostics["rounds"][0]["plan_fingerprint"])


if __name__ == "__main__":
    unittest.main()
