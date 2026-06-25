from __future__ import annotations

import os
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


class CodeWorkerStopGateTests(unittest.TestCase):
    def test_completed_without_evidence_triggers_stop_gate_retry(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            model = FakeModel([
                _final("Too early."),
                _action("step-1", "read_file", {"path": "src/app.py"}),
                _final("Done with evidence."),
            ])
            Path(tmp, "src").mkdir()
            Path(tmp, "src", "app.py").write_text("value = 1\n", encoding="utf-8")
            record = _run(tmp, model)

        self.assertEqual(record.status, "completed")
        self.assertEqual(len(model.prompts), 3)
        self.assertIn("stop_gate_failed", model.prompts[1])

    def test_wrong_work_item_id_blocks_immediately(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            record = _run(
                tmp,
                FakeModel(['{"artifact_type":"execution_result","work_item_id":"other","status":"completed","summary":"Wrong."}']),
            )

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "schema_validation_failed")

    def test_wrong_agent_id_blocks_immediately(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            record = _run(
                tmp,
                FakeModel(['{"artifact_type":"execution_result","agent_id":"other","status":"completed","summary":"Wrong."}']),
            )

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "schema_validation_failed")

    def test_planner_decision_field_blocks_immediately(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            record = _run(
                tmp,
                FakeModel(['{"artifact_type":"execution_result","status":"completed","summary":"Bad.","planner_decision":{}}']),
            )

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "permission_boundary")

    def test_planner_decision_artifact_blocks_immediately(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            record = _run(
                tmp,
                FakeModel(['{"artifact_type":"planner_decision","task_done":true,"next_action":"finish","reason":"bad"}']),
            )

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "permission_boundary")

    def test_final_report_field_blocks_immediately(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            record = _run(
                tmp,
                FakeModel(['{"artifact_type":"execution_result","status":"completed","summary":"Bad.","final_report":{}}']),
            )

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "permission_boundary")

    def test_human_message_field_blocks_immediately(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            record = _run(
                tmp,
                FakeModel(['{"artifact_type":"execution_result","status":"completed","summary":"Bad.","human_message":"ask"}']),
            )

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "permission_boundary")

    def test_failed_command_cannot_finalize_completed(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp, "repo")
            sandbox = Path(tmp, "sandbox")
            root.mkdir()
            sandbox.mkdir()
            command = 'python -c "raise SystemExit(2)"'
            record = _run(
                str(root),
                FakeModel([
                    _action("step-1", "run_command_sandbox", {"command": command}),
                    _final("Ignored failure."),
                    _final("Still ignored."),
                    _final("Still ignored."),
                ]),
                sandbox_root=str(sandbox),
            )

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "command_failed")

    def test_stop_gate_retry_limited_to_two_attempts(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            model = FakeModel([_final("Too early.")])
            record = _run(tmp, model)

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "schema_validation_failed")
        self.assertEqual(len(model.prompts), 3)


def _run(repo_root: str, model: FakeModel, *, sandbox_root: str | None = None):
    with patch.dict(os.environ, {"CODER_ENABLE_CODE_WORKER_TOOL_LOOP": "1"}):
        return CodeWorkerHarness(model=model).create_execution_result(
            item=_item(),
            envelope=_envelope(),
            repo_root=repo_root,
            sandbox_root=sandbox_root,
            run_id="run",
            data={},
        )


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
