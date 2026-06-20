from __future__ import annotations

import tempfile
import unittest

from coder_workbench.core import compile_agent_workflow, default_planner_led_agent_workflow
from coder_workbench.core.artifacts import supported_artifact_types, validate_artifact
from coder_workbench.runtime.runner import WorkflowRunner


class PlannerLedArtifactTests(unittest.TestCase):
    def test_new_artifact_protocol_is_supported(self) -> None:
        self.assertTrue(
            {
                "run_contract",
                "planner_order",
                "execution_result",
                "test_result",
                "planner_decision",
                "round_summary",
            }.issubset(set(supported_artifact_types()))
        )

    def test_run_contract_validation_normalizes_defaults(self) -> None:
        artifact = validate_artifact(
            {
                "artifact_type": "run_contract",
                "user_goal": "Implement the default Planner-led loop.",
            }
        )

        self.assertEqual(artifact["artifact_type"], "run_contract")
        self.assertEqual(artifact["loop_policy"]["max_auto_rounds"], 3)
        self.assertTrue(artifact["risk_policy"]["planner_is_risk_judge"])
        self.assertTrue(artifact["execution_policy"]["executor_cannot_ask_human"])
        self.assertTrue(artifact["test_policy"]["tester_cannot_ask_human"])

    def test_planner_decision_only_allows_known_next_actions(self) -> None:
        artifact = validate_artifact(
            {
                "artifact_type": "planner_decision",
                "round": 1,
                "task_done": True,
                "next_action": "finish",
                "reason": "All required artifacts are present.",
            }
        )

        self.assertEqual(artifact["next_action"], "finish")


class AgentWorkflowCompilerTests(unittest.TestCase):
    def test_default_agent_workflow_compiles_to_hidden_runtime_graph(self) -> None:
        agent_workflow = default_planner_led_agent_workflow()
        workflow = compile_agent_workflow(agent_workflow)

        self.assertEqual(agent_workflow.name, workflow.name)
        self.assertEqual([agent.role for agent in agent_workflow.agents], ["planner", "executor", "tester"])
        self.assertEqual(workflow.max_tool_calls, 0)
        self.assertIn("planner_loop", {node.id for node in workflow.nodes})
        self.assertEqual(
            [agent.artifact_type for agent in workflow.agents],
            [
                "run_contract",
                "planner_order",
                "execution_result",
                "test_result",
                "planner_decision",
                "round_summary",
            ],
        )

    def test_default_planner_led_workflow_runs_in_mock_mode(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workflow = compile_agent_workflow(default_planner_led_agent_workflow())

            result = WorkflowRunner(workflow).run("Build the smallest Planner-led loop.", tmp)

            self.assertEqual(result.status, "completed")
            produced_types = [
                event.payload["artifact_type"]
                for event in result.events
                if event.type == "artifact.produced"
            ]
            self.assertEqual(
                produced_types,
                [
                    "run_contract",
                    "planner_order",
                    "execution_result",
                    "test_result",
                    "planner_decision",
                    "round_summary",
                ],
            )
            self.assertEqual(result.data["planner_decision"]["next_action"], "finish")


if __name__ == "__main__":
    unittest.main()
