from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from typing import Any

from coder_workbench.actions import ActionGateway, ActionSpec, RunContext
from coder_workbench.actions.schema import ACTION_TYPES
from coder_workbench.agent_graph.cache import GraphRunCache
from coder_workbench.agent_graph.schema import WorkItem
from coder_workbench.skills import SkillIndex


class ActionGatewayActionClosureTests(unittest.TestCase):
    def test_action_gateway_has_no_declared_unimplemented_actions(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "example.py").write_text("x = 1\n", encoding="utf-8")
            gateway = _gateway()
            context = RunContext(
                run_id="run",
                repo_root=root,
                cache=GraphRunCache(round=1),
                item=_work_item(),
                planner_order_ref="planner_order_round_1",
                upstream_refs=[],
                user_request="Build context.",
                role="worker",
                skill_index=SkillIndex(),
                skill_store_root=root / ".coder",
                repo_intelligence={},
                data={"preapprove_all": True},
            )
            specs = [
                ActionSpec(action_id="build_context", action_type="build_context"),
                ActionSpec(action_id="call_plugin", action_type="call_plugin", input={"operation_id": "safe.op", "approved": True}),
                ActionSpec(action_id="call_mcp", action_type="call_mcp", input={"operation_id": "mcp.fs.read", "approved": True}),
                ActionSpec(action_id="repo_index", action_type="repo_index"),
                ActionSpec(action_id="propose_patch", action_type="propose_patch", input={"changes": []}),
                ActionSpec(action_id="apply_patch_sandbox", action_type="apply_patch_sandbox", input={"changes": []}),
                ActionSpec(action_id="run_command_sandbox", action_type="run_command_sandbox", input={"command": "echo ok"}),
                ActionSpec(action_id="run_command", action_type="run_command", input={"command": "echo ok"}),
                ActionSpec(
                    action_id="validate_artifact",
                    action_type="validate_artifact",
                    input={
                        "expected_type": "execution_result",
                        "artifact": {
                            "artifact_type": "execution_result",
                            "status": "completed",
                            "summary": "Done.",
                        },
                    },
                ),
                ActionSpec(action_id="repair_artifact", action_type="repair_artifact"),
            ]

            self.assertEqual({spec.action_type for spec in specs}, ACTION_TYPES)
            for spec in specs:
                with self.subTest(action_type=spec.action_type):
                    result = gateway.run(spec, run_context=context)
                    self.assertNotEqual(result.error_code, "action_not_implemented")

    def test_action_gateway_repo_index_builds_repo_intelligence(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "example.py").write_text("x = 1\n", encoding="utf-8")
            data: dict[str, Any] = {}
            result = ActionGateway().run(
                ActionSpec(action_id="repo-index", action_type="repo_index"),
                run_context=RunContext(run_id="run", repo_root=tmp, data=data),
            )

        self.assertEqual(result.status, "ok")
        self.assertIn("repo_intelligence", result.payload)
        self.assertIn("repo_intelligence", data)

    def test_action_gateway_blocks_permissioned_plugin_before_runtime_execution(self) -> None:
        calls: list[str] = []

        class FakeExtensionRuntime:
            def execute_plugin_operation(self, operation_id, args, runtime_context):
                calls.append(operation_id)
                return {"status": "completed", "result": {}}

        gateway = ActionGateway(extension_runtime_factory=lambda: FakeExtensionRuntime())
        result = gateway.run(
            ActionSpec(
                action_id="plugin",
                action_type="call_plugin",
                input={"operation_id": "danger.op"},
                risk_level="high",
                requires_permission=True,
            ),
            run_context=RunContext(run_id="run", repo_root="."),
        )

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.error_code, "plugin_requires_approval")
        self.assertEqual(calls, [])
        self.assertTrue(result.payload["reservation"]["released"])

    def test_action_gateway_calls_plugin_runtime_when_approved(self) -> None:
        calls: list[str] = []

        class FakeExtensionRuntime:
            def execute_plugin_operation(self, operation_id, args, runtime_context):
                calls.append(operation_id)
                return {
                    "operation_id": operation_id,
                    "status": "completed",
                    "result": {"ok": True, "args": args},
                }

        gateway = ActionGateway(extension_runtime_factory=lambda: FakeExtensionRuntime())
        result = gateway.run(
            ActionSpec(
                action_id="plugin",
                action_type="call_plugin",
                input={"operation_id": "safe.op", "approved": True, "args": {"x": 1}},
                risk_level="medium",
                requires_permission=True,
            ),
            run_context=RunContext(run_id="run", repo_root="."),
        )

        self.assertEqual(result.status, "ok")
        self.assertEqual(calls, ["safe.op"])
        self.assertEqual(result.payload["operation"]["result"]["args"], {"x": 1})

    def test_call_mcp_uses_extension_runtime_boundary(self) -> None:
        calls: list[str] = []

        class FakeExtensionRuntime:
            def execute_plugin_operation(self, operation_id, args, runtime_context):
                calls.append(operation_id)
                return {"operation_id": operation_id, "status": "completed", "result": {"ok": True}}

        gateway = ActionGateway(extension_runtime_factory=lambda: FakeExtensionRuntime())
        result = gateway.run(
            ActionSpec(
                action_id="mcp",
                action_type="call_mcp",
                input={"operation_id": "mcp.fs.read", "approved": True},
                requires_permission=True,
            ),
            run_context=RunContext(run_id="run", repo_root="."),
        )

        self.assertEqual(result.status, "ok")
        self.assertEqual(calls, ["mcp.fs.read"])


class FakePatchService:
    def preview(self, changes):
        return {"status": "proposed", "change_count": 0}

    def apply(self, patch, *, approved: bool = False):
        return {"status": "applied", "approved": approved}


class FakeCommandService:
    def run_check(self, *args, **kwargs):
        return {"passed": True, "output": "ok"}


class FakeExtensionRuntime:
    def execute_plugin_operation(self, operation_id, args, runtime_context):
        return {"operation_id": operation_id, "status": "completed", "result": {"ok": True}}


def _gateway() -> ActionGateway:
    return ActionGateway(
        patch_service_factory=lambda repo_root, scopes, data: FakePatchService(),
        command_service_factory=lambda repo_root, scopes, data: FakeCommandService(),
        extension_runtime_factory=lambda: FakeExtensionRuntime(),
    )


def _work_item() -> WorkItem:
    return WorkItem(
        work_item_id="executor-work",
        merge_index=1,
        assignee_agent_id="executor",
        task_summary="Do work.",
        depends_on=[],
        tester_agent_ids=[],
    )


if __name__ == "__main__":
    unittest.main()
