from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

from coder_workbench.actions import ActionResult
from coder_workbench.agent_graph.cache import GraphRunCache
from coder_workbench.agent_graph.effects import apply_hidden_effects
from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, ExecutionRecord, PlannerInputBundle, PlannerOrder, WorkItem
from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.server.storage import RunStore


class AgentGraphEffectsTests(unittest.TestCase):
    def test_unapproved_optional_check_command_requires_planner_confirmation(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            marker = repo / "created.txt"
            command = f'"{sys.executable}" -c "from pathlib import Path; Path(\'created.txt\').write_text(\'bad\')"'

            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=EffectSourceExecutor(check_commands=[command]),
            ).run(
                "Run hidden effect.",
                str(repo),
            )
            marker_exists = marker.exists()

        self.assertEqual(result.status, "completed")
        self.assertFalse(marker_exists)
        effect = result.data["planner_input_bundle"]["effects"][0]
        self.assertEqual(effect["effect_type"], "optional_check_command")
        self.assertEqual(effect["status"], "check_requires_planner_confirmation")
        self.assertIn("approval_key", effect)

    def test_preapproved_optional_check_command_records_output_ref(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            command = f'"{sys.executable}" -c "print(42)"'

            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=EffectSourceExecutor(check_commands=[command]),
            ).run(
                "Run hidden effect.",
                str(repo),
                initial_data={"preapprove_all": True},
            )

        effect = result.data["planner_input_bundle"]["effects"][0]
        self.assertEqual(effect["status"], "completed")
        self.assertEqual(effect["artifact_ref"], "check_result_round_1_1")
        self.assertEqual(effect["output_ref"], "check_output_round_1_1")
        self.assertEqual(result.artifacts["check_result_round_1_1"]["artifact_type"], "check_result")
        output = result.data["graph_run_cache"]["hidden_effect_outputs"]["check_output_round_1_1"]
        self.assertIn("42", output["output"])
        tool_event = next(event for event in result.events if event.type == "tool.result")
        self.assertEqual(tool_event.payload["tool_result_id"], "check_output_round_1_1")

        with tempfile.TemporaryDirectory() as tmp:
            store = RunStore(Path(tmp) / ".coder")
            stored = store.save("agent-graph", tmp, "Run hidden effect.", result)
            loaded = store.get_tool_result(stored.id, "check_output_round_1_1")

        self.assertIn("42", loaded["output"])

    def test_modify_files_effect_creates_patch_preview_without_applying(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            src = repo / "src"
            src.mkdir()
            target = src / "example.txt"
            target.write_text("before\n", encoding="utf-8")

            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=EffectSourceExecutor(
                    proposed_changes=[
                        {
                            "path": "src/example.txt",
                            "action": "update",
                            "expected_before": "before\n",
                            "content": "after\n",
                        }
                    ],
                ),
            ).run(
                "Preview file changes.",
                str(repo),
                initial_data={"scopes": ["src"]},
            )
            current_content = target.read_text(encoding="utf-8")

        self.assertEqual(current_content, "before\n")
        effect = result.data["planner_input_bundle"]["effects"][0]
        self.assertEqual(effect["effect_type"], "modify_files")
        self.assertEqual(effect["status"], "patch_preview_created")
        patch_ref = effect["patch_ref"]
        self.assertTrue(patch_ref.startswith("patch_preview_"))
        self.assertEqual(effect["artifact_ref"], patch_ref)
        self.assertEqual(result.artifacts[patch_ref]["artifact_type"], "patch_preview")
        preview = result.data["graph_run_cache"]["hidden_effect_outputs"][patch_ref]
        self.assertEqual(preview["status"], "proposed")
        self.assertTrue(preview["requires_approval"])
        self.assertIn("-before", preview["files"][0]["diff"])
        self.assertIn("+after", preview["files"][0]["diff"])

    def test_modify_files_effect_blocks_risk_paths(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            (repo / ".env").write_text("TOKEN=old\n", encoding="utf-8")

            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=EffectSourceExecutor(
                    proposed_changes=[
                        {
                            "path": ".env",
                            "action": "update",
                            "expected_before": "TOKEN=old\n",
                            "content": "TOKEN=new\n",
                        }
                    ],
                ),
            ).run("Preview risky file changes.", str(repo))

        effect = result.data["planner_input_bundle"]["effects"][0]
        self.assertEqual(effect["status"], "patch_preview_blocked")
        self.assertEqual(result.data["planner_input_bundle"]["plan_status"], "interrupted")
        self.assertEqual(result.data["planner_input_bundle"]["interrupts"][0]["blocker_type"], "risk_boundary")

    def test_requested_plugin_action_becomes_hidden_effect(self) -> None:
        cache = GraphRunCache(round=1)
        cache.record_execution(
            ExecutionRecord(
                work_item_id="executor-work",
                merge_index=1,
                agent_id="executor",
                status="completed",
                execution_summary="Needs project index.",
                execution_result_ref="execution_result_executor-work",
                artifact_payload={
                    "artifact_type": "execution_result",
                    "status": "completed",
                    "summary": "Needs project index.",
                    "requested_actions": [
                        {"action_type": "call_plugin", "operation_id": "project_index", "args": {"max_files": 10}}
                    ],
                },
            )
        )

        class FakeGateway:
            def run(self, spec, *, run_context):
                return ActionResult(
                    status="ok",
                    summary=f"{spec.input['operation_id']} ok",
                    payload={"operation": {"status": "completed", "result": {"ok": True}}},
                )

        with tempfile.TemporaryDirectory() as tmp:
            records = apply_hidden_effects(
                agent_workflow=default_planner_led_agent_workflow(),
                cache=cache,
                repo_root=tmp,
                scopes=[],
                data={"run_id": "run", "preapprove_all": True},
                action_gateway=FakeGateway(),
            )

        effect = next(record for record in records if record["effect_type"] == "runtime_action")
        self.assertEqual(effect["status"], "ok")
        self.assertEqual(effect["operation_id"], "project_index")
        self.assertEqual(effect["output_ref"], "tool_result_round_1_1")
        self.assertIn("tool_result_round_1_1", cache.hidden_effect_outputs)

    def test_requested_plugin_action_enters_planner_input_bundle(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "README.md").write_text("sample\n", encoding="utf-8")
            result = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=EffectSourceExecutor(
                    requested_actions=[
                        {
                            "action_type": "call_plugin",
                            "operation_id": "project_index",
                            "args": {"max_files": 10},
                        }
                    ],
                ),
            ).run("Index the project.", tmp)

        effect = result.data["planner_input_bundle"]["effects"][0]
        output_ref = effect["output_ref"]

        self.assertEqual(effect["effect_type"], "runtime_action")
        self.assertEqual(effect["operation_id"], "project_index")
        self.assertEqual(effect["tool_result_ref"], output_ref)
        self.assertIn(output_ref, result.data["graph_run_cache"]["hidden_effect_outputs"])
        self.assertEqual(result.artifacts[output_ref]["artifact_type"], "runtime_action")

    def test_unknown_requested_runtime_action_records_failed_effect(self) -> None:
        cache = GraphRunCache(round=1)
        cache.record_execution(
            ExecutionRecord(
                work_item_id="executor-work",
                merge_index=1,
                agent_id="executor",
                status="completed",
                execution_summary="Requested unsupported runtime action.",
                execution_result_ref="execution_result_executor-work",
                artifact_payload={
                    "artifact_type": "execution_result",
                    "status": "completed",
                    "summary": "Requested unsupported runtime action.",
                    "requested_actions": [
                        {"action_type": "open_browser", "operation_id": "browser.open"}
                    ],
                },
            )
        )

        with tempfile.TemporaryDirectory() as tmp:
            records = apply_hidden_effects(
                agent_workflow=default_planner_led_agent_workflow(),
                cache=cache,
                repo_root=tmp,
                scopes=[],
                data={"run_id": "run"},
            )

        effect = next(record for record in records if record["effect_type"] == "runtime_action")
        self.assertEqual(effect["artifact_type"], "runtime_action")
        self.assertEqual(effect["status"], "failed")
        self.assertEqual(effect["error_code"], "unknown_action_type")
        self.assertEqual(effect["work_item_id"], "executor-work")
        self.assertEqual(effect["action_spec"]["action_type"], "open_browser")
        self.assertTrue(effect["requires_planner_replan"])

    def test_blocked_plugin_runtime_action_records_replay_metadata(self) -> None:
        cache = GraphRunCache(round=1)
        cache.record_execution(
            ExecutionRecord(
                work_item_id="executor-work",
                merge_index=1,
                agent_id="executor",
                status="completed",
                execution_summary="Needs project index after approval.",
                execution_result_ref="execution_result_executor-work",
                artifact_payload={
                    "artifact_type": "execution_result",
                    "status": "completed",
                    "summary": "Needs project index after approval.",
                    "requested_actions": [
                        {
                            "action_type": "call_plugin",
                            "operation_id": "project_index",
                            "risk_level": "high",
                            "args": {"max_files": 5},
                        }
                    ],
                },
            )
        )

        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "README.md").write_text("sample\n", encoding="utf-8")
            records = apply_hidden_effects(
                agent_workflow=default_planner_led_agent_workflow(),
                cache=cache,
                repo_root=tmp,
                scopes=[],
                data={"run_id": "run"},
            )

        effect = next(record for record in records if record["effect_type"] == "runtime_action")
        self.assertEqual(effect["status"], "blocked")
        self.assertEqual(effect["error_code"], "plugin_requires_approval")
        self.assertEqual(effect["approval_key"], "plugin:project_index:high")
        self.assertEqual(effect["policy"]["operation_id"], "project_index")
        self.assertEqual(effect["action_spec"]["action_type"], "call_plugin")
        self.assertEqual(effect["action_spec"]["input"]["operation_id"], "project_index")
        self.assertEqual(effect["work_item_id"], "executor-work")

    def test_blocked_runtime_action_finishes_without_planner_response_checkpoint(self) -> None:
        executor = RuntimeActionReplayExecutor()
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "README.md").write_text("sample\n", encoding="utf-8")
            blocked = AgentGraphRunner(
                default_planner_led_agent_workflow(),
                executor=executor,
            ).run("Replay approved action.", tmp)
            blocked_effect = blocked.data["planner_input_bundle"]["effects"][0]

        self.assertEqual(blocked.status, "blocked")
        self.assertEqual(blocked.status_code, "planner_blocked")
        self.assertIsNone(blocked.resume_checkpoint)
        self.assertEqual(executor.execution_calls, 1)
        self.assertEqual(blocked_effect["status"], "blocked")
        self.assertEqual(blocked.data["planner_decision"]["next_action"], "finish")
        self.assertEqual(blocked.data["planner_decision"]["final_status"], "blocked")
        self.assertEqual(blocked.data["final_report"]["status"], "blocked")


class EffectSourceExecutor:
    def __init__(
        self,
        *,
        proposed_changes: list[dict[str, Any]] | None = None,
        check_commands: list[str] | None = None,
        requested_actions: list[dict[str, Any]] | None = None,
    ) -> None:
        self.proposed_changes = proposed_changes or []
        self.check_commands = check_commands or []
        self.requested_actions = requested_actions or []

    def create_planner_order(self, request: str, *, emit=None) -> PlannerOrder:
        return PlannerOrder.model_validate(
            {
                "artifact_type": "planner_order",
                "round": 1,
                "round_goal": request,
                "plan_graph": {
                    "work_items": [
                        {
                            "work_item_id": "executor-work",
                            "merge_index": 1,
                            "assignee_agent_id": "executor",
                            "task_summary": "Run effect source work.",
                            "depends_on": [],
                        }
                    ]
                },
            }
        )

    def create_execution_result(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        emit=None,
    ) -> ExecutionRecord:
        artifact = {
            "artifact_type": "execution_result",
            "round": envelope.round,
            "work_item_id": item.work_item_id,
            "merge_index": item.merge_index,
            "agent_id": item.assignee_agent_id,
            "status": "completed",
            "summary": "Execution produced validated effect inputs.",
            "proposed_changes": self.proposed_changes,
            "requested_actions": [
                *self.requested_actions,
                *[{"action_type": "run_command_sandbox", "command": command} for command in self.check_commands],
            ],
            "outputs": ["execution_result_executor-work"],
            "verification": {
                "status": "pass",
                "checks_run": [
                    {
                        "check_id": "static",
                        "kind": "static",
                        "command": None,
                        "status": "pass",
                        "summary": "Execution result contains effect requests.",
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
        }
        return ExecutionRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status="completed",
            execution_summary=artifact["summary"],
            execution_result_ref="execution_result_executor-work",
            artifact_payload=artifact,
        )

    def create_planner_decision(
        self,
        *,
        bundle: PlannerInputBundle,
        emit=None,
    ) -> dict[str, Any]:
        return {
            "artifact_type": "planner_decision",
            "round": bundle.round,
            "task_done": True,
            "next_action": "finish",
            "reason": "Effect source test completed.",
        }


class RuntimeActionReplayExecutor(EffectSourceExecutor):
    def __init__(self) -> None:
        super().__init__(
            requested_actions=[
                {
                    "action_type": "call_plugin",
                    "operation_id": "project_index",
                    "risk_level": "high",
                    "args": {"max_files": 5},
                }
            ]
        )
        self.execution_calls = 0

    def create_execution_result(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        emit=None,
    ) -> ExecutionRecord:
        self.execution_calls += 1
        return super().create_execution_result(item=item, envelope=envelope, emit=emit)

    def create_planner_decision(
        self,
        *,
        bundle: PlannerInputBundle,
        emit=None,
    ) -> dict[str, Any]:
        replay_ok = any(
            effect.get("effect_type") == "runtime_action"
            and effect.get("status") == "ok"
            and effect.get("replay_of")
            for effect in bundle.effects
        )
        blocked_runtime_action = any(
            effect.get("effect_type") == "runtime_action"
            and effect.get("status") == "blocked"
            for effect in bundle.effects
        )
        if blocked_runtime_action and not replay_ok:
            return {
                "artifact_type": "planner_decision",
                "round": bundle.round,
                "task_done": False,
                "next_action": "ask_human",
                "risk_level": "medium",
                "requires_human_confirmation": True,
                "reason": "Runtime action requires user approval.",
                "human_message": "Approve project_index runtime action?",
            }
        return {
            "artifact_type": "planner_decision",
            "round": bundle.round,
            "task_done": True,
            "next_action": "finish",
            "reason": "Approved runtime action replay completed.",
        }


if __name__ == "__main__":
    unittest.main()
