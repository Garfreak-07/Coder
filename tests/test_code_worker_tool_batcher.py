from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from coder_workbench.agent_graph.schema import AgentTaskEnvelope, WorkItem
from coder_workbench.agent_harness import CodeWorkerHarness, HarnessActionRequest, ToolBatcher
from coder_workbench.runtime_capabilities.registries import code_worker_tool_capabilities


class FakeResponse:
    def __init__(self, content: str) -> None:
        self.content = content


class FakeModel:
    def __init__(self, responses: list[str]) -> None:
        self.responses = list(responses)

    def invoke(self, prompt: str) -> FakeResponse:
        if len(self.responses) > 1:
            return FakeResponse(self.responses.pop(0))
        return FakeResponse(self.responses[0])


class CodeWorkerToolBatcherTests(unittest.TestCase):
    def test_read_and_search_batch_together(self) -> None:
        batches = ToolBatcher().partition(
            [
                _request("read", "read_file", {"path": "src/app.py"}),
                _request("search", "search_files", {"query": "needle"}),
            ]
        )

        self.assertEqual(len(batches), 1)
        self.assertEqual(batches[0].execution_mode, "concurrent")
        self.assertEqual([action.action_id for action in batches[0].actions], ["read", "search"])

    def test_diff_and_read_output_batch_together(self) -> None:
        batches = ToolBatcher().partition(
            [
                _request("diff", "inspect_git_diff", {}),
                _request("output", "read_tool_output", {"output_ref": "sha256:abc"}),
            ]
        )

        self.assertEqual(len(batches), 1)
        self.assertEqual(batches[0].execution_mode, "concurrent")

    def test_apply_patch_runs_exclusively(self) -> None:
        batches = ToolBatcher().partition(
            [
                _request("read", "read_file", {"path": "src/app.py"}),
                _request("patch", "apply_patch_sandbox", {"changes": []}),
                _request("search", "search_files", {"query": "needle"}),
            ]
        )

        self.assertEqual([batch.execution_mode for batch in batches], ["concurrent", "exclusive", "concurrent"])
        self.assertEqual(batches[1].actions[0].action_type, "apply_patch_sandbox")

    def test_run_command_runs_exclusively(self) -> None:
        batches = ToolBatcher().partition(
            [
                _request("diff", "inspect_git_diff", {}),
                _request("command", "run_command_sandbox", {"command": "python -m unittest"}),
            ]
        )

        self.assertEqual([batch.execution_mode for batch in batches], ["concurrent", "exclusive"])
        self.assertEqual(batches[1].actions[0].action_type, "run_command_sandbox")

    def test_batch_observations_keep_stable_order(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "src").mkdir()
            Path(tmp, "src", "app.py").write_text("needle = True\n", encoding="utf-8")
            record = _run_tool_loop(
                tmp,
                [
                    _batch(
                        [
                            _action_payload("search", "search_files", {"query": "needle", "paths": ["src"]}),
                            _action_payload("read", "read_file", {"path": "src/app.py"}),
                        ]
                    ),
                    _final("Batch completed."),
                ],
            )

        requested = record.artifact_payload["requested_actions"]
        self.assertEqual(record.status, "completed")
        self.assertEqual([action["action_id"] for action in requested], ["search", "read"])

    def test_failed_exclusive_action_stops_dependent_actions(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp, "repo")
            sandbox = Path(tmp, "sandbox")
            root.mkdir()
            sandbox.mkdir()
            Path(root, "src").mkdir()
            Path(root, "src", "app.py").write_text("value = 1\n", encoding="utf-8")
            record = _run_tool_loop(
                str(root),
                [
                    _batch(
                        [
                            _action_payload(
                                "command",
                                "run_command_sandbox",
                                {"argv": [sys.executable, "-c", "raise SystemExit(2)"]},
                            ),
                            _action_payload("read-after-command", "read_file", {"path": "src/app.py"}),
                        ]
                    ),
                    _final("Command failed."),
                ],
                sandbox_root=str(sandbox),
            )

        observations = record.artifact_payload["requested_actions"]
        skipped = [item for item in observations if item.get("action_id") == "read-after-command"]
        self.assertEqual(record.status, "blocked")
        self.assertEqual(skipped[0]["status"], "blocked")
        self.assertEqual(skipped[0]["error_code"], "skipped_after_failed_exclusive_action")


def _run_tool_loop(
    repo_root: str,
    responses: list[str],
    *,
    sandbox_root: str | None = None,
):
    with patch.dict(os.environ, {"CODER_ENABLE_CODE_WORKER_TOOL_LOOP": "1"}):
        return CodeWorkerHarness(model=FakeModel(responses)).create_execution_result(
            item=_item(),
            envelope=_envelope(),
            repo_root=repo_root,
            sandbox_root=sandbox_root,
            run_id="run",
            data={},
        )


def _request(action_id: str, action_type: str, payload: dict) -> HarnessActionRequest:
    return HarnessActionRequest(
        action_id=action_id,
        action_type=action_type,
        payload=payload,
        reason="test action",
        risk_level="low",
    )


def _action_payload(action_id: str, action_type: str, payload: dict) -> dict:
    return {
        "artifact_type": "harness_action",
        "action_id": action_id,
        "action_type": action_type,
        "payload": payload,
        "reason": "test action",
        "risk_level": "low",
    }


def _batch(actions: list[dict]) -> str:
    return json.dumps({"artifact_type": "harness_action_batch", "actions": actions})


def _final(summary: str) -> str:
    return json.dumps({"artifact_type": "execution_result", "status": "completed", "summary": summary})


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


if __name__ == "__main__":
    unittest.main()
