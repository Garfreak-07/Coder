from __future__ import annotations

import tempfile
import time
import unittest

from coder_workbench.core import (
    AgentWorkflowSpec,
    capability_registry,
    default_planner_led_agent_workflow,
    validate_agent_workflow_payload,
)
from coder_workbench.core.artifacts import (
    ArtifactValidationError,
    artifact_summary,
    supported_artifact_types,
    validate_artifact,
)
import coder_workbench.server.app as server_app
from coder_workbench.server.app import create_app
from fastapi.testclient import TestClient


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

    def test_planner_order_accepts_agent_graph_plan_graph(self) -> None:
        artifact = validate_artifact(
            {
                "artifact_type": "planner_order",
                "round": 1,
                "round_goal": "Split the work by Agent.",
                "plan_graph": {
                    "work_items": [
                        {
                            "work_item_id": "frontend-errors",
                            "merge_index": 2,
                            "assignee_agent_id": "frontend",
                            "task_summary": "Show validation errors.",
                            "depends_on": ["backend-validation"],
                            "tester_agent_ids": ["frontend-tester"],
                        },
                        {
                            "work_item_id": "backend-validation",
                            "order_index": 1,
                            "assignee_agent_id": "backend",
                            "task_summary": "Return validation details.",
                            "depends_on": [],
                            "tester_agent_ids": ["backend-tester"],
                        },
                    ],
                    "final_tester_agent_id": "qa",
                },
            }
        )

        self.assertEqual(artifact["plan_graph"]["work_items"][0]["merge_index"], 2)
        self.assertEqual(artifact["plan_graph"]["work_items"][1]["merge_index"], 1)
        self.assertNotIn("order_index", str(artifact))
        self.assertEqual(artifact["instructions_for_executor"], [])

    def test_execution_result_accepts_agent_graph_work_item_fields(self) -> None:
        artifact = validate_artifact(
            {
                "artifact_type": "execution_result",
                "round": 1,
                "work_item_id": "backend-validation",
                "merge_index": 1,
                "agent_id": "backend",
                "status": "completed",
                "summary": "Backend validation now returns field-level errors.",
                "proposed_changes": [
                    {
                        "path": "src/api.py",
                        "action": "update",
                        "patch_ref": "artifact:patch:backend-validation",
                    }
                ],
            }
        )

        self.assertEqual(artifact["work_item_id"], "backend-validation")
        self.assertEqual(artifact["merge_index"], 1)
        self.assertEqual(artifact["agent_id"], "backend")
        self.assertEqual(artifact["proposed_changes"][0]["path"], "src/api.py")

    def test_execution_result_accepts_blocker_protocol(self) -> None:
        artifact = validate_artifact(
            {
                "artifact_type": "execution_result",
                "round": 1,
                "work_item_id": "auth-work",
                "merge_index": 1,
                "agent_id": "backend_agent",
                "status": "blocked",
                "summary": "Auth scope is unclear.",
                "needs_planner_decision": True,
                "blocker_type": "scope_boundary",
                "planner_question": "Can this task modify auth middleware?",
                "continue_without_human_possible": False,
                "candidate_options": [
                    {
                        "option_id": "ask_user",
                        "summary": "Ask user whether auth middleware is in scope.",
                        "risk_level": "high",
                        "requires_human": True,
                    }
                ],
            },
            expected_type="execution_result",
        )

        summary = artifact_summary(artifact)

        self.assertEqual(artifact["blocker_type"], "scope_boundary")
        self.assertEqual(artifact["candidate_options"][0]["option_id"], "ask_user")
        self.assertEqual(summary["blocker_type"], "scope_boundary")
        self.assertTrue(summary["needs_planner_decision"])
        self.assertFalse(summary["continue_without_human_possible"])
        self.assertEqual(summary["candidate_options"], 1)

    def test_execution_result_rejects_unknown_blocker_type(self) -> None:
        with self.assertRaises(ArtifactValidationError):
            validate_artifact(
                {
                    "artifact_type": "execution_result",
                    "status": "blocked",
                    "summary": "Blocked by an unsupported category.",
                    "needs_planner_decision": True,
                    "blocker_type": "unknown_blocker",
                },
                expected_type="execution_result",
            )

    def test_test_result_accepts_agent_graph_tester_fields(self) -> None:
        artifact = validate_artifact(
            {
                "artifact_type": "test_result",
                "round": 1,
                "work_item_id": "frontend-errors",
                "order_index": 2,
                "tester_agent_id": "frontend-tester",
                "status": "pass",
                "summary": "Validation errors render in the form.",
                "evidence": ["artifact:test:frontend-errors"],
            }
        )

        self.assertEqual(artifact["work_item_id"], "frontend-errors")
        self.assertEqual(artifact["merge_index"], 2)
        self.assertNotIn("order_index", str(artifact))
        self.assertEqual(artifact["tester_agent_id"], "frontend-tester")

    def test_round_summary_accepts_agent_graph_ordered_state(self) -> None:
        artifact = validate_artifact(
            {
                "artifact_type": "round_summary",
                "round": 1,
                "planner_order_ref": "artifact:planner-order:round-1",
                "plan_status": "partial_failed",
                "completed_count": 1,
                "failed_count": 1,
                "blocked_count": 0,
                "ordered_state": [
                    {
                        "work_item_id": "backend-validation",
                        "merge_index": 1,
                        "status": "completed",
                        "summary": "Backend validation completed.",
                        "refs": ["artifact:execution:backend-validation"],
                    },
                    {
                        "work_item_id": "frontend-errors",
                        "merge_index": 2,
                        "status": "failed_test",
                        "summary": "Frontend test still fails.",
                        "refs": ["artifact:test:frontend-errors"],
                    },
                ],
                "remaining_work": ["Fix frontend validation rendering."],
            }
        )

        self.assertEqual(artifact["plan_status"], "partial_failed")
        self.assertEqual(artifact["planner_order_summary"], "")
        self.assertEqual([item["work_item_id"] for item in artifact["ordered_state"]], ["backend-validation", "frontend-errors"])
        self.assertEqual(artifact["remaining_work"], ["Fix frontend validation rendering."])


class AgentWorkflowContractTests(unittest.TestCase):
    def test_capability_registry_contains_initial_user_choices(self) -> None:
        registry = capability_registry()

        self.assertIn("modify_files", registry)
        self.assertEqual(registry["modify_files"].produces, ["execution_result"])
        self.assertIn("planner", registry["make_next_decision"].allowed_roles)
        self.assertTrue(registry["make_next_decision"].can_talk_to_human)

    def test_valid_workflow_can_have_more_than_three_agents(self) -> None:
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
        self.assertEqual(validation.status, "pass")

    def test_validation_reports_missing_primary_planner(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload.pop("primary_planner_id")

        validation = validate_agent_workflow_payload(payload)

        self.assertEqual(validation.status, "error")
        self.assertIn("missing_primary_planner", {issue.code for issue in validation.issues})

    def test_validation_rejects_non_primary_human_access(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload["agents"][1]["can_talk_to_human"] = True

        validation = validate_agent_workflow_payload(payload)

        self.assertEqual(validation.status, "error")
        self.assertIn("non_primary_agent_can_talk_to_human", {issue.code for issue in validation.issues})

    def test_validation_rejects_duplicate_agent_ids(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload["agents"][1]["id"] = "tester"

        validation = validate_agent_workflow_payload(payload)

        self.assertEqual(validation.status, "error")
        self.assertIn("duplicate_agent_id", {issue.code for issue in validation.issues})

    def test_validation_rejects_unknown_capability(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload["agents"][1]["capabilities"].append("invent_runtime")

        validation = validate_agent_workflow_payload(payload)

        self.assertEqual(validation.status, "error")
        self.assertIn("unknown_capability", {issue.code for issue in validation.issues})

    def test_validation_rejects_capability_on_wrong_role(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload["agents"][1]["capabilities"] = ["make_next_decision"]

        validation = validate_agent_workflow_payload(payload)

        self.assertEqual(validation.status, "error")
        self.assertIn("capability_role_not_allowed", {issue.code for issue in validation.issues})

    def test_validation_rejects_missing_upstream_artifact(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload["edges"] = [
            {"from": "planner", "to": "tester"},
            {"from": "tester", "to": "planner", "loop": True},
        ]

        validation = validate_agent_workflow_payload(payload)

        self.assertEqual(validation.status, "error")
        self.assertIn("unsatisfied_capability_input", {issue.code for issue in validation.issues})


class AgentWorkflowApiTests(unittest.TestCase):
    def test_default_agent_workflow_api_returns_agent_contract_only(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))

            response = client.get("/api/v2/agent-workflows/default")

            self.assertEqual(response.status_code, 200)
            payload = response.json()
            self.assertEqual(payload["agent_workflow"]["id"], "default-planner-led")
            self.assertNotIn("workflow", payload)
            self.assertNotIn("runtime_boundary", payload)
            self.assertNotIn("runtime_type", payload)
            self.assertNotIn("deprecated", payload)

    def test_agent_workflow_compile_api_is_quarantined(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            agent_workflow = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)

            response = client.post("/api/v2/agent-workflows/compile", json=agent_workflow)

            self.assertEqual(response.status_code, 410)
            self.assertTrue(response.json()["detail"]["removed"])

    def test_agent_workflow_library_saves_agent_contract(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            agent_workflow = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)

            response = client.post("/api/v2/library/agent-workflows", json=agent_workflow)

            self.assertEqual(response.status_code, 200)
            self.assertEqual(response.json()["agent_workflow"]["id"], "default-planner-led")

            index = client.get("/api/v2/library").json()
            self.assertEqual(index["agent_workflows"][0]["id"], "default-planner-led")
            self.assertEqual(index["agent_workflows"][0]["agents"], 3)

            loaded = client.get("/api/v2/library/agent-workflows/default-planner-led").json()
            self.assertNotIn("handoff", loaded["agent_workflow"]["edges"][0])

    def test_agent_workflow_validate_api_reports_deterministic_errors(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            agent_workflow = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
            agent_workflow["agents"][1]["capabilities"].append("invent_runtime")

            response = client.post("/api/v2/agent-workflows/validate", json=agent_workflow)

            self.assertEqual(response.status_code, 200)
            payload = response.json()
            self.assertEqual(payload["status"], "error")
            self.assertIn("unknown_capability", {issue["code"] for issue in payload["issues"]})

    def test_agent_workflow_library_blocks_invalid_save(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            agent_workflow = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
            agent_workflow.pop("primary_planner_id")

            response = client.post("/api/v2/library/agent-workflows", json=agent_workflow)

            self.assertEqual(response.status_code, 400)
            detail = response.json()["detail"]
            self.assertIn("missing_primary_planner", {issue["code"] for issue in detail["issues"]})

    def test_live_agent_run_uses_agent_graph_runner_without_legacy_compile(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            agent_workflow = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)

            self.assertFalse(hasattr(server_app, "compile_agent_workflow_legacy_preview"))
            response = client.post(
                "/api/v2/live-agent-runs",
                json={
                    "repo": tmp,
                    "request": "Run the default workflow.",
                    "agent_workflow": agent_workflow,
                    "approved": True,
                    "scopes": [],
                },
            )

            self.assertEqual(response.status_code, 200)
            run_id = response.json()["run_id"]
            final_status = response.json()["status"]
            detail = client.get(f"/api/v2/live-agent-runs/{run_id}").json()
            for _ in range(50):
                if final_status not in {"queued", "running"}:
                    break
                time.sleep(0.02)
                detail = client.get(f"/api/v2/live-agent-runs/{run_id}").json()
                final_status = detail["status"]
            legacy = client.get(f"/api/v2/live-runs/{run_id}")
            self.assertEqual(final_status, "completed")
            self.assertEqual(detail["runtime_type"], "agent_graph")
            self.assertIn("agent_graph.run.completed", {event["type"] for event in detail["events"]})
            self.assertNotIn("compiled_workflow", detail["result"]["data"])
            self.assertEqual(legacy.status_code, 410)
            self.assertIn("/api/v2/live-agent-runs/", legacy.json()["detail"]["result_url"])

if __name__ == "__main__":
    unittest.main()
