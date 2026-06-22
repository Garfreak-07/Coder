from __future__ import annotations

import inspect
import re
import tempfile
import time
import unittest
from pathlib import Path

from fastapi.testclient import TestClient

import coder_workbench.server.app as server_app
from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.server.app import create_app


ROOT = Path(__file__).resolve().parents[1]


FORBIDDEN_LEGACY_IMPORTS = [
    "from coder_workbench.core.legacy_compile import",
    "import coder_workbench.core.legacy_compile",
    "from coder_workbench.core.schema import WorkflowSpec",
    "from coder_workbench.runtime import run_workflow",
    "from coder_workbench.runtime.runner import WorkflowRunner",
]


class LegacyQuarantineGateTests(unittest.TestCase):
    def test_default_agent_workflow_api_no_longer_returns_legacy_runtime_preview(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            response = client.get("/api/v2/agent-workflows/default")

        self.assertEqual(response.status_code, 200)
        payload = response.json()
        self.assertEqual(payload["agent_workflow"]["id"], "default-planner-led")
        for key in ["workflow", "runtime_boundary", "runtime_type", "deprecated"]:
            with self.subTest(key=key):
                self.assertNotIn(key, payload)

    def test_agent_workflow_compile_preview_is_quarantined(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            workflow = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
            response = client.post("/api/v2/agent-workflows/compile", json=workflow)

        self.assertEqual(response.status_code, 410)
        detail = response.json()["detail"]
        self.assertTrue(detail["removed"])
        self.assertEqual(detail["replacement"], "/api/v2/agent-workflows/validate")

    def test_product_server_app_does_not_import_legacy_preview_compiler(self) -> None:
        source = inspect.getsource(server_app)

        self.assertNotIn("compile_agent_workflow_legacy_preview", source)
        self.assertNotIn("LEGACY_RUNTIME_PREVIEW_BOUNDARY", source)
        self.assertNotIn("coder_workbench.core.legacy_compile", source)

    def test_product_runtime_modules_do_not_import_legacy_runtime(self) -> None:
        product_paths = [
            ROOT / "src" / "coder_workbench" / "server" / "app.py",
            ROOT / "src" / "coder_workbench" / "server" / "agent_manager.py",
            ROOT / "src" / "coder_workbench" / "core" / "__init__.py",
        ]
        product_paths.extend((ROOT / "src" / "coder_workbench" / "agent_graph").glob("*.py"))

        for path in product_paths:
            source = path.read_text(encoding="utf-8")
            for token in FORBIDDEN_LEGACY_IMPORTS:
                with self.subTest(path=path.relative_to(ROOT), token=token):
                    self.assertNotIn(token, source)

    def test_frontend_product_sources_do_not_import_legacy_workflow_spec(self) -> None:
        frontend_paths = [
            path
            for path in (ROOT / "frontend" / "src").glob("*.ts*")
            if path.name != "types.ts"
        ]

        for path in frontend_paths:
            source = path.read_text(encoding="utf-8")
            with self.subTest(path=path.relative_to(ROOT)):
                self.assertIsNone(re.search(r"(?<!Agent)\bWorkflowSpec\b", source))
                self.assertNotIn("NodeSpec", source)
                self.assertNotIn("EdgeSpec", source)

    def test_root_product_tests_do_not_import_legacy_runtime(self) -> None:
        for path in (ROOT / "tests").glob("test_*.py"):
            if path.name.startswith("test_legacy_"):
                continue
            source = path.read_text(encoding="utf-8")
            for token in FORBIDDEN_LEGACY_IMPORTS:
                with self.subTest(path=path.name, token=token):
                    self.assertNotIn(token, source)

    def test_agentgraph_live_and_stored_run_read_paths_remain(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))

            list_response = client.get("/api/v2/runs")
            live_response = client.post(
                "/api/v2/live-agent-runs",
                json={
                    "repo": tmp,
                    "request": "Run product AgentGraph only.",
                    "agent_workflow": default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True),
                    "approved": True,
                },
            )
            run_id = live_response.json()["run_id"]
            detail = client.get(f"/api/v2/live-agent-runs/{run_id}").json()
            for _ in range(50):
                if detail["status"] not in {"queued", "running"}:
                    break
                time.sleep(0.02)
                detail = client.get(f"/api/v2/live-agent-runs/{run_id}").json()
            stored_run_id = detail.get("stored_run_id")
            stored_response = client.get(f"/api/v2/runs/{stored_run_id}") if stored_run_id else None
            events_response = (
                client.get(f"/api/v2/runs/{stored_run_id}/events", params={"cursor": 0})
                if stored_run_id
                else None
            )
            artifact_response = None
            if stored_response is not None and stored_response.status_code == 200:
                artifacts = stored_response.json()["result"].get("artifacts", {})
                if artifacts:
                    artifact_id = next(iter(artifacts))
                    artifact_response = client.get(f"/api/v2/runs/{stored_run_id}/artifacts/{artifact_id}")

        self.assertEqual(list_response.status_code, 200)
        self.assertEqual(live_response.status_code, 200)
        self.assertIn("/api/v2/live-agent-runs/", live_response.json()["result_url"])
        self.assertEqual(detail["status"], "completed")
        self.assertIsNotNone(stored_run_id)
        self.assertIsNotNone(stored_response)
        self.assertIsNotNone(events_response)
        self.assertIsNotNone(artifact_response)
        self.assertEqual(stored_response.status_code, 200)
        self.assertEqual(events_response.status_code, 200)
        self.assertEqual(artifact_response.status_code, 200)


if __name__ == "__main__":
    unittest.main()
