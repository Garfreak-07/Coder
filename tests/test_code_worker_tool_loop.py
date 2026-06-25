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
        self.calls = 0

    def invoke(self, prompt: str) -> FakeResponse:
        self.calls += 1
        if len(self.responses) > 1:
            return FakeResponse(self.responses.pop(0))
        return FakeResponse(self.responses[0])


class CodeWorkerToolLoopTests(unittest.TestCase):
    def test_feature_flag_off_preserves_old_code_worker_behavior(self) -> None:
        model = FakeModel(['{"artifact_type":"execution_result","status":"completed","summary":"Old path."}'])
        with patch.dict(os.environ, {"CODER_ENABLE_CODE_WORKER_TOOL_LOOP": ""}):
            record = CodeWorkerHarness(model=model).create_execution_result(item=_item(), envelope=_envelope())

        self.assertEqual(record.status, "completed")
        self.assertEqual(record.execution_summary, "Old path.")

    def test_model_returns_read_file_action_and_runtime_records_observation(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "src").mkdir()
            Path(tmp, "src", "app.py").write_text("value = 1\n", encoding="utf-8")
            record = _run_tool_loop(
                tmp,
                [
                    _action("step-1", "read_file", {"path": "src/app.py"}),
                    _final("Read file."),
                ],
            )

        self.assertEqual(record.status, "completed")
        self.assertEqual(record.artifact_payload["requested_actions"][0]["action_type"], "read_file")
        self.assertTrue(record.artifact_payload["evidence_refs"])

    def test_model_returns_search_files_action_and_runtime_records_observation(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "src").mkdir()
            Path(tmp, "src", "app.py").write_text("needle = True\n", encoding="utf-8")
            record = _run_tool_loop(
                tmp,
                [
                    _action("step-1", "search_files", {"query": "needle", "paths": ["src"]}),
                    _final("Searched files."),
                ],
            )

        self.assertEqual(record.status, "completed")
        self.assertEqual(record.artifact_payload["requested_actions"][0]["action_type"], "search_files")

    def test_unknown_action_is_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            record = _run_tool_loop(tmp, [_action("step-1", "unknown_tool", {})])

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "tool_unavailable")

    def test_denied_action_is_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            record = _run_tool_loop(tmp, [_action("step-1", "ask_user", {"question": "Continue?"})])

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "permission_boundary")

    def test_path_escape_is_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            record = _run_tool_loop(tmp, [_action("step-1", "read_file", {"path": "../outside.txt"})])

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "scope_violation")

    def test_out_of_scope_file_read_is_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "src").mkdir()
            Path(tmp, "docs").mkdir()
            Path(tmp, "docs", "note.md").write_text("note\n", encoding="utf-8")
            record = _run_tool_loop(tmp, [_action("step-1", "read_file", {"path": "docs/note.md"})], scopes=["src"])

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "scope_violation")

    def test_propose_patch_uses_patch_service_preview(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "src").mkdir()
            Path(tmp, "src", "app.py").write_text("value = 1\n", encoding="utf-8")
            record = _run_tool_loop(
                tmp,
                [
                    _action(
                        "step-1",
                        "propose_patch",
                        {"changes": [{"path": "src/app.py", "action": "update", "content": "value = 2\n"}]},
                    ),
                    _final("Previewed patch."),
                ],
            )

        self.assertEqual(record.status, "completed")
        self.assertEqual(record.artifact_payload["requested_actions"][0]["action_type"], "propose_patch")
        self.assertTrue(record.artifact_payload["patch_refs"])

    def test_apply_patch_sandbox_uses_gateway_and_records_patch_refs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp, "repo")
            sandbox = Path(tmp, "sandbox")
            root.mkdir()
            sandbox.mkdir()
            Path(root, "sample.py").write_text("value = 1\n", encoding="utf-8")
            Path(sandbox, "sample.py").write_text("value = 1\n", encoding="utf-8")
            record = _run_tool_loop(
                str(root),
                [
                    _action(
                        "step-1",
                        "apply_patch_sandbox",
                        {"changes": [{"path": "sample.py", "action": "update", "content": "value = 2\n"}]},
                    ),
                    _final("Applied sandbox patch."),
                ],
                sandbox_root=str(sandbox),
            )

            self.assertEqual(Path(root, "sample.py").read_text(encoding="utf-8"), "value = 1\n")
            self.assertEqual(Path(sandbox, "sample.py").read_text(encoding="utf-8"), "value = 2\n")
        self.assertEqual(record.status, "completed")
        self.assertTrue(record.artifact_payload["patch_refs"])

    def test_risk_path_patch_is_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            record = _run_tool_loop(
                tmp,
                [
                    _action(
                        "step-1",
                        "propose_patch",
                        {"changes": [{"path": ".env", "action": "create", "content": "SECRET=x\n"}]},
                    )
                ],
            )

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "risk_path_blocked")

    def test_run_command_sandbox_records_command_check(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp, "repo")
            sandbox = Path(tmp, "sandbox")
            root.mkdir()
            sandbox.mkdir()
            command = f'"{sys.executable}" -c "print(123)"'
            record = _run_tool_loop(
                str(root),
                [_action("step-1", "run_command_sandbox", {"command": command}), _final("Ran check.")],
                sandbox_root=str(sandbox),
            )

        checks = record.artifact_payload["verification"]["checks_run"]
        self.assertEqual(record.status, "completed")
        self.assertEqual(checks[0]["status"], "pass")

    def test_high_risk_command_is_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            record = _run_tool_loop(
                tmp,
                [_action("step-1", "run_command_sandbox", {"command": "curl https://example.com"}, risk_level="high")],
            )

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "risk_path_blocked")

    def test_command_failure_observation_returns_to_model(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp, "repo")
            sandbox = Path(tmp, "sandbox")
            root.mkdir()
            sandbox.mkdir()
            command = f'"{sys.executable}" -c "raise SystemExit(2)"'
            model = FakeModel([
                _action("step-1", "run_command_sandbox", {"command": command}),
                _final("Command failed."),
            ])
            record = _run_tool_loop(str(root), model.responses, sandbox_root=str(sandbox), model=model)

        self.assertEqual(model.calls, 4)
        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["verification"]["checks_run"][0]["status"], "fail")

    def test_model_can_repair_after_command_failure(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp, "repo")
            sandbox = Path(tmp, "sandbox")
            root.mkdir()
            sandbox.mkdir()
            fail = f'"{sys.executable}" -c "raise SystemExit(2)"'
            ok = f'"{sys.executable}" -c "print(1)"'
            record = _run_tool_loop(
                str(root),
                [
                    _action("step-1", "run_command_sandbox", {"command": fail}),
                    _action("step-2", "run_command_sandbox", {"command": ok}),
                    _final("Repaired check."),
                ],
                sandbox_root=str(sandbox),
            )

        self.assertEqual(record.status, "completed")
        self.assertEqual(record.artifact_payload["verification"]["checks_run"][-1]["status"], "pass")

    def test_final_execution_result_is_enriched_from_session_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "src").mkdir()
            Path(tmp, "src", "app.py").write_text("value = 1\n", encoding="utf-8")
            record = _run_tool_loop(tmp, [_action("step-1", "read_file", {"path": "src/app.py"}), _final("Done.")])

        self.assertTrue(record.artifact_payload["evidence_refs"])
        self.assertEqual(record.artifact_payload["verification"]["evidence_refs"], record.artifact_payload["evidence_refs"])

    def test_model_provided_fake_changed_files_are_ignored(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "src").mkdir()
            Path(tmp, "src", "app.py").write_text("value = 1\n", encoding="utf-8")
            record = _run_tool_loop(
                tmp,
                [
                    _action("step-1", "read_file", {"path": "src/app.py"}),
                    '{"artifact_type":"execution_result","status":"completed","summary":"Done.","changed_files":["fake.py"]}',
                ],
            )

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "schema_validation_failed")

    def test_invalid_model_json_triggers_one_correction(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            model = FakeModel(["not json", _final_noop("Corrected.")])
            record = _run_tool_loop(tmp, model.responses, model=model)

        self.assertEqual(model.calls, 2)
        self.assertEqual(record.status, "completed")

    def test_invalid_final_artifact_triggers_repair_pipeline(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            record = _run_tool_loop(tmp, ['{"artifact_type":"execution_result","status":"completed","summary":"Done."}'])

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "schema_validation_failed")

    def test_self_check_blocks_mismatched_work_item_id(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            record = _run_tool_loop(
                tmp,
                ['{"artifact_type":"execution_result","work_item_id":"other","status":"completed","summary":"Wrong item."}'],
            )

        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "schema_validation_failed")

    def test_max_turns_returns_blocked_execution_result(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "src").mkdir()
            Path(tmp, "src", "app.py").write_text("value = 1\n", encoding="utf-8")
            model = FakeModel([_action("step-repeat", "read_file", {"path": "src/app.py"})])
            record = _run_tool_loop(tmp, model.responses, model=model)

        self.assertEqual(model.calls, 16)
        self.assertEqual(record.status, "blocked")
        self.assertEqual(record.artifact_payload["blocker_type"], "timeout")


def _run_tool_loop(
    repo_root: str,
    responses: list[str],
    *,
    model: FakeModel | None = None,
    scopes: list[str] | None = None,
    sandbox_root: str | None = None,
):
    active_model = model or FakeModel(responses)
    with patch.dict(os.environ, {"CODER_ENABLE_CODE_WORKER_TOOL_LOOP": "1"}):
        return CodeWorkerHarness(model=active_model).create_execution_result(
            item=_item(),
            envelope=_envelope(),
            repo_root=repo_root,
            sandbox_root=sandbox_root,
            scopes=scopes,
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


def _action(action_id: str, action_type: str, payload: dict, *, risk_level: str = "low") -> str:
    return (
        '{"artifact_type":"harness_action",'
        f'"action_id":"{action_id}",'
        f'"action_type":"{action_type}",'
        f'"payload":{_json(payload)},'
        '"reason":"test action",'
        f'"risk_level":"{risk_level}"'
        "}"
    )


def _final(summary: str) -> str:
    return '{"artifact_type":"execution_result","status":"completed","summary":' + _json(summary) + "}"


def _final_noop(summary: str) -> str:
    return (
        '{"artifact_type":"execution_result","status":"completed","summary":'
        + _json(summary)
        + ',"no_op_rationale":"No runtime action was needed for this test."}'
    )


def _json(value) -> str:
    import json

    return json.dumps(value)


if __name__ == "__main__":
    unittest.main()
