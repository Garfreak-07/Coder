from __future__ import annotations

import tempfile
import time
import unittest
from unittest.mock import patch

from fastapi.testclient import TestClient

from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.server.app import LEGACY_RUNTIME_PREVIEW_BOUNDARY, create_app


class LegacyIsolationGateTests(unittest.TestCase):
    def test_live_agent_run_does_not_compile_or_run_legacy_runtime(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with (
                patch(
                    "coder_workbench.server.app.compile_agent_workflow_legacy_preview",
                    side_effect=AssertionError("legacy compiler called"),
                ),
                patch("coder_workbench.server.app.WorkflowRunner", side_effect=AssertionError("legacy runner called")),
            ):
                client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
                response = client.post(
                    "/api/v2/live-agent-runs",
                    json={
                        "repo": tmp,
                        "request": "Run product AgentGraph only.",
                        "agent_workflow": default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True),
                        "approved": True,
                    },
                )
                self.assertEqual(response.status_code, 200)
                detail = _wait_for_agent_run(client, response.json()["run_id"])

        self.assertEqual(detail["runtime_type"], "agent_graph")
        self.assertIn(detail["status"], {"completed", "blocked"})

    def test_legacy_preview_endpoints_are_explicitly_deprecated(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            workflow = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
            default_response = client.get("/api/v2/agent-workflows/default")
            compile_response = client.post("/api/v2/agent-workflows/compile", json=workflow)

        for response in [default_response, compile_response]:
            with self.subTest(endpoint=response.request.url.path):
                self.assertEqual(response.status_code, 200)
                payload = response.json()
                self.assertTrue(payload["deprecated"])
                self.assertEqual(payload["runtime_boundary"], LEGACY_RUNTIME_PREVIEW_BOUNDARY)
                self.assertEqual(payload["runtime_type"], "legacy_preview")
                self.assertIn("workflow", payload)

    def test_agentgraph_product_artifacts_exclude_legacy_artifact_types(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow()).run("Check product artifacts.", tmp)

        artifact_types = {
            artifact.get("artifact_type")
            for artifact in result.artifacts.values()
            if isinstance(artifact, dict)
        }

        self.assertEqual(result.status, "completed")
        self.assertFalse({"plan_artifact", "patch_artifact", "review_artifact"}.intersection(artifact_types))
        self.assertIn("planner_order", artifact_types)
        self.assertIn("planner_input_bundle", artifact_types)
        self.assertIn("planner_decision", artifact_types)

    def test_legacy_live_run_endpoint_stays_deprecated_legacy_workflow(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            list_response = client.get("/api/v2/live-runs")

        self.assertEqual(list_response.status_code, 200)
        self.assertTrue(list_response.json()["deprecated"])


def _wait_for_agent_run(client: TestClient, run_id: str) -> dict:
    detail = client.get(f"/api/v2/live-agent-runs/{run_id}").json()
    for _ in range(50):
        if detail["status"] not in {"queued", "running"}:
            return detail
        time.sleep(0.02)
        detail = client.get(f"/api/v2/live-agent-runs/{run_id}").json()
    return detail


if __name__ == "__main__":
    unittest.main()
