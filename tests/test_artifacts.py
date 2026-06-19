from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from typing import Any

from coder_workbench.core import WorkflowSpec
from coder_workbench.core.preflight import validate_workflow_preflight
from coder_workbench.runtime import RunEvent, RunResult
from coder_workbench.runtime.runner import WorkflowRunner
from coder_workbench.server.storage import RunStore


class StaticExecutor:
    def __init__(self, result: dict[str, Any]) -> None:
        self.result = result

    def run(self, agent, context: dict[str, Any]) -> dict[str, Any]:
        return dict(self.result)


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
            self.assertEqual(result.artifacts, {})
            self.assertTrue(any(event.type == "artifact.validation_failed" for event in result.events))


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


if __name__ == "__main__":
    unittest.main()
