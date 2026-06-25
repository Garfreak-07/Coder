from __future__ import annotations

import os
import re
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from coder_workbench.harness_runtime import (
    ArtifactProjector,
    HarnessRuntimeContext,
    HarnessRuntimeManager,
    NativeRuntimeStore,
    OpenHandsRuntimeProvider,
)
from coder_workbench.harness_runtime.profiles import OPENHANDS_PROVIDER_ID
from coder_workbench.harness_runtime.runtime_context import HarnessRunRequest, HarnessRunResult


class OpenHandsRuntimeProviderTests(unittest.TestCase):
    def test_openhands_provider_fails_closed_when_sdk_missing(self) -> None:
        provider = OpenHandsRuntimeProvider(runtime_module_names=("definitely_missing_openhands_sdk",))
        request = _request()

        self.assertFalse(provider.is_available())
        result = provider.run(request)

        self.assertEqual(result.status, "failed")
        self.assertEqual(result.error["code"], "openhands_sdk_unavailable")
        self.assertEqual(len(result.native_event_refs), 1)

    def test_manager_falls_back_when_openhands_flag_disabled(self) -> None:
        provider = _FakeOpenHandsProvider()
        manager = HarnessRuntimeManager(providers=[provider, _FakeFallbackProvider()])

        with _env("CODER_ENABLE_OPENHANDS_RUNTIME", None):
            result = manager.run_workflow_supervisor(context=_context())

        self.assertEqual(result.error["code"], "fallback_used")
        self.assertEqual(provider.calls, 0)

    def test_manager_falls_back_when_openhands_enabled_but_sdk_unavailable(self) -> None:
        provider = OpenHandsRuntimeProvider(runtime_module_names=("definitely_missing_openhands_sdk",))
        manager = HarnessRuntimeManager(providers=[provider, _FakeFallbackProvider()])

        with _env("CODER_ENABLE_OPENHANDS_RUNTIME", "1"):
            result = manager.run_workflow_supervisor(context=_context())

        self.assertEqual(result.error["code"], "fallback_used")

    def test_manager_uses_openhands_provider_when_flag_enabled_and_available(self) -> None:
        provider = _FakeOpenHandsProvider()
        manager = HarnessRuntimeManager(providers=[provider, _FakeFallbackProvider()])

        with _env("CODER_ENABLE_OPENHANDS_RUNTIME", "1"):
            result = manager.run_workflow_supervisor(context=_context())

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact_type, "final_report")
        self.assertEqual(provider.calls, 1)

    def test_openhands_provider_blocks_when_credentials_missing(self) -> None:
        store = NativeRuntimeStore()
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(native_store=store, sdk_loader=lambda: _fake_sdk(state))

        with _env("LLM_API_KEY", None), _env("DEEPSEEK_API_KEY", None):
            result = provider.run(_task_request(sandbox_root="F:\\sandbox"))

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.error["code"], "openhands_llm_credentials_missing")
        self.assertNotIn("conversation", state)
        self.assertEqual(
            [event.native_type for event in store.list_events("run-1")],
            ["provider.selected", "credentials.missing"],
        )

    def test_openhands_provider_invokes_fake_sdk_and_returns_execution_result(self) -> None:
        store = NativeRuntimeStore()
        state: dict[str, Any] = {}
        run_output = SimpleNamespace(
            summary="Fake OpenHands completed.",
            changed_files=["src/app.py"],
            diff_refs=["diff-ref"],
            log_refs=["log-ref"],
            evidence_refs=["runtime-ref"],
        )
        provider = OpenHandsRuntimeProvider(native_store=store, sdk_loader=lambda: _fake_sdk(state, run_output=run_output))

        with tempfile.TemporaryDirectory() as sandbox:
            with _env("LLM_API_KEY", "test-key"), _env("DEEPSEEK_API_KEY", None), _env("LLM_MODEL", "test-model"):
                result = provider.run(_task_request(sandbox_root=sandbox))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact_type, "execution_result")
        self.assertEqual(result.artifact["verification"]["status"], "pass")
        self.assertEqual(result.artifact["changed_files"], ["src/app.py"])
        self.assertEqual(result.artifact["patch_refs"], ["diff-ref"])
        self.assertEqual(result.diff_refs, ["diff-ref"])
        self.assertEqual(result.log_refs, ["log-ref"])
        self.assertIn("runtime-ref", result.evidence_refs)
        self.assertEqual(state["llm"]["model"], "test-model")
        self.assertEqual([tool.name for tool in state["agent"]["tools"]], ["terminal", "file_editor", "task_tracker"])
        self.assertIn("Do not ask the user", state["conversation"]["prompt"])
        self.assertEqual(
            [event.native_type for event in store.list_events("run-1")],
            ["provider.selected", "conversation.started", "conversation.completed"],
        )

        projected = ArtifactProjector().project(result)
        self.assertEqual(projected["artifact_type"], "execution_result")
        self.assertEqual(projected["status"], "completed")

    def test_openhands_provider_uses_task_tracker_only_for_conversation_modes(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(native_store=NativeRuntimeStore(), sdk_loader=lambda: _fake_sdk(state))

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request())

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact_type, "final_report")
        self.assertEqual([tool.name for tool in state["agent"]["tools"]], ["task_tracker"])
        self.assertIn("Do not write files or run commands", state["conversation"]["prompt"])

    def test_openhands_provider_records_failed_conversation(self) -> None:
        store = NativeRuntimeStore()
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=store,
            sdk_loader=lambda: _fake_sdk(state, run_error=RuntimeError("boom")),
        )

        with tempfile.TemporaryDirectory() as sandbox:
            with _env("LLM_API_KEY", "test-key"):
                result = provider.run(_task_request(sandbox_root=sandbox))

        self.assertEqual(result.status, "failed")
        self.assertEqual(result.error["code"], "openhands_run_failed")
        self.assertEqual(
            [event.native_type for event in store.list_events("run-1")],
            ["provider.selected", "conversation.started", "conversation.failed"],
        )

    def test_openhands_imports_stay_inside_provider(self) -> None:
        root = Path(__file__).resolve().parents[1] / "src" / "coder_workbench"
        offenders: list[str] = []
        pattern = re.compile(r"^\s*(from|import)\s+openhands\b", re.MULTILINE)
        for path in root.rglob("*.py"):
            if path.name == "openhands_provider.py":
                continue
            if pattern.search(path.read_text(encoding="utf-8")):
                offenders.append(str(path.relative_to(root)))

        self.assertEqual(offenders, [])


class _FakeOpenHandsProvider:
    provider_id = OPENHANDS_PROVIDER_ID

    def __init__(self) -> None:
        self.calls = 0

    def is_available(self) -> bool:
        return True

    def run(self, request: HarnessRunRequest, *, emit: Any | None = None) -> HarnessRunResult:
        self.calls += 1
        return HarnessRunResult(
            status="completed",
            artifact_type="final_report",
            artifact={"artifact_type": "final_report", "status": "completed", "summary": "ok"},
        )


class _FakeFallbackProvider:
    provider_id = "internal-fallback"

    def is_available(self) -> bool:
        return True

    def run(self, request: HarnessRunRequest, *, emit: Any | None = None) -> HarnessRunResult:
        return HarnessRunResult(status="failed", error={"code": "fallback_used", "message": "fallback"})


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


def _request() -> HarnessRunRequest:
    manager = HarnessRuntimeManager()
    return manager._request(
        request_id="request-1",
        contract_id="conversation-harness",
        mode="workflow_supervisor",
        profile_id="openhands-workflow-supervisor-default",
        context=_context(),
        input_artifacts={},
    )


def _task_request(*, sandbox_root: str | None) -> HarnessRunRequest:
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
            repo_root="F:\\repo",
            sandbox_root=sandbox_root,
            context_packet={
                "hot": {
                    "work_item": {"work_item_id": "work-1", "task_summary": "Do work."},
                    "task_envelope": {"work_item_id": "work-1", "task_summary": "Do work."},
                    "constraints": ["Stay in workspace."],
                }
            },
        ),
        input_artifacts={"work_item_id": "work-1", "success_criteria": ["Return evidence."]},
    )


def _fake_sdk(
    state: dict[str, Any],
    *,
    run_error: Exception | None = None,
    run_output: Any | None = None,
) -> Any:
    class FakeLLM:
        def __init__(self, **kwargs: Any) -> None:
            state["llm"] = kwargs

    class FakeTool:
        def __init__(self, *, name: str) -> None:
            self.name = name

    class FakeAgent:
        def __init__(self, *, llm: Any, tools: list[Any]) -> None:
            state["agent"] = {"llm": llm, "tools": tools}

    class FakeConversation:
        def __init__(self, *, agent: Any, workspace: str) -> None:
            state["conversation"] = {"agent": agent, "workspace": workspace}

        def send_message(self, prompt: str) -> None:
            state["conversation"]["prompt"] = prompt

        def run(self) -> Any:
            if run_error is not None:
                raise run_error
            state["conversation"]["ran"] = True
            return run_output or SimpleNamespace(summary="Fake OpenHands completed.")

    return SimpleNamespace(
        LLM=FakeLLM,
        Tool=FakeTool,
        Agent=FakeAgent,
        Conversation=FakeConversation,
        TerminalTool=SimpleNamespace(name="terminal"),
        FileEditorTool=SimpleNamespace(name="file_editor"),
        TaskTrackerTool=SimpleNamespace(name="task_tracker"),
    )


if __name__ == "__main__":
    unittest.main()
