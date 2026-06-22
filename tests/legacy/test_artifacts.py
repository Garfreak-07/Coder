from __future__ import annotations

import tempfile
import time
import unittest
from pathlib import Path
from typing import Any

from coder_workbench.core.schema import WorkflowSpec
from coder_workbench.core.artifacts import artifact_summary, validate_artifact
from coder_workbench.core.preflight import validate_workflow_preflight
from coder_workbench.runtime import RunEvent, RunResult
from coder_workbench.runtime.runner import WorkflowRunner
from coder_workbench.server.manager import RunManager
from coder_workbench.server.storage import RunStore
from coder_workbench.tools import default_tool_registry


class StaticExecutor:
    def __init__(self, result: dict[str, Any]) -> None:
        self.result = result

    def run(self, agent, context: dict[str, Any]) -> dict[str, Any]:
        return dict(self.result)


class SequenceExecutor:
    def __init__(self, results: list[dict[str, Any]]) -> None:
        self.results = list(results)

    def run(self, agent, context: dict[str, Any]) -> dict[str, Any]:
        if len(self.results) > 1:
            return dict(self.results.pop(0))
        return dict(self.results[0])


class ArtifactRuntimeTests(unittest.TestCase):
    def test_agent_output_is_validated_and_recorded_as_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workflow = _single_agent_workflow("plan_artifact")
            executor = StaticExecutor(
                {
                    "artifact_type": "plan_artifact",
                    "summary": "Implement a small change.",
                    "target_files": ["src/example.py"],
                    "required_context": ["project_index"],
                    "implementation_steps": ["Inspect file", "Patch file"],
                    "risks": [],
                    "recommended_checks": ["python -m unittest"],
                    "executor_instructions": "Return a patch artifact.",
                }
            )

            result = WorkflowRunner(workflow, agent_executor=executor).run("plan work", tmp)

            self.assertEqual(result.status, "completed")
            produced = [event for event in result.events if event.type == "artifact.produced"]
            self.assertEqual(len(produced), 1)
            artifact_id = produced[0].payload["artifact_id"]
            self.assertIn(artifact_id, result.artifacts)
            self.assertEqual(result.data["agent_result"]["artifact_id"], artifact_id)
            self.assertEqual(produced[0].payload["summary"]["artifact_type"], "plan_artifact")

    def test_invalid_declared_artifact_blocks_before_downstream_nodes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workflow = _single_agent_workflow("plan_artifact")
            executor = StaticExecutor({"artifact_type": "plan_artifact"})

            result = WorkflowRunner(workflow, agent_executor=executor).run("plan work", tmp)

            self.assertEqual(result.status, "blocked")
            self.assertEqual(result.blocked_node_id, "agent")
            self.assertEqual(result.status_code, "artifact_validation_failed")
            self.assertEqual(result.artifacts, {})
            self.assertTrue(any(event.type == "artifact.validation_failed" for event in result.events))

    def test_live_recovery_preserves_non_approval_artifact_block(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store = RunStore(Path(tmp) / ".coder")
            workflow = _single_agent_workflow("plan_artifact")
            executor = StaticExecutor({"artifact_type": "plan_artifact"})
            manager = RunManager(store, runner_factory=lambda spec: WorkflowRunner(spec, agent_executor=executor))
            run = manager.start(workflow, tmp, "plan work", {})

            _wait_for_status(run, "blocked")
            restored = RunManager(store, runner_factory=lambda spec: WorkflowRunner(spec, agent_executor=executor)).get(run.id)

            self.assertIsNotNone(restored.result)
            assert restored.result is not None
            self.assertEqual(restored.status, "blocked")
            self.assertEqual(restored.result.status_code, "artifact_validation_failed")
            self.assertFalse(restored.result.events[-1].type == "approval.required")
            with self.assertRaisesRegex(ValueError, "not waiting for approval"):
                RunManager(store, runner_factory=lambda spec: WorkflowRunner(spec, agent_executor=executor)).approve(restored.id)

    def test_live_retry_current_node_reexecutes_blocked_artifact_node(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store = RunStore(Path(tmp) / ".coder")
            workflow = _single_agent_workflow("plan_artifact")
            executor = SequenceExecutor(
                [
                    {"artifact_type": "plan_artifact"},
                    {
                        "artifact_type": "plan_artifact",
                        "summary": "Retry produced a valid plan.",
                        "target_files": ["src/example.py"],
                        "required_context": [],
                        "implementation_steps": ["Patch file"],
                        "risks": [],
                        "recommended_checks": [],
                        "executor_instructions": "Return a patch artifact.",
                    },
                ]
            )
            manager = RunManager(store, runner_factory=lambda spec: WorkflowRunner(spec, agent_executor=executor))
            run = manager.start(workflow, tmp, "plan work", {})

            _wait_for_status(run, "blocked")
            manager.retry_current_node(run.id)
            _wait_for_status(run, "completed")

            self.assertIsNotNone(run.result)
            assert run.result is not None
            self.assertEqual(run.result.status, "completed")
            self.assertIn("agent_result", run.result.data)


class PlannerArtifactSchemaTests(unittest.TestCase):
    def test_synthesis_artifact_validates_and_summarizes(self) -> None:
        artifact = validate_artifact(
            {
                "artifact_type": "synthesis_artifact",
                "round": 1,
                "work_item_id": "organize-work",
                "merge_index": 1,
                "agent_id": "organizer",
                "status": "completed",
                "summary": "Organized source material.",
                "sources": [
                    {
                        "source_id": "task",
                        "ref": "planner_order_round_1",
                        "source_type": "task",
                        "title": "Task",
                        "summary": "Organize the facts.",
                    }
                ],
                "deduplicated_source_ids": ["task"],
                "clusters": [
                    {
                        "cluster_id": "cluster-1",
                        "title": "Facts",
                        "summary": "Organize the facts.",
                        "source_ids": ["task"],
                        "rank_score": 1.0,
                    }
                ],
                "ranked_items": [
                    {
                        "item_id": "ranked-1",
                        "rank": 1,
                        "title": "Facts",
                        "summary": "Organize the facts.",
                        "source_ids": ["task"],
                        "score": 1.0,
                    }
                ],
                "compressed_summary": "Facts: Organize the facts.",
                "index": {"facts": ["task"]},
            }
        )

        summary = artifact_summary(artifact)

        self.assertEqual(artifact["artifact_type"], "synthesis_artifact")
        self.assertEqual(summary["sources"], 1)
        self.assertEqual(summary["clusters"], 1)
        self.assertEqual(summary["ranked_items"], 1)


class ArtifactStorageTests(unittest.TestCase):
    def test_artifacts_are_split_and_large_values_become_blobs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / ".coder"
            store = RunStore(root)
            large_diff = "diff --git a/file.txt b/file.txt\n" + ("+" * 9000)
            artifact = {
                "artifact_id": "artifact_test",
                "artifact_type": "patch_artifact",
                "implementation_summary": "Prepared a patch.",
                "changed_files": ["file.txt"],
                "patches": [{"path": "file.txt", "action": "update", "diff": large_diff}],
                "risks": [],
                "suggested_check_command": "python -m unittest",
            }
            result = RunResult(
                status="completed",
                data={},
                summaries={},
                artifacts={"artifact_test": artifact},
                events=[RunEvent(type="artifact.produced", message="artifact", payload={"artifact_id": "artifact_test"})],
                estimated_tokens_used=0,
                agent_calls=1,
                tool_calls=0,
            )

            stored = store.save("workflow", "/repo", "patch", result)
            run_dir = root / "runs" / stored.id

            self.assertTrue((run_dir / "artifacts" / "artifact_test.json").exists())
            compact = store.get(stored.id, include_events=False).result.artifacts["artifact_test"]
            self.assertEqual(compact["artifact_type"], "patch_artifact")
            self.assertIn("summary", compact)

            loaded = store.get_artifact(stored.id, "artifact_test")
            diff_ref = loaded["patches"][0]["diff"]
            self.assertIn("blob_id", diff_ref)
            self.assertEqual(diff_ref["size_chars"], len(large_diff))
            blob = store.get_blob(diff_ref["blob_id"])
            self.assertEqual(blob["content"], large_diff)

    def test_tool_results_are_split_from_stored_event_log(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp) / "repo"
            repo.mkdir()
            target = repo / "sample.txt"
            target.write_text("before\n", encoding="utf-8")
            workflow = WorkflowSpec.model_validate(
                {
                    "id": "tool-result-test",
                    "name": "Tool result test",
                    "nodes": [
                        {"id": "start", "type": "start"},
                        {
                            "id": "propose_patch",
                            "type": "tool",
                            "tool": "propose_patch",
                            "output_key": "patch_preview",
                            "input": {
                                "files": [
                                    {
                                        "path": "sample.txt",
                                        "action": "update",
                                        "content": "after\n",
                                    }
                                ]
                            },
                        },
                        {"id": "end", "type": "end"},
                    ],
                    "edges": [
                        {"from": "start", "to": "propose_patch"},
                        {"from": "propose_patch", "to": "end"},
                    ],
                }
            )
            result = WorkflowRunner(workflow).run("preview patch", str(repo))
            live_tool_result = next(event for event in result.events if event.type == "tool.result")

            self.assertIn("result", live_tool_result.payload)
            self.assertEqual(live_tool_result.payload["result"]["status"], "proposed")

            store = RunStore(Path(tmp) / ".coder")
            stored = store.save("workflow", str(repo), "preview patch", result)
            event_page = store.get_events(stored.id)
            stored_tool_result = next(event for event in event_page["events"] if event["type"] == "tool.result")

            self.assertNotIn("result", stored_tool_result["payload"])
            tool_result_id = stored_tool_result["payload"]["tool_result_id"]
            loaded = store.get_tool_result(stored.id, tool_result_id)
            self.assertEqual(loaded["status"], "proposed")
            self.assertEqual(loaded["files"][0]["path"], "sample.txt")
            self.assertIn("-before", loaded["files"][0]["diff"])

    def test_large_tool_result_values_become_blobs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            large_output = "check output\n" + ("x" * 9000)
            result = RunResult(
                status="completed",
                data={},
                summaries={},
                events=[
                    RunEvent(
                        type="tool.result",
                        message="tool result",
                        node_id="check",
                        payload={
                            "tool": "run_check",
                            "result": {"status": "completed", "output": large_output},
                        },
                    )
                ],
                estimated_tokens_used=0,
                agent_calls=0,
                tool_calls=1,
            )

            store = RunStore(Path(tmp) / ".coder")
            stored = store.save("workflow", "/repo", "check", result)
            event_page = store.get_events(stored.id)
            stored_tool_result = next(event for event in event_page["events"] if event["type"] == "tool.result")
            tool_result_id = stored_tool_result["payload"]["tool_result_id"]

            loaded = store.get_tool_result(stored.id, tool_result_id)
            output_ref = loaded["output"]
            self.assertIn("blob_id", output_ref)
            self.assertEqual(output_ref["size_chars"], len(large_output))
            blob = store.get_blob(output_ref["blob_id"])
            self.assertEqual(blob["content"], large_output)

    def test_delete_run_removes_orphan_blob_references(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            large_output = "check output\n" + ("x" * 9000)
            result = RunResult(
                status="completed",
                data={},
                summaries={},
                events=[
                    RunEvent(
                        type="tool.result",
                        message="tool result",
                        payload={
                            "tool": "run_check",
                            "result": {"status": "completed", "output": large_output},
                        },
                    )
                ],
                estimated_tokens_used=0,
                agent_calls=0,
                tool_calls=1,
            )
            store = RunStore(Path(tmp) / ".coder")
            stored = store.save("workflow", "/repo", "check", result)
            loaded = store.get_tool_result(
                stored.id,
                store.get_events(stored.id)["events"][0]["payload"]["tool_result_id"],
            )
            blob_id = loaded["output"]["blob_id"]
            self.assertEqual(store.get_blob(blob_id)["content"], large_output)

            deletion = store.delete(stored.id)

            self.assertEqual(deletion["orphan_blobs_removed"], 1)
            self.assertEqual(store.list(), [])
            with self.assertRaises(KeyError):
                store.get_blob(blob_id)


class WorkflowPreflightTests(unittest.TestCase):
    def test_preflight_reports_unknown_tool_and_unreachable_node(self) -> None:
        workflow = WorkflowSpec.model_validate(
            {
                "id": "preflight-test",
                "name": "Preflight test",
                "nodes": [
                    {"id": "start", "type": "start"},
                    {"id": "tool", "type": "tool", "tool": "missing_tool"},
                    {"id": "orphan", "type": "tool", "tool": "project_index"},
                    {"id": "end", "type": "end"},
                ],
                "edges": [
                    {"from": "start", "to": "tool"},
                    {"from": "tool", "to": "end"},
                ],
            }
        )

        result = validate_workflow_preflight(workflow, registered_tools=["project_index"])

        self.assertEqual(result["status"], "error")
        codes = {issue["code"] for issue in result["issues"]}
        self.assertIn("unknown_tool", codes)
        self.assertIn("unreachable_node", codes)

    def test_preflight_checks_agent_tool_policy_and_returns_capability_summary(self) -> None:
        workflow = WorkflowSpec.model_validate(
            {
                "id": "preflight-policy-test",
                "name": "Preflight policy test",
                "agents": [
                    {
                        "id": "checker",
                        "role": "Checker",
                        "goal": "Run checks.",
                        "tools": ["run_check"],
                        "permissions": {
                            "read_files": True,
                            "edit_files": False,
                            "run_commands": False,
                            "use_network": False,
                            "requires_approval": True,
                        },
                    }
                ],
                "nodes": [
                    {"id": "start", "type": "start"},
                    {"id": "agent", "type": "agent", "agent_id": "checker"},
                    {"id": "check", "type": "tool", "tool": "run_check", "input": {"command": ""}},
                    {"id": "end", "type": "end"},
                ],
                "edges": [
                    {"from": "start", "to": "agent"},
                    {"from": "agent", "to": "check"},
                    {"from": "check", "to": "end"},
                ],
            }
        )
        registry = default_tool_registry()

        result = validate_workflow_preflight(
            workflow,
            registered_tools=registry.names(),
            tool_capabilities=registry.capabilities(),
        )

        codes = {issue["code"] for issue in result["issues"]}
        self.assertEqual(result["status"], "error")
        self.assertIn("agent_tool_permission_denied", codes)
        self.assertEqual(result["summary"]["tools"][0]["risk_level"], "high")
        self.assertEqual(result["summary"]["permission_summary"]["permissions"]["run_commands"], 1)

    def test_runner_rejects_unknown_tool_at_runtime(self) -> None:
        workflow = WorkflowSpec.model_validate(
            {
                "id": "runtime-tool-guard-test",
                "name": "Runtime tool guard test",
                "nodes": [
                    {"id": "start", "type": "start"},
                    {"id": "tool", "type": "tool", "tool": "missing_tool"},
                    {"id": "end", "type": "end"},
                ],
                "edges": [
                    {"from": "start", "to": "tool"},
                    {"from": "tool", "to": "end"},
                ],
            }
        )

        result = WorkflowRunner(workflow).run("run missing tool", "/repo")

        self.assertEqual(result.status, "failed")
        completed = [event for event in result.events if event.type == "node.completed" and event.node_id == "tool"]
        self.assertEqual(completed[0].payload["result_status"], "failed")
        self.assertTrue(any(event.type == "run.failed" for event in result.events))

    def test_runner_rejects_agent_tool_policy_before_agent_call(self) -> None:
        workflow = WorkflowSpec.model_validate(
            {
                "id": "runtime-agent-policy-test",
                "name": "Runtime agent policy test",
                "agents": [
                    {
                        "id": "checker",
                        "role": "Checker",
                        "goal": "Run checks.",
                        "tools": ["run_check"],
                        "permissions": {
                            "read_files": True,
                            "edit_files": False,
                            "run_commands": False,
                            "use_network": False,
                            "requires_approval": True,
                        },
                    }
                ],
                "nodes": [
                    {"id": "start", "type": "start"},
                    {"id": "agent", "type": "agent", "agent_id": "checker", "output_key": "agent_result"},
                    {"id": "end", "type": "end"},
                ],
                "edges": [
                    {"from": "start", "to": "agent"},
                    {"from": "agent", "to": "end"},
                ],
            }
        )
        executor = StaticExecutor({"status": "should_not_run"})

        result = WorkflowRunner(workflow, agent_executor=executor).run("run policy check", "/repo")

        self.assertEqual(result.status, "failed")
        self.assertNotIn("agent_result", result.data)
        completed = [event for event in result.events if event.type == "node.completed" and event.node_id == "agent"]
        self.assertEqual(completed[0].payload["result_status"], "failed")
        self.assertFalse(any(event.type == "agent.called" for event in result.events))


def _single_agent_workflow(artifact_type: str) -> WorkflowSpec:
    return WorkflowSpec.model_validate(
        {
            "id": "artifact-test",
            "name": "Artifact test",
            "nodes": [
                {"id": "start", "type": "start"},
                {"id": "agent", "type": "agent", "agent_id": "worker", "output_key": "agent_result"},
                {"id": "end", "type": "end"},
            ],
            "edges": [
                {"from": "start", "to": "agent"},
                {"from": "agent", "to": "end"},
            ],
            "agents": [
                {
                    "id": "worker",
                    "role": "Worker",
                    "goal": "Produce an artifact.",
                    "artifact_type": artifact_type,
                }
            ],
        }
    )


def _wait_for_status(run, status: str, timeout: float = 5.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if run.status == status:
            return
        time.sleep(0.02)
    raise AssertionError(f"timed out waiting for {status}, last status={run.status}")


if __name__ == "__main__":
    unittest.main()
