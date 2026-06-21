from __future__ import annotations

import unittest

from coder_workbench.agent_engine import (
    CodeWorkerEngine,
    FinalReviewEngine,
    PlannerEngine,
    SynthesizerEngine,
    TesterEngine,
    default_agent_engine_registry,
)
from coder_workbench.core import default_planner_led_agent_workflow


class AgentEngineRegistryTests(unittest.TestCase):
    def test_default_registry_contains_control_plane_engines(self) -> None:
        registry = default_agent_engine_registry()

        self.assertEqual(
            registry.ids(),
            [
                "code-worker-engine",
                "final-review-engine",
                "planner-engine",
                "synthesizer-engine",
                "tester-engine",
            ],
        )
        self.assertIsInstance(registry.get("code-worker-engine"), CodeWorkerEngine)
        self.assertIsInstance(registry.planner(), PlannerEngine)
        self.assertIsInstance(registry.tester(), TesterEngine)
        self.assertIsInstance(registry.final_review(), FinalReviewEngine)
        self.assertIsInstance(registry.synthesizer(), SynthesizerEngine)

    def test_unknown_engine_raises_key_error(self) -> None:
        with self.assertRaises(KeyError):
            default_agent_engine_registry().get("missing-engine")

    def test_planner_engine_mock_mode_returns_valid_planner_order(self) -> None:
        workflow = default_planner_led_agent_workflow()

        order = default_agent_engine_registry().planner().run_planner_order(
            "Plan a small coding task.",
            agent_workflow=workflow,
        )

        self.assertEqual(order.artifact_type, "planner_order")
        self.assertEqual(order.plan_graph.work_items[0].assignee_agent_id, "executor")


if __name__ == "__main__":
    unittest.main()
