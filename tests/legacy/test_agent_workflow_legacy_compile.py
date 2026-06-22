from __future__ import annotations

import tempfile
import unittest

from coder_workbench.core import AgentWorkflowSpec, default_planner_led_agent_workflow, validate_agent_workflow_payload
from coder_workbench.core.legacy_compile import compile_agent_workflow
from coder_workbench.runtime.runner import WorkflowRunner


class AgentWorkflowLegacyCompilerTests(unittest.TestCase):
    def test_default_agent_workflow_compiles_to_hidden_runtime_graph(self) -> None:
        agent_workflow = default_planner_led_agent_workflow()
        workflow = compile_agent_workflow(agent_workflow)

        self.assertEqual(agent_workflow.name, workflow.name)
        self.assertEqual(agent_workflow.version, "0.4")
        self.assertEqual(agent_workflow.primary_planner_id, "planner")
        self.assertEqual([agent.role for agent in agent_workflow.agents], ["planner", "executor", "tester"])
        self.assertIsNone(agent_workflow.edges[0].handoff)
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

    def test_valid_workflow_with_more_than_three_agents_compiles_for_legacy_compatibility(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload["agents"].append(
            {
                "id": "reviewer",
                "name": "Reviewer Agent",
                "role": "reviewer",
                "model_tier": "standard",
                "can_talk_to_human": False,
                "capabilities": ["model_review", "aggregate_tests", "return_test_result"],
            }
        )
        payload["edges"].extend(
            [
                {"from": "tester", "to": "reviewer"},
                {"from": "reviewer", "to": "planner", "loop": True},
            ]
        )

        validation = validate_agent_workflow_payload(payload)
        workflow = compile_agent_workflow(AgentWorkflowSpec.model_validate(payload))

        self.assertEqual(validation.status, "pass")
        self.assertIn("agent_reviewer", {node.id for node in workflow.nodes})

    def test_default_planner_led_workflow_runs_in_legacy_mock_mode(self) -> None:
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
