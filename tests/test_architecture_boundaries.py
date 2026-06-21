from __future__ import annotations

import inspect
import tempfile
import time
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from fastapi.testclient import TestClient

from coder_workbench.agent_engine import CodeWorkerEngine, HarnessBlock, HarnessGraph, HarnessValidator
from coder_workbench.agent_engine.schema import AgentEngineSpec
from coder_workbench.agent_graph.executor import AgentGraphExecutor
from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_model import AgentRecipe, RuntimeProfileCompiler, TokenBudget
from coder_workbench.core import AgentWorkflowSpec, default_planner_led_agent_workflow, validate_agent_workflow_payload
from coder_workbench.server.app import create_app


class ArchitectureBoundaryTests(unittest.TestCase):
    def test_live_agent_runs_do_not_use_workflow_runner(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
            with patch("coder_workbench.server.app.WorkflowRunner", side_effect=AssertionError("legacy runner called")):
                response = client.post(
                    "/api/v2/live-agent-runs",
                    json={
                        "repo": tmp,
                        "request": "Run the default AgentGraph path.",
                        "agent_workflow": payload,
                        "approved": True,
                    },
                )
                if response.status_code == 200:
                    _wait_for_live_run(client, response.json()["run_id"])

        self.assertEqual(response.status_code, 200)
        self.assertIn(response.json()["status"], {"queued", "running", "completed"})

    def test_agent_graph_runner_does_not_import_default_agent_executor(self) -> None:
        source = inspect.getsource(__import__("coder_workbench.agent_graph.runner", fromlist=["_"]))

        self.assertNotIn("DefaultAgentExecutor", source)
        self.assertIn("AgentRun", source)

    def test_agent_graph_low_level_services_route_through_action_gateway(self) -> None:
        runner_source = inspect.getsource(__import__("coder_workbench.agent_graph.runner", fromlist=["_"]))
        effects_source = inspect.getsource(__import__("coder_workbench.agent_graph.effects", fromlist=["_"]))

        self.assertNotIn("ContextService", runner_source)
        self.assertIn("ActionGateway", runner_source)
        self.assertNotIn("PatchService", effects_source)
        self.assertNotIn("CommandService", effects_source)
        self.assertIn("ActionGateway", effects_source)

    def test_agent_run_uses_runtime_profile_cache(self) -> None:
        source = inspect.getsource(__import__("coder_workbench.agent_graph.agent_run", fromlist=["_"]))

        self.assertIn("RuntimeProfileCache", source)
        self.assertIn("compile_or_get", source)

    def test_code_work_item_uses_agent_engine_path(self) -> None:
        calls: list[str] = []
        original = CodeWorkerEngine.run_execution

        def tracking_run(self: CodeWorkerEngine, **kwargs: Any):
            calls.append(kwargs["item"].work_item_id)
            return original(self, **kwargs)

        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(CodeWorkerEngine, "run_execution", tracking_run):
                result = AgentGraphRunner(default_planner_led_agent_workflow()).run("Use engine path.", tmp)

        self.assertEqual(result.status, "completed")
        self.assertIn("executor-work", calls)

    def test_ordinary_ui_does_not_expose_legacy_runtime_json_editor(self) -> None:
        app_source = (Path(__file__).parents[1] / "frontend" / "src" / "App.tsx").read_text(encoding="utf-8")
        agent_inspector_source = (
            Path(__file__).parents[1]
            / "frontend"
            / "src"
            / "features"
            / "agent-workflow"
            / "AgentWorkflowAgentInspector.tsx"
        ).read_text(encoding="utf-8")

        self.assertNotIn("Legacy Runtime Preview JSON", app_source)
        self.assertNotIn("Apply Legacy Runtime JSON", app_source)
        self.assertNotIn("View legacy runtime preview", app_source)
        self.assertNotIn("onChange={(event) => toggleCapability", agent_inspector_source)

    def test_agent_graph_product_artifacts_do_not_emit_legacy_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow()).run("Check artifacts.", tmp)

        legacy_types = {"plan_artifact", "patch_artifact", "review_artifact"}
        produced_types = {
            str(artifact.get("artifact_type"))
            for artifact in result.artifacts.values()
            if isinstance(artifact, dict)
        }
        self.assertEqual(result.status, "completed")
        self.assertFalse(produced_types.intersection(legacy_types))

    def test_agent_recipe_compiles_to_internal_runtime_profile(self) -> None:
        profile = RuntimeProfileCompiler().compile(
            AgentRecipe(id="worker", name="Worker", role="do_work", purpose="Implement a change.")
        )

        self.assertEqual(profile.engine_id, "code-worker-engine")
        self.assertEqual(profile.context_profile, "coding-worker")
        self.assertIn("execution_result", profile.allowed_artifacts)
        self.assertTrue(profile.tool_policy["write_files"])

    def test_workflow_agent_can_omit_manual_capabilities(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload["agents"][1] = {
            "id": "executor",
            "name": "Code Worker Agent",
            "role": "worker",
            "model_tier": "standard",
            "can_talk_to_human": False,
        }

        workflow = AgentWorkflowSpec.model_validate(payload)
        validation = validate_agent_workflow_payload(payload)

        self.assertEqual(validation.status, "pass")
        self.assertIn("return_execution_result", workflow.agents[1].capabilities)

    def test_harness_validator_enforces_engine_boundaries(self) -> None:
        valid_worker = AgentEngineSpec(
            id="code-worker-engine",
            name="Code Worker Engine",
            engine_type="worker",
            harness_graph=HarnessGraph(
                nodes=[
                    HarnessBlock(id="context", type="context_builder"),
                    HarnessBlock(id="loop", type="model_loop", config={"max_steps": 4}),
                    HarnessBlock(id="validate", type="artifact_validator"),
                    HarnessBlock(id="out", type="output_artifact"),
                ],
                edges=[("context", "loop"), ("loop", "validate"), ("validate", "out")],
            ),
            allowed_artifacts=["execution_result"],
            token_budget=TokenBudget(max_input_tokens=8000),
        )
        worker_asks_human = valid_worker.model_copy(
            update={
                "harness_graph": HarnessGraph(
                    nodes=[
                        *valid_worker.harness_graph.nodes,
                        HarnessBlock(id="ask", type="interrupt_gate", config={"ask_human": True}),
                    ]
                )
            }
        )
        tester_writes_files = valid_worker.model_copy(
            update={
                "id": "tester-engine",
                "engine_type": "tester",
                "harness_graph": HarnessGraph(
                    nodes=[
                        HarnessBlock(id="context", type="context_builder"),
                        HarnessBlock(id="apply", type="patch_preview", config={"operation": "patch_apply", "requires_preview": True}),
                        HarnessBlock(id="validate", type="artifact_validator"),
                        HarnessBlock(id="out", type="output_artifact"),
                    ]
                ),
            }
        )

        validator = HarnessValidator()

        self.assertTrue(validator.validate(valid_worker).valid)
        self.assertIn("non_planner_ask_human", {issue.code for issue in validator.validate(worker_asks_human).issues})
        self.assertIn("tester_cannot_write_files", {issue.code for issue in validator.validate(tester_writes_files).issues})

    def test_repair_logic_is_centralized_outside_executor_classes(self) -> None:
        executor_source = inspect.getsource(AgentGraphExecutor)

        self.assertNotIn("def _repair_once", executor_source)
        self.assertIn("ArtifactRepairService", executor_source)

    def test_extensions_api_splits_plugins_and_skills(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            plugins = client.get("/api/v2/extensions/plugins")
            skills = client.get("/api/v2/extensions/skills")
            search = client.get("/api/v2/extensions/search?q=worker")

        self.assertEqual(plugins.status_code, 200)
        self.assertEqual(skills.status_code, 200)
        self.assertEqual(search.status_code, 200)
        self.assertTrue(any(item["extension_type"] in {"plugin", "agent_engine"} for item in plugins.json()["plugins"]))

    def test_legacy_patch_tools_route_through_patch_service(self) -> None:
        registry_source = inspect.getsource(__import__("coder_workbench.tools.registry", fromlist=["_"]))

        self.assertIn("_propose_patch", registry_source)
        self.assertIn("_apply_patch", registry_source)
        self.assertIn("_rollback_patch", registry_source)
        self.assertIn("PatchService", registry_source)


if __name__ == "__main__":
    unittest.main()


def _wait_for_live_run(client: TestClient, run_id: str) -> dict[str, Any]:
    for _ in range(50):
        payload = client.get(f"/api/v2/live-agent-runs/{run_id}").json()
        if payload.get("status") not in {"queued", "running"}:
            return payload
        time.sleep(0.05)
    return payload
