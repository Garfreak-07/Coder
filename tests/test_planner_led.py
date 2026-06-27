from __future__ import annotations

import tempfile
import unittest

from fastapi.testclient import TestClient

from coder_workbench.core import AgentWorkflowSpec, default_planner_led_agent_workflow, validate_agent_workflow_payload
from coder_workbench.core.artifacts import supported_artifact_types, validate_artifact
from coder_workbench.server.app import create_app


class PlannerLedArtifactTests(unittest.TestCase):
    def test_new_artifact_protocol_is_supported_without_test_result(self) -> None:
        self.assertEqual(
            set(supported_artifact_types()),
            {
                "project_plan_draft",
                "run_contract_draft",
                "run_contract",
                "planner_order",
                "execution_result",
                "planner_decision",
                "planner_chat_turn",
                "workflow_activity_update",
                "round_summary",
                "final_report",
                "planner_memory_write_proposal",
            },
        )

    def test_execution_result_contains_verification(self) -> None:
        artifact = validate_artifact(
            {
                "artifact_type": "execution_result",
                "round": 1,
                "work_item_id": "work",
                "merge_index": 1,
                "agent_id": "executor",
                "status": "completed",
                "summary": "Done.",
                "outputs": ["execution_result_work"],
                "verification": {
                    "status": "pass",
                    "checks_run": [
                        {
                            "check_id": "static",
                            "kind": "static",
                            "command": None,
                            "status": "pass",
                            "summary": "Static evidence passed.",
                            "output_ref": None,
                            "evidence_refs": ["execution_result_work"],
                        }
                    ],
                    "evidence_refs": ["execution_result_work"],
                    "confidence": "medium",
                    "remaining_work": [],
                    "no_check_rationale": None,
                    "repair_attempted": False,
                    "repair_summary": None,
                },
            },
            expected_type="execution_result",
        )

        self.assertEqual(artifact["verification"]["status"], "pass")

    def test_execution_result_rejects_failed_status(self) -> None:
        with self.assertRaises(Exception):
            validate_artifact(
                {
                    "artifact_type": "execution_result",
                    "status": "failed",
                    "summary": "Nope.",
                    "verification": {
                        "status": "blocked",
                        "checks_run": [],
                        "evidence_refs": [],
                        "confidence": "low",
                        "remaining_work": ["Nope."],
                        "no_check_rationale": None,
                        "repair_attempted": False,
                        "repair_summary": None,
                    },
                },
                expected_type="execution_result",
            )

    def test_blocked_execution_result_requires_structured_blocked_contract(self) -> None:
        with self.assertRaises(Exception):
            validate_artifact(
                {
                    "artifact_type": "execution_result",
                    "round": 1,
                    "work_item_id": "work",
                    "merge_index": 1,
                    "agent_id": "executor",
                    "status": "blocked",
                    "summary": "Missing dependency.",
                    "remaining_work": ["Install dependency."],
                    "blocker_type": "dependency_missing",
                    "verification": {
                        "status": "blocked",
                        "checks_run": [],
                        "evidence_refs": [],
                        "confidence": "low",
                        "remaining_work": ["Install dependency."],
                    },
                },
                expected_type="execution_result",
            )

    def test_legacy_blocker_type_maps_to_new_taxonomy(self) -> None:
        artifact = validate_artifact(
            {
                "artifact_type": "execution_result",
                "round": 1,
                "work_item_id": "work",
                "merge_index": 1,
                "agent_id": "executor",
                "status": "blocked",
                "summary": "Missing dependency.",
                "remaining_work": ["Install dependency."],
                "needs_planner_decision": True,
                "blocker_type": "dependency_missing",
                "executor_recovery_exhausted": True,
                "blocker_reason": "Missing dependency.",
                "planner_recommendation": "finish",
                "constraint_boundary": {"within_scope": True},
                "verification": {
                    "status": "blocked",
                    "checks_run": [],
                    "evidence_refs": [],
                    "confidence": "low",
                    "remaining_work": ["Install dependency."],
                },
            },
            expected_type="execution_result",
        )

        self.assertEqual(artifact["blocker_type"], "missing_dependency")


class AgentWorkflowContractTests(unittest.TestCase):
    def test_default_workflow_is_planner_executor_loop(self) -> None:
        workflow = default_planner_led_agent_workflow()

        self.assertEqual(workflow.version, "0.5")
        self.assertEqual(workflow.harness_bindings.planning_chat.profile_id, "openhands-planning-chat-default")
        self.assertEqual(workflow.harness_bindings.workflow_supervisor.profile_id, "openhands-workflow-supervisor-default")
        self.assertEqual(workflow.harness_bindings.task_execution.profile_id, "openhands-task-executor-default")
        self.assertEqual([agent.role for agent in workflow.agents], ["planner", "executor"])
        self.assertEqual([(edge.from_agent, edge.to_agent, edge.loop) for edge in workflow.edges], [("planner", "executor", False), ("executor", "planner", True)])

    def test_legacy_workflow_payload_migrates_to_harness_bindings(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload["version"] = "0.4"
        payload.pop("harness_bindings", None)
        payload["agents"][1].pop("skill_pack_ids", None)

        workflow = AgentWorkflowSpec.model_validate(payload)

        self.assertEqual(workflow.version, "0.5")
        self.assertEqual(workflow.agents[1].skill_pack_ids, [])
        self.assertEqual(workflow.harness_bindings.task_execution.profile_id, "openhands-task-executor-default")

    def test_legacy_tester_workflow_payload_is_rejected(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload["agents"].append(
            {
                "id": "legacy",
                "name": "Legacy",
                "role": "tester",
                "model_tier": "standard",
                "can_talk_to_human": False,
                "capabilities": ["model_review", "return_test_result"],
            }
        )

        result = validate_agent_workflow_payload(payload)

        self.assertEqual(result.status, "error")


class AgentWorkflowApiTests(unittest.TestCase):
    def test_agent_workflow_library_saves_agent_contract(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
            response = client.post("/api/v2/library/agent-workflows", json=payload)
            index = client.get("/api/v2/library").json()

        self.assertEqual(response.status_code, 200)
        self.assertEqual(index["agent_workflows"][0]["agents"], 2)

    def test_live_agent_run_uses_agent_graph_runner(self) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as store, tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as repo:
            client = TestClient(create_app(store_root=store, frontend_dist=store))
            response = client.post(
                "/api/v2/live-agent-runs",
                json={
                    "request": "Run default workflow.",
                    "repo": repo,
                    "agent_workflow": default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True),
                    "approved": False,
                    "scopes": [],
                },
            )

        self.assertEqual(response.status_code, 200)
        self.assertIn(response.json()["status"], {"running", "completed"})
        self.assertTrue(response.json()["run_id"])


if __name__ == "__main__":
    unittest.main()
