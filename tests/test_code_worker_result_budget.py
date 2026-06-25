from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from coder_workbench.agent_graph.schema import AgentTaskEnvelope, WorkItem
from coder_workbench.agent_harness import CodeWorkerHarness
from coder_workbench.runtime_capabilities.registries import code_worker_tool_capabilities


class FakeResponse:
    def __init__(self, content: str) -> None:
        self.content = content


class FakeModel:
    def __init__(self, responses: list[str]) -> None:
        self.responses = list(responses)
        self.prompts: list[str] = []

    def invoke(self, prompt: str) -> FakeResponse:
        self.prompts.append(prompt)
        if len(self.responses) > 1:
            return FakeResponse(self.responses.pop(0))
        return FakeResponse(self.responses[0])


class CodeWorkerResultBudgetTests(unittest.TestCase):
    def test_large_file_read_is_externalized(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "src").mkdir()
            huge = "a" * 20000
            Path(tmp, "src", "huge.txt").write_text(huge, encoding="utf-8")
            model = FakeModel([_action("read", "read_file", {"path": "src/huge.txt"}), _final("Read huge file.")])
            data: dict = {}
            record = _run(tmp, model, data=data)

        self.assertEqual(record.status, "completed")
        self.assertTrue(data["pending_blob_writes"])
        self.assertIn("sha256:", model.prompts[1])
        self.assertNotIn("x" * 10000, model.prompts[1])
        self.assertNotIn(huge, model.prompts[1])

    def test_large_search_result_is_externalized(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "src").mkdir()
            for index in range(80):
                Path(tmp, "src", f"file_{index}.txt").write_text("needle " + ("x" * 500), encoding="utf-8")
            model = FakeModel([_action("search", "search_files", {"query": "needle", "paths": ["src"], "max_results": 80}), _final("Searched.")])
            data: dict = {}
            record = _run(tmp, model, data=data)

        self.assertEqual(record.status, "completed")
        self.assertTrue(data["pending_blob_writes"])
        self.assertIn("sha256:", model.prompts[1])

    def test_large_git_diff_is_externalized(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            _git_repo(tmp)
            Path(tmp, "large.txt").write_text("changed\n" + ("x" * 20000), encoding="utf-8")
            model = FakeModel([_action("diff", "inspect_git_diff", {}), _final("Inspected diff.")])
            data: dict = {}
            record = _run(tmp, model, data=data)

        self.assertEqual(record.status, "completed")
        self.assertTrue(data["pending_blob_writes"])
        self.assertIn("sha256:", model.prompts[1])

    def test_large_command_output_is_externalized(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp, "repo")
            sandbox = Path(tmp, "sandbox")
            root.mkdir()
            sandbox.mkdir()
            argv = [sys.executable, "-c", "print('x' * 20000)"]
            model = FakeModel([_action("cmd", "run_command_sandbox", {"argv": argv}), _final("Ran command.")])
            data: dict = {}
            record = _run(str(root), model, sandbox_root=str(sandbox), data=data)

        self.assertEqual(record.status, "completed")
        self.assertTrue(data["pending_blob_writes"])
        self.assertIn("sha256:", model.prompts[1])

    def test_read_tool_output_returns_bounded_preview(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            data = {"tool_outputs": {"output-ref": "z" * 20000}}
            model = FakeModel([_action("read-output", "read_tool_output", {"output_ref": "output-ref"}), _final("Read output.")])
            record = _run(tmp, model, data=data)

        self.assertEqual(record.status, "completed")
        observation = record.artifact_payload["requested_actions"][0]
        self.assertEqual(observation["action_type"], "read_tool_output")
        self.assertIn("truncated", model.prompts[1])
        self.assertNotIn("z" * 10000, model.prompts[1])


def _run(
    repo_root: str,
    model: FakeModel,
    *,
    sandbox_root: str | None = None,
    data: dict | None = None,
):
    with patch.dict(os.environ, {"CODER_ENABLE_CODE_WORKER_TOOL_LOOP": "1"}):
        return CodeWorkerHarness(model=model).create_execution_result(
            item=_item(),
            envelope=_envelope(),
            repo_root=repo_root,
            sandbox_root=sandbox_root,
            run_id="run",
            data={} if data is None else data,
        )


def _git_repo(path: str) -> None:
    import subprocess

    subprocess.run(["git", "init"], cwd=path, check=True, capture_output=True, text=True)
    subprocess.run(["git", "config", "user.email", "test@example.com"], cwd=path, check=True)
    subprocess.run(["git", "config", "user.name", "Test"], cwd=path, check=True)
    Path(path, "large.txt").write_text("base\n", encoding="utf-8")
    subprocess.run(["git", "add", "large.txt"], cwd=path, check=True)
    subprocess.run(["git", "commit", "-m", "base"], cwd=path, check=True, capture_output=True, text=True)


def _item() -> WorkItem:
    return WorkItem(
        work_item_id="executor-work",
        merge_index=1,
        assignee_agent_id="executor",
        task_summary="Fix src/app.py.",
    )


def _envelope() -> AgentTaskEnvelope:
    return AgentTaskEnvelope(
        round=1,
        work_item_id="executor-work",
        merge_index=1,
        assigned_agent_id="executor",
        task_summary="Fix src/app.py.",
        planner_order_ref="planner_order_round_1",
        capability_set={"tools": [tool.model_dump(mode="json") for tool in code_worker_tool_capabilities()]},
    )


def _action(action_id: str, action_type: str, payload: dict) -> str:
    import json

    return json.dumps(
        {
            "artifact_type": "harness_action",
            "action_id": action_id,
            "action_type": action_type,
            "payload": payload,
            "reason": "test action",
            "risk_level": "low",
        }
    )


def _final(summary: str) -> str:
    import json

    return json.dumps({"artifact_type": "execution_result", "status": "completed", "summary": summary})


if __name__ == "__main__":
    unittest.main()
