from __future__ import annotations

import inspect
import re
import tempfile
import unittest
from pathlib import Path

from fastapi.testclient import TestClient

import coder_workbench.server.app as server_app
from coder_workbench.server.app import create_app


ROOT = Path(__file__).resolve().parents[1]


class LegacyDeletionGateTests(unittest.TestCase):
    def test_product_server_does_not_import_legacy_runner(self) -> None:
        source = inspect.getsource(server_app)

        self.assertNotIn("from coder_workbench.runtime import run_workflow", source)
        self.assertNotIn("from coder_workbench.runtime.runner import WorkflowRunner", source)
        self.assertNotIn("from coder_workbench.server.manager import RunManager", source)
        self.assertIsNone(re.search(r"(?<!AgentGraph)RunManager\(", source))

    def test_legacy_execution_endpoints_are_removed_or_gone(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))

            responses = {
                "create_stored_run": client.post("/api/v2/runs", json={}),
                "create_live_run": client.post("/api/v2/live-runs", json={}),
                "validate_workflow": client.post("/api/v2/workflows/validate", json={}),
            }

        for name, response in responses.items():
            with self.subTest(endpoint=name):
                self.assertIn(response.status_code, {404, 405, 410})

    def test_stored_agentgraph_run_read_endpoints_remain(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))

            list_response = client.get("/api/v2/runs")
            detail_response = client.get("/api/v2/runs/missing")
            delete_response = client.delete("/api/v2/runs/missing")
            events_response = client.get("/api/v2/runs/missing/events")
            context_response = client.get("/api/v2/runs/missing/context-packets/missing")
            artifact_response = client.get("/api/v2/runs/missing/artifacts/missing")
            tool_result_response = client.get("/api/v2/runs/missing/tool-results/missing")
            blob_response = client.get("/api/v2/runs/missing/blobs/missing")

        self.assertEqual(list_response.status_code, 200)
        for name, response in {
            "detail": detail_response,
            "delete": delete_response,
            "events": events_response,
            "context": context_response,
            "artifact": artifact_response,
            "tool_result": tool_result_response,
            "blob": blob_response,
        }.items():
            with self.subTest(endpoint=name):
                self.assertEqual(response.status_code, 404)

    def test_legacy_workflow_library_paths_are_removed(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))

            index_response = client.get("/api/v2/library")
            save_response = client.post("/api/v2/library/workflows", json={})
            get_response = client.get("/api/v2/library/workflows/example")

        self.assertEqual(index_response.status_code, 200)
        self.assertNotIn("workflows", index_response.json())
        self.assertIn(save_response.status_code, {404, 405, 410})
        self.assertIn(get_response.status_code, {404, 405, 410})

    def test_frontend_does_not_expose_legacy_runtime_editor_or_api_wrappers(self) -> None:
        api_source = (ROOT / "frontend" / "src" / "api.ts").read_text(encoding="utf-8")
        app_source = (ROOT / "frontend" / "src" / "App.tsx").read_text(encoding="utf-8")

        for token in [
            "compileLegacyRuntimePreview",
            "getWorkflow",
            "saveWorkflow",
            "validateWorkflow",
            "startLiveRun",
            "approveLiveRun",
            "retryCurrentNode",
            "getLiveRun",
            "/api/v2/live-runs",
            "/api/v2/workflows/validate",
            "/api/v2/library/workflows",
        ]:
            with self.subTest(token=token):
                self.assertNotIn(token, api_source)

        for token in [
            "runtimeJsonText",
            "showAdvancedRuntime",
            "runtimePreviewDirty",
            "compileLegacyRuntimePreview",
            "PreflightModal",
            "function NodeInspector",
            "function EdgeInspector",
            "function AgentInspector",
            "Legacy Runtime Inspector",
            "Legacy Runtime Agents",
            "/api/v2/live-runs",
        ]:
            with self.subTest(token=token):
                self.assertNotIn(token, app_source)


if __name__ == "__main__":
    unittest.main()
