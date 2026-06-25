from __future__ import annotations

import tempfile
import unittest

from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.core import default_planner_led_agent_workflow


class AgentGraphNoHumanPromptTests(unittest.TestCase):
    def test_normal_run_does_not_emit_planner_human_prompt(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow()).run(
                "Complete a normal planner-led run.",
                tmp,
            )

        self.assertEqual(result.status, "completed")
        self.assertNotIn("planner.human_prompt", {event.type for event in result.events})
        self.assertNotEqual(result.status_code, "planner_ask_human")
        self.assertNotIn("planner_human_prompt", result.data)
        self.assertEqual(result.data["final_report"]["status"], "completed")


if __name__ == "__main__":
    unittest.main()
