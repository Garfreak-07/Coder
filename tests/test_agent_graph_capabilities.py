from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from coder_workbench.agent_graph import PlannerMemoryStore, skill_modules_for_authority
from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.core import (
    AgentWorkflowSpec,
    authority_profile_for_agent,
    default_planner_led_agent_workflow,
    validate_agent_workflow,
)


class AgentGraphCapabilityBoundaryTests(unittest.TestCase):
    def test_authority_profile_matches_default_agents(self) -> None:
        workflow = default_planner_led_agent_workflow()
        profiles = {
            agent.id: authority_profile_for_agent(agent, primary_planner_id=workflow.primary_planner_id)
            for agent in workflow.agents
        }

        self.assertEqual(profiles["planner"].authority, "planner")
        self.assertIn("planner_decision", profiles["planner"].allowed_artifact_types)
        self.assertEqual(profiles["executor"].authority, "worker")
        self.assertTrue(profiles["executor"].can_trigger_interrupt)
        self.assertEqual(profiles["tester"].authority, "tester")
        self.assertIn("test_result", profiles["tester"].allowed_artifact_types)

    def test_authority_validation_rejects_wrong_artifact_owner(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload["agents"][0]["capabilities"].append("return_execution_result")

        result = validate_agent_workflow(AgentWorkflowSpec.model_validate(payload))

        self.assertEqual(result.status, "error")
        self.assertIn("authority_artifact_not_allowed", {issue.code for issue in result.issues})

    def test_planner_memory_records_round_summary(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow()).run("Record memory.", tmp)
            memory_path = Path(tmp) / ".coder" / "memory" / "workflows" / "default-planner-led.json"

            memory = PlannerMemoryStore(tmp).load_workflow_memory("default-planner-led")
            memory_exists = memory_path.exists()

        self.assertEqual(result.status, "completed")
        self.assertTrue(memory_exists)
        self.assertGreaterEqual(len(memory.planner_notes), 1)
        self.assertGreaterEqual(len(memory.successful_assignments), 1)
        self.assertEqual(memory.workflow_id, "default-planner-led")

    def test_skill_modules_define_agent_boundaries(self) -> None:
        planner_modules = {module.id for module in skill_modules_for_authority("planner")}
        worker_modules = {module.id for module in skill_modules_for_authority("worker")}
        tester_modules = {module.id for module in skill_modules_for_authority("tester")}
        final_tester_modules = {module.id for module in skill_modules_for_authority("final_tester")}

        self.assertIn("replanning", planner_modules)
        self.assertIn("human_escalation", planner_modules)
        self.assertIn("blocker_reporting", worker_modules)
        self.assertIn("confidence_calibration", tester_modules)
        self.assertIn("round_evidence_compression", final_tester_modules)


if __name__ == "__main__":
    unittest.main()
