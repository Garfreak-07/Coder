from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from coder_workbench.harness_runtime import HarnessRuntimeContext, HarnessRuntimeManager, run_harness_dry_run


class HarnessDryRunTests(unittest.TestCase):
    def setUp(self) -> None:
        self._profile_env = _env("CODER_LLM_PROVIDER_PROFILE", None)
        self._profile_env.__enter__()

    def tearDown(self) -> None:
        self._profile_env.__exit__()

    def test_dry_run_does_not_invoke_openhands_conversation_or_prepare_sandbox(self) -> None:
        request = _request(input_artifacts={"requested_artifact_type": "planner_order"})

        with (
            _env("LLM_API_KEY", "test-key"),
            _env("DEEPSEEK_API_KEY", None),
            patch("coder_workbench.harness_runtime.dry_run.importlib.util.find_spec", return_value=object()),
            patch("coder_workbench.harness_runtime.openhands_provider.OpenHandsRuntimeProvider.run") as provider_run,
            patch("coder_workbench.harness_runtime.sandbox.prepare_sandbox_workspace") as prepare_sandbox,
        ):
            provider_run.side_effect = AssertionError("provider run must not be called")
            prepare_sandbox.side_effect = AssertionError("sandbox preparation must not be called")

            report = run_harness_dry_run(request)

        self.assertEqual(report.status, "ready")
        provider_run.assert_not_called()
        prepare_sandbox.assert_not_called()

    def test_valid_planner_order_dry_run_ready_when_imports_and_credentials_exist(self) -> None:
        request = _request(input_artifacts={"requested_artifact_type": "planner_order"})

        with (
            _env("LLM_API_KEY", "test-key"),
            _env("DEEPSEEK_API_KEY", None),
            patch("coder_workbench.harness_runtime.dry_run.importlib.util.find_spec", return_value=object()),
        ):
            report = run_harness_dry_run(request)

        self.assertEqual(report.status, "ready")
        self.assertEqual(report.artifact_target, "planner_order")

    def test_missing_credentials_blocks_openhands_dry_run(self) -> None:
        request = _request(input_artifacts={"requested_artifact_type": "planner_order"})

        with (
            _env("LLM_API_KEY", None),
            _env("DEEPSEEK_API_KEY", None),
            patch("coder_workbench.harness_runtime.dry_run.importlib.util.find_spec", return_value=object()),
        ):
            report = run_harness_dry_run(request)

        self.assertEqual(report.status, "blocked")
        check = _check(report, "llm_credentials")
        self.assertEqual(check.status, "blocked")
        payload = report.model_dump_json()
        self.assertIn("LLM_API_KEY", payload)
        self.assertIn("DEEPSEEK_API_KEY", payload)

    def test_invalid_artifact_target_blocks(self) -> None:
        request = _request(input_artifacts={"requested_artifact_type": "execution_result"})

        with (
            _env("LLM_API_KEY", "test-key"),
            patch("coder_workbench.harness_runtime.dry_run.importlib.util.find_spec", return_value=object()),
        ):
            report = run_harness_dry_run(request)

        self.assertEqual(report.status, "blocked")
        self.assertEqual(_check(report, "artifact_target").status, "blocked")

    def test_execution_sandbox_readiness_is_reported_without_repo_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as repo:
            repo_root = Path(repo)
            (repo_root / "src").mkdir()
            source = repo_root / "src" / "app.py"
            source.write_text("original\n", encoding="utf-8")
            request = _task_request(repo_root=str(repo_root), sandbox_root=None)

            with (
                _env("LLM_API_KEY", "test-key"),
                patch("coder_workbench.harness_runtime.dry_run.importlib.util.find_spec", return_value=object()),
            ):
                report = run_harness_dry_run(request)

            sandbox_check = _check(report, "sandbox_readiness")
            self.assertEqual(sandbox_check.status, "ready")
            self.assertEqual(source.read_text(encoding="utf-8"), "original\n")
            self.assertEqual(sorted(path.relative_to(repo_root).as_posix() for path in repo_root.rglob("*")), ["src", "src/app.py"])

    def test_permission_readiness_check_included(self) -> None:
        request = _task_request(repo_root=str(Path.cwd()), sandbox_root=None)

        with (
            _env("LLM_API_KEY", "test-key"),
            patch("coder_workbench.harness_runtime.dry_run.importlib.util.find_spec", return_value=object()),
        ):
            report = run_harness_dry_run(request)

        check = _check(report, "permission_readiness")
        self.assertEqual(check.status, "ready")
        self.assertTrue(check.metadata["expectations"]["planner_write_denied"])
        self.assertTrue(check.metadata["expectations"]["commit_push_deploy_denied"])

    def test_license_metadata_check_included(self) -> None:
        request = _request(input_artifacts={"requested_artifact_type": "planner_order"})

        with (
            _env("LLM_API_KEY", "test-key"),
            patch("coder_workbench.harness_runtime.dry_run.importlib.util.find_spec", return_value=object()),
        ):
            report = run_harness_dry_run(request)

        check = _check(report, "license_metadata")
        self.assertEqual(check.status, "ready")
        self.assertTrue(check.metadata["license_mit"])

    def test_secret_values_are_redacted_from_serialized_report(self) -> None:
        request = _request(input_artifacts={"requested_artifact_type": "planner_order"})

        with (
            _env("LLM_API_KEY", "test-secret-key-value"),
            _env("DEEPSEEK_API_KEY", None),
            patch("coder_workbench.harness_runtime.dry_run.importlib.util.find_spec", return_value=object()),
        ):
            report = run_harness_dry_run(request)

        payload = report.model_dump_json()
        self.assertNotIn("test-secret-key-value", payload)
        self.assertNotIn("credential_value", payload)

    def test_prompt_text_is_absent_from_serialized_report(self) -> None:
        request = _request(
            input_artifacts={
                "requested_artifact_type": "planner_order",
                "prompt": "FULL_PROMPT_MARKER",
            }
        )

        with (
            _env("LLM_API_KEY", "test-key"),
            patch("coder_workbench.harness_runtime.dry_run.importlib.util.find_spec", return_value=object()),
        ):
            report = run_harness_dry_run(request)

        payload = json.dumps(report.model_dump(mode="json"), sort_keys=True)
        self.assertNotIn("FULL_PROMPT_MARKER", payload)


class _env:
    def __init__(self, key: str, value: str | None) -> None:
        self.key = key
        self.value = value
        self.old = os.environ.get(key)

    def __enter__(self) -> None:
        if self.value is None:
            os.environ.pop(self.key, None)
        else:
            os.environ[self.key] = self.value

    def __exit__(self, *_args: object) -> None:
        if self.old is None:
            os.environ.pop(self.key, None)
        else:
            os.environ[self.key] = self.old


def _context() -> HarnessRuntimeContext:
    return HarnessRuntimeContext(
        run_id="run-1",
        agent_id="planner",
        workflow_id="workflow-1",
        harness_id="conversation-harness",
        mode="workflow_supervisor",
        profile_id="openhands-workflow-supervisor-default",
    )


def _request(input_artifacts: dict[str, object] | None = None):
    manager = HarnessRuntimeManager()
    return manager._request(
        request_id="request-1",
        contract_id="conversation-harness",
        mode="workflow_supervisor",
        profile_id="openhands-workflow-supervisor-default",
        context=_context(),
        input_artifacts=input_artifacts or {},
    )


def _task_request(*, repo_root: str, sandbox_root: str | None):
    manager = HarnessRuntimeManager()
    return manager._request(
        request_id="request-1",
        contract_id="task-execution-harness",
        mode="task_execution",
        profile_id="openhands-task-executor-default",
        context=HarnessRuntimeContext(
            run_id="run-1",
            agent_id="executor",
            workflow_id="workflow-1",
            harness_id="task-execution-harness",
            mode="task_execution",
            profile_id="openhands-task-executor-default",
            repo_root=repo_root,
            sandbox_root=sandbox_root,
        ),
        input_artifacts={"work_item_id": "work-1", "success_criteria": ["Return evidence."]},
    )


def _check(report, name: str):
    return next(check for check in report.checks if check.name == name)


if __name__ == "__main__":
    unittest.main()
