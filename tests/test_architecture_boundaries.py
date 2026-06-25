from __future__ import annotations

import inspect
import tempfile
import time
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from fastapi.testclient import TestClient

from coder_workbench.actions import ActionGateway, ActionResult
from coder_workbench.actions.schema import ACTION_TYPES
from coder_workbench.agent_graph.cache import GraphRunCache
from coder_workbench.agent_graph.agent_run import AgentRun
from coder_workbench.agent_graph.effects import apply_hidden_effects
from coder_workbench.agent_graph.executor import AgentGraphExecutor
from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_graph.schema import ExecutionRecord
from coder_workbench.agent_harness import CodeWorkerHarness
from coder_workbench.agent_model import AgentRecipe, RuntimeProfileCompiler
from coder_workbench.budget import BudgetLimit
from coder_workbench.core import AgentWorkflowSpec, default_planner_led_agent_workflow, validate_agent_workflow_payload
from coder_workbench.server.storage import RunStore
from coder_workbench.server.settings import ProviderSettings
from coder_workbench.server.app import create_app


class ArchitectureBoundaryTests(unittest.TestCase):
    def test_live_agent_runs_use_agentgraph_urls(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
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
        self.assertIn("/api/v2/live-agent-runs/", response.json()["events_url"])
        self.assertIn("/api/v2/live-agent-runs/", response.json()["result_url"])

    def test_agent_graph_runner_does_not_import_legacy_agent_execution_adapter(self) -> None:
        source = inspect.getsource(__import__("coder_workbench.agent_graph.runner", fromlist=["_"]))

        self.assertNotIn("DefaultAgentExecutor", source)
        self.assertNotIn("AgentGraphExecutor", source)
        self.assertNotIn("signature(", source)
        self.assertIn("AgentRun", source)

    def test_resume_after_node_is_not_a_normal_runtime_api(self) -> None:
        runner_source = inspect.getsource(__import__("coder_workbench.agent_graph.runner", fromlist=["_"]))
        app_source = inspect.getsource(__import__("coder_workbench.server.app", fromlist=["_"]))
        frontend_source = (Path(__file__).resolve().parents[1] / "frontend" / "src" / "App.tsx").read_text(encoding="utf-8")

        self.assertNotIn("resume_after_node", runner_source)
        self.assertNotIn("resume_after_node", app_source)
        self.assertNotIn("resumeAfter", frontend_source)

    def test_agent_graph_low_level_services_route_through_action_gateway(self) -> None:
        runner_source = inspect.getsource(__import__("coder_workbench.agent_graph.runner", fromlist=["_"]))
        effects_source = inspect.getsource(__import__("coder_workbench.agent_graph.effects", fromlist=["_"]))

        self.assertNotIn("ContextService", runner_source)
        self.assertIn("ActionGateway", runner_source)
        self.assertNotIn("PatchService", effects_source)
        self.assertNotIn("CommandService", effects_source)
        self.assertIn("ActionGateway", effects_source)

    def test_runner_delegates_wave_concurrency_to_wave_executor(self) -> None:
        runner_source = inspect.getsource(__import__("coder_workbench.agent_graph.runner", fromlist=["_"]))

        self.assertIn("WaveExecutor", runner_source)
        self.assertNotIn("ThreadPoolExecutor", runner_source)
        self.assertNotIn("as_completed", runner_source)

    def test_product_action_types_are_gateway_closed(self) -> None:
        gateway_source = inspect.getsource(ActionGateway.run)

        self.assertNotIn("load_skill", ACTION_TYPES)
        for action_type in ACTION_TYPES:
            expected = "run_command" if action_type in {"run_command", "run_command_sandbox"} else action_type
            with self.subTest(action_type=action_type):
                self.assertIn(expected, gateway_source)

    def test_agent_run_uses_runtime_profile_cache(self) -> None:
        source = inspect.getsource(__import__("coder_workbench.agent_graph.agent_run", fromlist=["_"]))

        self.assertIn("RuntimeProfileCache", source)
        self.assertIn("compile_or_get", source)

    def test_code_work_item_uses_harness_runtime_fallback_path(self) -> None:
        calls: list[str] = []
        original = CodeWorkerHarness.create_execution_result

        def tracking_run(self: CodeWorkerHarness, **kwargs: Any):
            calls.append(kwargs["item"].work_item_id)
            return original(self, **kwargs)

        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(CodeWorkerHarness, "create_execution_result", tracking_run):
                result = AgentGraphRunner(default_planner_led_agent_workflow()).run("Use harness runtime path.", tmp)

        self.assertEqual(result.status, "completed")
        self.assertIn("executor-work", calls)

    def test_product_agent_work_dispatches_through_agent_run_facade(self) -> None:
        calls: list[str] = []
        originals = {
            "planner_order": AgentRun.run_planner_order,
            "execution": AgentRun.run_execution,
            "planner_decision": AgentRun.run_planner_decision,
        }

        def track(name: str, original: Any):
            def wrapper(self: AgentRun, *args: Any, **kwargs: Any):
                calls.append(name)
                return original(self, *args, **kwargs)

            return wrapper

        with tempfile.TemporaryDirectory() as tmp:
            with (
                patch.object(AgentRun, "run_planner_order", track("planner_order", originals["planner_order"])),
                patch.object(AgentRun, "run_execution", track("execution", originals["execution"])),
                patch.object(AgentRun, "run_planner_decision", track("planner_decision", originals["planner_decision"])),
            ):
                result = AgentGraphRunner(default_planner_led_agent_workflow()).run("Use every product AgentRun path.", tmp)

        self.assertEqual(result.status, "completed")
        for name in originals:
            self.assertIn(name, calls)

    def test_start_work_item_builds_context_through_action_gateway(self) -> None:
        action_types: list[str] = []
        original = ActionGateway.run

        def tracking_run(self: ActionGateway, spec, *, run_context):
            action_types.append(spec.action_type)
            return original(self, spec, run_context=run_context)

        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(ActionGateway, "run", tracking_run):
                result = AgentGraphRunner(default_planner_led_agent_workflow()).run("Use gateway.", tmp)

        self.assertEqual(result.status, "completed")
        self.assertIn("build_context", action_types)

    def test_hidden_effects_route_patch_and_checks_through_action_gateway(self) -> None:
        cache = GraphRunCache(round=1)
        cache.record_execution(
            ExecutionRecord(
                work_item_id="executor-work",
                merge_index=1,
                agent_id="executor",
                status="completed",
                execution_summary="Proposed change.",
                execution_result_ref="execution_result_executor-work",
                artifact_payload={
                    "artifact_type": "execution_result",
                    "status": "completed",
                    "summary": "Proposed change.",
                    "proposed_changes": [
                        {"path": "src/example.py", "action": "update", "content": "value = 2\n"}
                    ],
                    "requested_actions": [{"action_type": "run_command_sandbox", "command": "python -m unittest", "cwd": "."}],
                    "outputs": ["execution_result_executor-work"],
                    "verification": {
                        "status": "pass",
                        "checks_run": [
                            {
                                "check_id": "static",
                                "kind": "static",
                                "command": None,
                                "status": "pass",
                                "summary": "Effect inputs were produced.",
                                "output_ref": None,
                                "evidence_refs": ["execution_result_executor-work"],
                            }
                        ],
                        "evidence_refs": ["execution_result_executor-work"],
                        "confidence": "medium",
                        "remaining_work": [],
                        "no_check_rationale": None,
                        "repair_attempted": False,
                        "repair_summary": None,
                    },
                },
            )
        )
        action_types: list[str] = []

        def tracking_run(self: ActionGateway, spec, *, run_context):
            action_types.append(spec.action_type)
            if spec.action_type == "run_command_sandbox":
                return ActionResult(
                    status="ok",
                    summary="Sandbox check completed.",
                    payload={"result": {"passed": True, "returncode": 0, "output": "ok"}},
                )
            if spec.action_type == "propose_patch":
                return ActionResult(
                    status="ok",
                    summary="Patch preview generated.",
                    payload={"preview": {"status": "proposed", "patch_id": "patch-1", "change_count": 1}},
                )
            return ActionResult(status="failed", summary="Unexpected action.", error_code="unexpected_action")

        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(ActionGateway, "run", tracking_run):
                records = apply_hidden_effects(
                    agent_workflow=default_planner_led_agent_workflow(),
                    cache=cache,
                    repo_root=tmp,
                    scopes=[],
                    data={"run_id": "run"},
                    action_gateway=ActionGateway(),
                )

        self.assertIn("run_command_sandbox", action_types)
        self.assertIn("propose_patch", action_types)
        self.assertTrue(any(record["status"] == "patch_preview_created" for record in records))

    def test_round_budget_preflight_blocks_before_worker_wave(self) -> None:
        model = CountingChatModel(
            [
                (
                    '{"artifact_type":"planner_order","round":1,"round_goal":"Stay inside budget.",'
                    '"plan_graph":{"work_items":[{"work_item_id":"budget-work","merge_index":1,'
                    '"assignee_agent_id":"executor","task_summary":"Implement within budget.",'
                    '"depends_on":[]}]}}'
                )
            ]
        )
        settings = ProviderSettings(
            default_provider="openai",
            default_model="fake-model",
            api_keys={"openai": "test-key"},
            mock_mode=False,
        )
        workflow = default_planner_led_agent_workflow()
        agent_run = AgentRun(
            workflow,
            runtime_settings=settings,
            model_factory=lambda config: model,
        )

        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(
                workflow,
                runtime_settings=settings,
                agent_run=agent_run,
            ).run(
                "Stay inside budget.",
                tmp,
                initial_data={"budget_limit": {"max_model_calls": 1}},
            )

        event_types = {event.type for event in result.events}

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.status_code, "round_model_call_budget_exceeded")
        self.assertEqual(model.invocation_count, 0)
        self.assertIn("planner.order.produced", event_types)
        self.assertNotIn("agent_graph.wave.started", event_types)
        self.assertNotIn("action.started", event_types)
        self.assertIn("budget_preflight", result.data)
        self.assertFalse(result.data["budget_preflight"][0]["approved"])
        self.assertEqual(result.data["budget_preflight"][0]["reason"], "round_model_call_budget_exceeded")

    def test_agent_run_no_longer_imports_obsolete_agent_engine_package(self) -> None:
        root = Path(__file__).resolve().parents[1]
        agent_engine_dir = root / "src" / "coder_workbench" / "agent_engine"
        source = inspect.getsource(__import__("coder_workbench.agent_graph.agent_run", fromlist=["_"]))

        self.assertFalse(any(agent_engine_dir.glob("*.py")))
        self.assertNotIn("agent_engine", source)
        self.assertNotIn("engine_registry", source)
        self.assertIn("HarnessRuntimeManager", source)
        self.assertIn("InternalFallbackProvider", source)

    def test_ordinary_ui_does_not_expose_legacy_runtime_json_editor(self) -> None:
        app_source = (Path(__file__).parents[1] / "frontend" / "src" / "App.tsx").read_text(encoding="utf-8")

        self.assertNotIn("Legacy Runtime Preview JSON", app_source)
        self.assertNotIn("Apply Legacy Runtime JSON", app_source)
        self.assertNotIn("View legacy runtime preview", app_source)
        self.assertNotIn("jsonText", app_source)
        self.assertNotIn("Apply JSON", app_source)

        frontend_root = Path(__file__).parents[1] / "frontend" / "src"
        frontend_source = "\n".join(
            path.read_text(encoding="utf-8")
            for path in frontend_root.rglob("*")
            if path.suffix in {".ts", ".tsx"}
        )
        self.assertNotIn("planner_mode", frontend_source)
        self.assertNotIn("PlannerStrategy", frontend_source)
        self.assertNotIn("planner strategy", frontend_source)

    def test_single_executor_planner_strategy_preserves_control_plane_path(self) -> None:
        execution_calls: list[str] = []
        action_types: list[str] = []
        original_execution = AgentRun.run_execution
        original_gateway_run = ActionGateway.run

        def tracking_execution(self: AgentRun, *args: Any, **kwargs: Any):
            item = kwargs["item"]
            execution_calls.append(item.work_item_id)
            return original_execution(self, *args, **kwargs)

        def tracking_gateway(self: ActionGateway, spec, *, run_context):
            action_types.append(spec.action_type)
            return original_gateway_run(self, spec, run_context=run_context)

        with tempfile.TemporaryDirectory() as tmp:
            with (
                patch.object(AgentRun, "run_execution", tracking_execution),
                patch.object(ActionGateway, "run", tracking_gateway),
            ):
                result = AgentGraphRunner(default_planner_led_agent_workflow()).run(
                    "Run a single executor local plan.",
                    tmp,
                    initial_data={"planner_mode": "single_executor"},
                )

        event_types = {event.type for event in result.events}

        self.assertEqual(result.status, "completed")
        self.assertEqual(execution_calls, ["executor-work"])
        self.assertIn("build_context", action_types)
        self.assertNotIn("propose_patch", action_types)
        self.assertNotIn("run_command_sandbox", action_types)
        self.assertIn("planner_order", result.data)
        self.assertIn("planner_input_bundle", result.data)
        self.assertIn("planner_decision", result.data)
        self.assertEqual(result.data["planner_order"]["plan_graph"]["work_items"][0]["assignee_agent_id"], "executor")
        self.assertEqual(result.data["planner_decision"]["next_action"], "finish")
        self.assertIn("planner.strategy.used", event_types)

    def test_planner_strategy_module_does_not_call_low_level_effects(self) -> None:
        source = inspect.getsource(__import__("coder_workbench.agent_graph.planner_strategy", fromlist=["_"]))

        self.assertNotIn("ActionGateway", source)
        self.assertNotIn("propose_patch", source)
        self.assertNotIn("run_command_sandbox", source)

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
        self.assertIn("trace_id", result.data)
        self.assertIn("trace_spans", result.data)
        self.assertIn("budget_usage", result.data)
        self.assertIn("budget_reservations", result.data)
        self.assertIn("run_controller", result.data)
        self.assertIn("runtime_profiles", result.data)
        self.assertIn("token_ledger", result.data)
        self.assertIn("graph_run_cache", result.data)

    def test_agent_recipe_compiles_to_internal_runtime_profile(self) -> None:
        profile = RuntimeProfileCompiler().compile(
            AgentRecipe(id="executor", name="Executor", role="executor", purpose="Implement a change.")
        )

        self.assertEqual(profile.engine_id, "code-worker-engine")
        self.assertEqual(profile.context_profile, "coding-executor")
        self.assertIn("execution_result", profile.allowed_artifacts)
        self.assertTrue(profile.tool_policy["write_files"])

    def test_workflow_agent_can_omit_manual_capabilities(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload["agents"][1] = {
            "id": "executor",
            "name": "Executor Agent",
            "role": "executor",
            "model_tier": "standard",
            "can_talk_to_human": False,
        }

        workflow = AgentWorkflowSpec.model_validate(payload)
        validation = validate_agent_workflow_payload(payload)

        self.assertEqual(validation.status, "pass")
        self.assertIn("return_execution_result", workflow.agents[1].capabilities)

    def test_repair_logic_is_centralized_outside_executor_classes(self) -> None:
        executor_source = inspect.getsource(AgentGraphExecutor)
        agent_run_source = inspect.getsource(__import__("coder_workbench.agent_graph.agent_run", fromlist=["_"]))

        self.assertNotIn("def _repair_once", executor_source)
        self.assertNotIn("ArtifactRepairService", executor_source)
        self.assertNotIn("ArtifactRepairService", agent_run_source)
        self.assertNotIn("build_planner_order_prompt", executor_source)
        self.assertNotIn("build_planner_decision_prompt", executor_source)

    def test_extensions_api_splits_plugins_and_skills(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            plugins = client.get("/api/v2/extensions/plugins")
            skills = client.get("/api/v2/extensions/skills")
            search = client.get("/api/v2/extensions/search?q=executor")

        self.assertEqual(plugins.status_code, 200)
        self.assertEqual(skills.status_code, 200)
        self.assertEqual(search.status_code, 200)
        self.assertTrue(any(item["extension_type"] in {"plugin", "harness_runtime"} for item in plugins.json()["plugins"]))

    def test_legacy_patch_tools_route_through_patch_service(self) -> None:
        registry_source = inspect.getsource(__import__("coder_workbench.tools.registry", fromlist=["_"]))

        self.assertIn("_propose_patch", registry_source)
        self.assertIn("_apply_patch", registry_source)
        self.assertIn("_rollback_patch", registry_source)
        self.assertIn("PatchService", registry_source)

    def test_run_store_save_uses_partitioned_primary_write_path(self) -> None:
        source = inspect.getsource(RunStore.save)

        self.assertIn("partitions.metadata.write", source)
        self.assertIn("partitions.results.write", source)
        self.assertIn("_write_artifacts", source)
        self.assertIn("_write_events", source)
        self.assertIn("_write_ledgers", source)
        self.assertIn("_externalize_context_packets", source)
        self.assertIn("_externalize_tool_results", source)
        self.assertNotIn("metadata.json", source)
        self.assertNotIn("result.json", source)


if __name__ == "__main__":
    unittest.main()


class CountingResponse:
    def __init__(self, content: str) -> None:
        self.content = content


class CountingChatModel:
    def __init__(self, responses: list[str]) -> None:
        self.responses = list(responses)
        self.prompts: list[str] = []

    @property
    def invocation_count(self) -> int:
        return len(self.prompts)

    def invoke(self, prompt: str) -> CountingResponse:
        self.prompts.append(prompt)
        if not self.responses:
            raise AssertionError("model should not be invoked again")
        return CountingResponse(self.responses.pop(0))


def _wait_for_live_run(client: TestClient, run_id: str) -> dict[str, Any]:
    for _ in range(50):
        payload = client.get(f"/api/v2/live-agent-runs/{run_id}").json()
        if payload.get("status") not in {"queued", "running"}:
            return payload
        time.sleep(0.05)
    return payload
