from __future__ import annotations

import tempfile
import time
import unittest
from unittest.mock import patch

from coder_workbench.core import (
    AgentWorkflowSpec,
    capability_registry,
    compile_agent_workflow,
    default_planner_led_agent_workflow,
    validate_agent_workflow_payload,
)
from coder_workbench.core.artifacts import supported_artifact_types, validate_artifact
from coder_workbench.runtime.runner import WorkflowRunner
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


class AgentWorkflowCompilerTests(unittest.TestCase):
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

        workflow = compile_agent_workflow(AgentWorkflowSpec.model_validate(payload))
        self.assertIn("agent_reviewer", {node.id for node in workflow.nodes})

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


class AgentWorkflowApiTests(unittest.TestCase):
    def test_default_agent_workflow_api_returns_compiled_runtime(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))

            response = client.get("/api/v2/agent-workflows/default")

            self.assertEqual(response.status_code, 200)
            payload = response.json()
            self.assertEqual(payload["agent_workflow"]["id"], "default-planner-led")
            self.assertEqual(payload["workflow"]["max_tool_calls"], 0)
            self.assertIn("planner_loop", {node["id"] for node in payload["workflow"]["nodes"]})

    def test_agent_workflow_compile_api_accepts_agent_contract(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            agent_workflow = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)

            response = client.post("/api/v2/agent-workflows/compile", json=agent_workflow)

            self.assertEqual(response.status_code, 200)
            payload = response.json()
            self.assertEqual(payload["workflow"]["id"], "default-planner-led-runtime")
            self.assertEqual(
                [agent["artifact_type"] for agent in payload["workflow"]["agents"]],
                [
                    "run_contract",
                    "planner_order",
                    "execution_result",
                    "test_result",
                    "planner_decision",
                    "round_summary",
                ],
            )

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

            with patch("coder_workbench.server.app.compile_agent_workflow", side_effect=AssertionError("legacy compile called")):
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
            detail = client.get(f"/api/v2/live-runs/{run_id}").json()
            for _ in range(50):
                if final_status not in {"queued", "running"}:
                    break
                time.sleep(0.02)
                detail = client.get(f"/api/v2/live-runs/{run_id}").json()
                final_status = detail["status"]
            self.assertEqual(final_status, "completed")
            self.assertEqual(detail["runtime_type"], "agent_graph")
            self.assertIn("agent_graph.run.completed", {event["type"] for event in detail["events"]})
            self.assertNotIn("compiled_workflow", detail["result"]["data"])

if __name__ == "__main__":
    unittest.main()
