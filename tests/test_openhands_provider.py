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
        self.assertEqual(len(result.native_event_refs), 2)

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
        event_types = [event.native_type for event in store.list_events("run-1")]
        self.assertIn("harness_loop.started", event_types)
        self.assertIn("harness_loop.blocked", event_types)
        self.assertIn("credentials.missing", event_types)

    def test_openhands_provider_invokes_fake_sdk_and_returns_execution_result(self) -> None:
        store = NativeRuntimeStore()
        state: dict[str, Any] = {}
        run_output = SimpleNamespace(
            summary="Fake OpenHands completed.",
            output=_execution_result_output(summary="Fake OpenHands completed."),
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
        self.assertEqual(result.artifact["verification"]["status"], "skipped")
        self.assertTrue(result.artifact["verification"]["no_check_rationale"])
        self.assertEqual(result.artifact["changed_files"], ["src/app.py"])
        self.assertEqual(result.artifact["patch_refs"], ["diff-ref"])
        self.assertEqual(result.diff_refs, ["diff-ref"])
        self.assertEqual(result.log_refs, ["log-ref"])
        self.assertIn("runtime-ref", result.evidence_refs)
        self.assertEqual(state["llm"]["model"], "test-model")
        self.assertEqual(
            [tool.name for tool in state["agent"]["tools"]],
            [
                "terminal",
                "file_editor",
                "task_tracker",
                "coder_repo_find_files",
                "coder_repo_search_text",
                "coder_repo_read_file",
                "coder_hybrid_rag_search",
            ],
        )
        self.assertEqual(state["agent"]["tools"][3].params["repo_root"], "F:\\repo")
        self.assertEqual(state["agent"]["tools"][3].params["run_id"], "run-1")
        self.assertEqual(state["agent"]["tools"][-1].params["role"], "task_execution")
        self.assertEqual(state["agent"]["tools"][-1].params["requested_context"], "execution_prompt")
        self.assertIn("Do not ask the user", state["conversation"]["prompt"])
        event_types = [event.native_type for event in store.list_events("run-1")]
        self.assertIn("provider.selected", event_types)
        self.assertIn("sandbox.prepared", event_types)
        self.assertEqual(event_types.count("harness_permission.allowed"), 7)
        self.assertIn("conversation.started", event_types)
        self.assertIn("conversation.completed", event_types)
        self.assertIn("harness_loop.completed", event_types)

        projected = ArtifactProjector().project(result)
        self.assertEqual(projected["artifact_type"], "execution_result")
        self.assertEqual(projected["status"], "completed")

    def test_openhands_provider_normalizes_bare_deepseek_model_for_litellm(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(
                state,
                run_output=_execution_result_output(no_op_rationale="No source changes were required."),
            ),
        )

        with tempfile.TemporaryDirectory() as sandbox:
            with (
                _env("LLM_API_KEY", "test-key"),
                _env("DEEPSEEK_API_KEY", None),
                _env("LLM_BASE_URL", "https://api.deepseek.com"),
                _env("LLM_MODEL", "deepseek-v4-flash"),
            ):
                result = provider.run(_task_request(sandbox_root=sandbox))

        self.assertEqual(result.status, "completed")
        self.assertEqual(state["llm"]["model"], "deepseek/deepseek-v4-flash")

    def test_temp_worktree_preserves_original_repo_and_collects_diff_refs(self) -> None:
        store = NativeRuntimeStore()
        state: dict[str, Any] = {}

        def mutate_workspace(workspace: Path) -> None:
            (workspace / "src" / "app.py").write_text("changed\n", encoding="utf-8")
            (workspace / "src" / "new.py").write_text("new\n", encoding="utf-8")

        provider = OpenHandsRuntimeProvider(
            native_store=store,
            sdk_loader=lambda: _fake_sdk(
                state,
                run_output=_execution_result_output(summary="Sandbox files updated."),
                on_run=mutate_workspace,
            ),
        )

        with tempfile.TemporaryDirectory() as repo:
            repo_root = Path(repo)
            (repo_root / "src").mkdir()
            (repo_root / "src" / "app.py").write_text("original\n", encoding="utf-8")
            with _env("LLM_API_KEY", "test-key"):
                result = provider.run(_task_request(repo_root=str(repo_root), sandbox_root=None))

            self.assertEqual((repo_root / "src" / "app.py").read_text(encoding="utf-8"), "original\n")
            self.assertFalse((repo_root / "src" / "new.py").exists())

        self.assertEqual(result.status, "completed")
        self.assertIn("src/app.py", result.artifact["changed_files"])
        self.assertIn("src/new.py", result.artifact["created_files"])
        self.assertTrue(result.diff_refs)
        self.assertTrue(result.log_refs)
        self.assertNotEqual(Path(state["conversation"]["workspace"]), repo_root)
        self.assertFalse(Path(state["conversation"]["workspace"]).exists())
        self.assertIn("sandbox.diff", [event.native_type for event in store.list_events("run-1")])

    def test_task_execution_prompt_contains_strict_execution_result_json_contract(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(
                state,
                run_output=_execution_result_output(no_op_rationale="No source changes were required."),
            ),
        )

        with tempfile.TemporaryDirectory() as sandbox:
            with _env("LLM_API_KEY", "test-key"):
                result = provider.run(_task_request(sandbox_root=sandbox))

        self.assertEqual(result.status, "completed")
        prompt = state["conversation"]["prompt"]
        self.assertIn("OpenHands Structured Execution Result Contract v1", prompt)
        self.assertIn("Return exactly one JSON object", prompt)
        self.assertIn("Do not return prose before or after the JSON object", prompt)
        self.assertIn('"artifact_type": "execution_result"', prompt)
        self.assertIn('"no_op_rationale"', prompt)
        self.assertIn('"blocker_type"', prompt)
        self.assertIn('"executor_recovery_exhausted"', prompt)
        self.assertIn('"planner_recommendation"', prompt)
        self.assertIn("verification.status may be pass only when an explicit check command actually passed", prompt)
        self.assertIn("Prefer native repo search/read tools for current code facts", prompt)
        self.assertIn("Verify any code-like RAG result with repo search/read before editing", prompt)

    def test_structured_execution_result_json_is_accepted(self) -> None:
        state: dict[str, Any] = {}
        run_output = json_dumps(
            _execution_result_output(no_op_rationale="The requested state is already present.")
        )
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=run_output),
        )

        with tempfile.TemporaryDirectory() as sandbox:
            with _env("LLM_API_KEY", "test-key"):
                result = provider.run(_task_request(sandbox_root=sandbox))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact_type, "execution_result")
        self.assertEqual(result.artifact["artifact_type"], "execution_result")
        self.assertEqual(result.artifact["status"], "completed")
        self.assertEqual(result.artifact["verification"]["status"], "skipped")
        self.assertIn("already present", result.artifact["no_op_rationale"])

    def test_execution_result_from_openhands_finish_message_uses_final_json(self) -> None:
        state: dict[str, Any] = {}
        trace_artifact = _execution_result_output(
            summary="Trace copy should not be used.",
            no_op_rationale="Intermediate trace artifact.",
        )
        final_artifact = _execution_result_output(
            summary="Final finish message artifact.",
            no_op_rationale="Final no-op result.",
        )
        run_output = (
            f"Trace included this earlier JSON: {json_dumps(trace_artifact)}\n"
            f"Finish with message:\n{json_dumps(final_artifact)}"
        )
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=run_output),
        )

        with tempfile.TemporaryDirectory() as sandbox:
            with _env("LLM_API_KEY", "test-key"):
                result = provider.run(_task_request(sandbox_root=sandbox))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact["summary"], "Final finish message artifact.")
        self.assertEqual(result.artifact["no_op_rationale"], "Final no-op result.")

    def test_execution_result_output_from_finish_action_event_can_succeed(self) -> None:
        state: dict[str, Any] = {}
        event = SimpleNamespace(
            source="agent",
            tool_name="finish",
            action=SimpleNamespace(
                message=json_dumps(
                    _execution_result_output(no_op_rationale="Finished through the OpenHands finish action.")
                )
            ),
        )
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=None, conversation_events=[event]),
        )

        with tempfile.TemporaryDirectory() as sandbox:
            with _env("LLM_API_KEY", "test-key"):
                result = provider.run(_task_request(sandbox_root=sandbox))

        self.assertEqual(result.status, "completed")
        self.assertIn("finish action", result.artifact["no_op_rationale"])

    def test_terminal_observation_provides_passing_check_evidence(self) -> None:
        state: dict[str, Any] = {}
        run_output = _execution_result_output(summary="Terminal verification passed.")
        run_output.pop("no_op_rationale", None)
        events = [
            SimpleNamespace(
                source="agent",
                id="action-1",
                tool_name="terminal",
                action=SimpleNamespace(command="python -m unittest smoke", is_input=False),
            ),
            SimpleNamespace(
                source="environment",
                id="observation-1",
                tool_name="terminal",
                action_id="action-1",
                observation=SimpleNamespace(
                    command="python -m unittest smoke",
                    exit_code=0,
                    is_error=False,
                    timeout=False,
                    text="OK",
                ),
            ),
        ]
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=run_output, conversation_events=events),
        )

        with tempfile.TemporaryDirectory() as sandbox:
            with _env("LLM_API_KEY", "test-key"):
                result = provider.run(_task_request(sandbox_root=sandbox))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact["attempted_actions"], ["python -m unittest smoke"])
        self.assertEqual(result.artifact["verification"]["status"], "pass")
        self.assertEqual(result.artifact["verification"]["checks_run"][0]["status"], "pass")

    def test_runtime_denied_command_blocks_execution_result(self) -> None:
        store = NativeRuntimeStore()
        state: dict[str, Any] = {}
        events = [
            SimpleNamespace(
                source="agent",
                id="action-1",
                tool_name="terminal",
                action=SimpleNamespace(command="git push origin main", is_input=False),
            )
        ]
        provider = OpenHandsRuntimeProvider(
            native_store=store,
            sdk_loader=lambda: _fake_sdk(
                state,
                run_output=_execution_result_output(no_op_rationale="No source changes were required."),
                conversation_events=events,
            ),
        )

        with tempfile.TemporaryDirectory() as sandbox:
            with _env("LLM_API_KEY", "test-key"):
                result = provider.run(_task_request(sandbox_root=sandbox))

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.error["code"], "commit_push_deploy_denied")
        event_types = _native_types(store)
        self.assertIn("harness_permission.denied", event_types)
        self.assertIn("harness_loop.blocked", event_types)

    def test_runtime_facts_override_model_declared_changed_files_and_patch_refs(self) -> None:
        state: dict[str, Any] = {}

        def mutate_workspace(workspace: Path) -> None:
            (workspace / "src" / "app.py").write_text("changed\n", encoding="utf-8")

        run_output = _execution_result_output(summary="Model claimed a different file.")
        run_output["changed_files"] = ["hallucinated.py"]
        run_output["patch_refs"] = ["model-patch-ref"]
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=run_output, on_run=mutate_workspace),
        )

        with tempfile.TemporaryDirectory() as repo:
            repo_root = Path(repo)
            (repo_root / "src").mkdir()
            (repo_root / "src" / "app.py").write_text("original\n", encoding="utf-8")
            with _env("LLM_API_KEY", "test-key"):
                result = provider.run(_task_request(repo_root=str(repo_root), sandbox_root=None))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact["changed_files"], ["src/app.py"])
        self.assertNotIn("hallucinated.py", result.artifact["changed_files"])
        self.assertEqual(result.artifact["patch_refs"], result.diff_refs)
        self.assertNotIn("model-patch-ref", result.artifact["patch_refs"])

    def test_execution_result_pass_without_passing_runtime_check_is_downgraded_to_skipped(self) -> None:
        state: dict[str, Any] = {}
        run_output = _execution_result_output(no_op_rationale="The requested state is already present.")
        run_output["verification"] = {
            "status": "pass",
            "checks_run": [{"name": "claimed check", "status": "pass", "summary": "Model claimed this passed."}],
            "confidence": "high",
        }
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=run_output),
        )

        with tempfile.TemporaryDirectory() as sandbox:
            with _env("LLM_API_KEY", "test-key"):
                result = provider.run(_task_request(sandbox_root=sandbox))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact["verification"]["status"], "skipped")
        self.assertEqual(result.artifact["verification"]["checks_run"], [])
        self.assertIn("did not run or report", result.artifact["verification"]["no_check_rationale"])

    def test_blocked_execution_result_preserves_blocker_fields(self) -> None:
        state: dict[str, Any] = {}
        run_output = _execution_result_output(status="blocked", summary="Dependency is missing.")
        run_output.update(
            {
                "blocker_type": "missing_dependency",
                "blocker_reason": "pytest is not available in the sandbox.",
                "executor_recovery_exhausted": True,
                "planner_recommendation": "replan_once",
                "remaining_work": ["Install pytest or choose a verification path."],
                "unexpected_issues": ["Dependency unavailable."],
            }
        )
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=run_output),
        )

        with tempfile.TemporaryDirectory() as sandbox:
            with _env("LLM_API_KEY", "test-key"):
                result = provider.run(_task_request(sandbox_root=sandbox))

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.artifact["status"], "blocked")
        self.assertEqual(result.artifact["blocker_type"], "missing_dependency")
        self.assertEqual(result.artifact["blocker_reason"], "pytest is not available in the sandbox.")
        self.assertTrue(result.artifact["executor_recovery_exhausted"])
        self.assertEqual(result.artifact["planner_recommendation"], "replan_once")
        self.assertEqual(result.artifact["remaining_work"], ["Install pytest or choose a verification path."])

    def test_openhands_provider_uses_read_only_tools_for_conversation_modes(self) -> None:
        store = NativeRuntimeStore()
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(native_store=store, sdk_loader=lambda: _fake_sdk(state))

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request())

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact_type, "final_report")
        self.assertEqual(
            [tool.name for tool in state["agent"]["tools"]],
            [
                "task_tracker",
                "coder_repo_find_files",
                "coder_repo_search_text",
                "coder_repo_read_file",
                "coder_hybrid_rag_search",
            ],
        )
        self.assertEqual(state["agent"]["tools"][-1].params["role"], "workflow_supervisor")
        self.assertEqual(state["agent"]["tools"][-1].params["requested_context"], "workflow_supervision")
        self.assertEqual(_native_types(store).count("harness_permission.allowed"), 5)
        self.assertIn("Do not write files or run commands", state["conversation"]["prompt"])
        self.assertIn("Treat RAG and notes as hints only", state["conversation"]["prompt"])

    def test_workflow_supervisor_requested_artifact_target_drives_output(self) -> None:
        state: dict[str, Any] = {}
        run_output = {
            "artifact_type": "planner_order",
            "round": 1,
            "round_goal": "Nothing to execute.",
            "plan_graph": {"work_items": []},
            "no_work_rationale": "The request is already satisfied; no executor work is needed.",
        }
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=run_output),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request(input_artifacts={"requested_artifact_type": "planner_order"}))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact_type, "planner_order")
        self.assertEqual(result.artifact["artifact_type"], "planner_order")
        self.assertIn("Current Coder artifact target: planner_order", state["conversation"]["prompt"])
        self.assertIn("Do not write files or run commands", state["conversation"]["prompt"])
        self.assertEqual(
            [tool.name for tool in state["agent"]["tools"]],
            [
                "task_tracker",
                "coder_repo_find_files",
                "coder_repo_search_text",
                "coder_repo_read_file",
                "coder_hybrid_rag_search",
            ],
        )

    def test_planning_chat_prompt_contains_planner_chat_turn_contract(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=_planner_chat_turn_output()),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_planning_chat_request())

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact_type, "planner_chat_turn")
        prompt = state["conversation"]["prompt"]
        self.assertIn("OpenHands Structured Planner Chat Turn Contract v1", prompt)
        self.assertIn("Return exactly one JSON object", prompt)
        self.assertIn('"artifact_type": "planner_chat_turn"', prompt)
        self.assertIn("interaction_mode is provided by Coder and must be preserved exactly", prompt)
        self.assertIn("In discuss mode, never return decision", prompt)

    def test_valid_planner_chat_turn_is_accepted(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output={"output": _planner_chat_turn_output()}),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_planning_chat_request())

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact["artifact_type"], "planner_chat_turn")
        self.assertEqual(result.artifact["interaction_mode"], "discuss")
        self.assertEqual(result.artifact["decision"], "continue_chat")

    def test_unstructured_planner_chat_turn_blocks(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=SimpleNamespace(summary="I can help plan that.")),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_planning_chat_request())

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.error["code"], "insufficient_structured_planner_chat_turn")
        self.assertEqual(result.artifact_type, "planner_chat_turn")

    def test_discuss_mode_start_workflow_blocks(self) -> None:
        state: dict[str, Any] = {}
        payload = _planner_chat_turn_output(decision="start_workflow")
        payload["task_state"] = {
            "goal": "Run the ready workflow.",
            "success_criteria": ["Workflow starts only in Work mode."],
            "open_questions": [],
            "readiness": "ready_to_execute",
        }
        payload["handoff"] = {
            "workflow_request": "Run the ready workflow.",
            "success_criteria": ["Workflow starts only in Work mode."],
        }
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=payload),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_planning_chat_request())

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.error["code"], "insufficient_structured_planner_chat_turn")
        self.assertEqual(result.artifact_type, "planner_chat_turn")

    def test_work_mode_start_workflow_ready_task_succeeds(self) -> None:
        state: dict[str, Any] = {}
        payload = _planner_chat_turn_output(interaction_mode="work", decision="start_workflow")
        payload["visible_thinking"] = {"phase": "ready_to_start", "summary": "Ready to start."}
        payload["task_state"] = {
            "goal": "Run the ready workflow.",
            "scope": ["src"],
            "success_criteria": ["Workflow starts from the existing path."],
            "open_questions": [],
            "readiness": "ready_to_execute",
        }
        payload["handoff"] = {
            "workflow_request": "Run the ready workflow.",
            "scope": ["src"],
            "success_criteria": ["Workflow starts from the existing path."],
        }
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=payload),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_planning_chat_request(interaction_mode="work"))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact["decision"], "start_workflow")
        self.assertEqual(result.artifact["interaction_mode"], "work")

    def test_planning_chat_tools_exclude_terminal_and_file_editor(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=_planner_chat_turn_output()),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_planning_chat_request())

        self.assertEqual(result.status, "completed")
        self.assertEqual(
            [tool.name for tool in state["agent"]["tools"]],
            [
                "task_tracker",
                "coder_repo_find_files",
                "coder_repo_search_text",
                "coder_repo_read_file",
                "coder_hybrid_rag_search",
            ],
        )
        self.assertEqual(state["agent"]["tools"][-1].params["role"], "planning_chat")
        self.assertEqual(state["agent"]["tools"][-1].params["requested_context"], "assistant_message")
        self.assertIn("use repo evidence for claims about current code", state["conversation"]["prompt"])

    def test_task_execution_rag_tool_params_use_confirmed_scope(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(
                state,
                run_output=_execution_result_output(no_op_rationale="No source changes were required."),
            ),
        )
        with tempfile.TemporaryDirectory() as sandbox:
            base_request = _task_request(sandbox_root=sandbox)
            request = base_request.model_copy(
                update={
                    "context": base_request.context.model_copy(
                        update={
                            "initial_data": {
                                "scopes": ["src"],
                                "memory_store_root": "F:\\store\\.coder",
                                "project_id": "project-1",
                            }
                        }
                    )
                }
            )

            with _env("LLM_API_KEY", "test-key"):
                result = provider.run(request)

        params = state["agent"]["tools"][-1].params
        self.assertEqual(result.status, "completed")
        self.assertEqual(params["role"], "task_execution")
        self.assertEqual(params["requested_context"], "execution_prompt")
        self.assertEqual(params["scope_paths"], ["src"])
        self.assertEqual(params["memory_root"], "F:\\store\\.coder")
        self.assertIsNone(params["session_id"])

    def test_workflow_supervisor_prompt_contains_planner_task_state(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(
                state,
                run_output={
                    "artifact_type": "final_report",
                    "status": "completed",
                    "summary": "Done.",
                    "checks": [],
                    "completed": ["Done."],
                },
            ),
        )
        context_packet = {
            "hot": {"confirmed_goal": "Run a ready plan."},
            "warm": {"planner_task_state": {"goal": "Run a ready plan.", "readiness": "ready_to_execute"}},
        }

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request(input_artifacts={"requested_artifact_type": "final_report"}, context_packet=context_packet))

        self.assertEqual(result.status, "completed")
        self.assertIn("Planner task state", state["conversation"]["prompt"])
        self.assertIn("Run a ready plan.", state["conversation"]["prompt"])

    def test_final_report_does_not_turn_skipped_verification_into_pass(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(
                state,
                run_output={
                    "artifact_type": "final_report",
                    "status": "completed",
                    "summary": "Done.",
                    "checks": [{"command": "python -m unittest", "status": "passed", "summary": "Tests passed."}],
                    "completed": ["Done."],
                },
            ),
        )
        context_packet = {
            "hot": {"confirmed_goal": "Run checks."},
            "warm": {
                "verification_summaries": [
                    {
                        "work_item_id": "work",
                        "status": "skipped",
                        "evidence_refs": [],
                        "no_check_rationale": "No checks were run.",
                    }
                ]
            },
        }

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request(input_artifacts={"requested_artifact_type": "final_report"}, context_packet=context_packet))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact["checks"][0]["status"], "skipped")
        self.assertIn("Skipped verification", result.artifact["warnings"][0])

    def test_blocked_execution_result_becomes_clear_final_report_blocker(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(
                state,
                run_output={
                    "artifact_type": "final_report",
                    "status": "completed",
                    "summary": "Done.",
                    "checks": [],
                    "completed": ["Done."],
                    "blocked_by": [],
                },
            ),
        )
        context_packet = {
            "hot": {"confirmed_goal": "Run blocked work."},
            "warm": {"blocked_reasons": ["pytest is unavailable in the sandbox."]},
        }

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request(input_artifacts={"requested_artifact_type": "final_report"}, context_packet=context_packet))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact["status"], "blocked")
        self.assertIn("pytest is unavailable in the sandbox.", result.artifact["blocked_by"])

    def test_workflow_activity_update_output_can_succeed(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(
                state,
                run_output={
                    "artifact_type": "workflow_activity_update",
                    "visible_phase": "executing",
                    "user_message": "Executor work is in progress.",
                    "steps": [{"id": "execute", "label": "Execute", "status": "active"}],
                    "technical_refs": {"evidence_refs": ["event-1"]},
                },
            ),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request(input_artifacts={"requested_artifact_type": "workflow_activity_update"}))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact_type, "workflow_activity_update")
        self.assertIn("Workflow Activity Update Contract", state["conversation"]["prompt"])

    def test_completed_planner_order_records_loop_trace_events(self) -> None:
        store = NativeRuntimeStore()
        state: dict[str, Any] = {}
        run_output = {
            "artifact_type": "planner_order",
            "round": 1,
            "round_goal": "No executor action required.",
            "plan_graph": {"work_items": []},
            "no_work_rationale": "The requested state is already present.",
        }
        provider = OpenHandsRuntimeProvider(native_store=store, sdk_loader=lambda: _fake_sdk(state, run_output=run_output))

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request(input_artifacts={"requested_artifact_type": "planner_order"}))

        self.assertEqual(result.status, "completed")
        event_types = _native_types(store)
        for native_type in (
            "harness_loop.started",
            "harness_loop.prompt_contract",
            "harness_loop.artifact_candidate",
            "harness_loop.artifact_validation",
            "harness_loop.completed",
        ):
            self.assertIn(native_type, event_types)
        for event in _trace_events(store):
            self.assertIn(event.event_id, result.native_event_refs)

    def test_planner_order_prompt_contains_strict_json_contract(self) -> None:
        state: dict[str, Any] = {}
        run_output = {
            "artifact_type": "planner_order",
            "round": 1,
            "round_goal": "No executor action required.",
            "plan_graph": {"work_items": []},
            "no_work_rationale": "The requested state is already present.",
        }
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=run_output),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request(input_artifacts={"requested_artifact_type": "planner_order"}))

        self.assertEqual(result.status, "completed")
        prompt = state["conversation"]["prompt"]
        self.assertIn("Return exactly one JSON object", prompt)
        self.assertIn('"artifact_type": "planner_order"', prompt)
        self.assertIn('"plan_graph"', prompt)
        self.assertIn('"work_items"', prompt)
        self.assertIn('"no_work_rationale"', prompt)
        self.assertIn("Do not return prose", prompt)
        self.assertIn("Never return an empty work_items list unless no_work_rationale is present", prompt)

    def test_unstructured_planner_order_output_blocks_instead_of_empty_success(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=SimpleNamespace(summary="Do the requested work.")),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request(input_artifacts={"requested_artifact_type": "planner_order"}))

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.error["code"], "insufficient_structured_planner_output")
        self.assertIn("did not return an actionable planner_order", result.error["message"])
        self.assertEqual(result.artifact_type, "planner_order")

    def test_blocked_planner_order_records_loop_trace_events(self) -> None:
        store = NativeRuntimeStore()
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=store,
            sdk_loader=lambda: _fake_sdk(state, run_output=SimpleNamespace(summary="Do the requested work.")),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request(input_artifacts={"requested_artifact_type": "planner_order"}))

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.error["code"], "insufficient_structured_planner_output")
        event_types = _native_types(store)
        self.assertIn("harness_loop.artifact_validation", event_types)
        self.assertIn("harness_loop.blocked", event_types)
        blocked_trace_refs = [
            event.event_id for event in _trace_events(store) if event.native_type == "harness_loop.blocked"
        ]
        self.assertTrue(blocked_trace_refs)
        self.assertIn(blocked_trace_refs[0], result.native_event_refs)

    def test_explicit_no_work_planner_order_output_can_succeed(self) -> None:
        state: dict[str, Any] = {}
        run_output = """```json
{
  "artifact_type": "planner_order",
  "round": 1,
  "round_goal": "No executor action required.",
  "plan_graph": {"work_items": []},
  "no_work_rationale": "The requested state is already present."
}
```"""
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=run_output),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request(input_artifacts={"requested_artifact_type": "planner_order"}))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact_type, "planner_order")
        self.assertEqual(result.artifact["plan_graph"]["work_items"], [])
        self.assertIn("already present", result.artifact["instructions_for_executor"][0])
        projected = ArtifactProjector().project(result)
        self.assertEqual(projected["artifact_type"], "planner_order")

    def test_planner_order_output_from_conversation_events_can_succeed(self) -> None:
        state: dict[str, Any] = {}
        event = SimpleNamespace(
            source="agent",
            llm_message=SimpleNamespace(
                content=[
                    SimpleNamespace(
                        text=(
                            '{"artifact_type":"planner_order","round":1,'
                            '"round_goal":"No executor action required.",'
                            '"plan_graph":{"work_items":[]},'
                            '"no_work_rationale":"The requested state is already present."}'
                        )
                    )
                ]
            ),
        )
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=None, conversation_events=[event]),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request(input_artifacts={"requested_artifact_type": "planner_order"}))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact_type, "planner_order")
        self.assertEqual(result.artifact["plan_graph"]["work_items"], [])
        self.assertIn("already present", result.artifact["no_work_rationale"])

    def test_structured_planner_order_output_with_work_items_succeeds(self) -> None:
        state: dict[str, Any] = {}
        run_output = SimpleNamespace(
            output={
                "artifact_type": "planner_order",
                "round": 1,
                "round_goal": "Execute one task.",
                "plan_graph": {
                    "work_items": [
                        {
                            "work_item_id": "executor-work",
                            "merge_index": 1,
                            "assignee_agent_id": "executor",
                            "task_summary": "Perform the scoped change.",
                            "depends_on": [],
                        }
                    ]
                },
                "instructions_for_executor": ["Stay in scope."],
                "allowed_actions": ["modify_files"],
                "forbidden_actions": ["commit", "push"],
                "expected_outputs": ["execution_result"],
                "risk_level": "low",
            }
        )
        provider = OpenHandsRuntimeProvider(
            native_store=NativeRuntimeStore(),
            sdk_loader=lambda: _fake_sdk(state, run_output=run_output),
        )

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request(input_artifacts={"requested_artifact_type": "planner_order"}))

        self.assertEqual(result.status, "completed")
        self.assertEqual(result.artifact_type, "planner_order")
        self.assertEqual(result.artifact["plan_graph"]["work_items"][0]["work_item_id"], "executor-work")
        projected = ArtifactProjector().project(result)
        self.assertEqual(projected["plan_graph"]["work_items"][0]["work_item_id"], "executor-work")

    def test_completed_execution_result_records_loop_trace_events(self) -> None:
        store = NativeRuntimeStore()
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(
            native_store=store,
            sdk_loader=lambda: _fake_sdk(
                state,
                run_output=_execution_result_output(no_op_rationale="No source changes were required."),
            ),
        )

        with tempfile.TemporaryDirectory() as sandbox:
            with _env("LLM_API_KEY", "test-key"):
                result = provider.run(_task_request(sandbox_root=sandbox))

        self.assertEqual(result.status, "completed")
        event_types = _native_types(store)
        for native_type in (
            "harness_loop.started",
            "harness_loop.prompt_contract",
            "harness_loop.artifact_candidate",
            "harness_loop.artifact_validation",
            "harness_loop.completed",
        ):
            self.assertIn(native_type, event_types)
        for event in _trace_events(store):
            self.assertIn(event.event_id, result.native_event_refs)

    def test_loop_trace_payloads_are_compact_and_safe(self) -> None:
        store = NativeRuntimeStore()
        state: dict[str, Any] = {}
        run_output = _execution_result_output(
            summary=(
                "Fake output test-key DEEPSEEK_API_KEY LLM_API_KEY BEGIN RSA "
                "FULL_PROMPT_MARKER FULL_DIFF_BODY_MARKER"
            ),
            no_op_rationale="No source changes were required.",
        )
        provider = OpenHandsRuntimeProvider(
            native_store=store,
            sdk_loader=lambda: _fake_sdk(state, run_output=run_output),
        )

        with tempfile.TemporaryDirectory() as sandbox:
            with _env("LLM_API_KEY", "test-key"), _env("DEEPSEEK_API_KEY", None):
                result = provider.run(_task_request(sandbox_root=sandbox))

        self.assertEqual(result.status, "completed")
        self.assertTrue(_trace_events(store))
        forbidden = (
            "test-key",
            "DEEPSEEK_API_KEY",
            "LLM_API_KEY",
            "BEGIN RSA",
            "FULL_PROMPT_MARKER",
            "FULL_DIFF_BODY_MARKER",
        )
        for event in _trace_events(store):
            payload_text = _event_payload_text(store, event)
            self.assertLess(len(payload_text), 8000)
            for marker in forbidden:
                self.assertNotIn(marker, payload_text)

    def test_invalid_requested_artifact_target_fails_closed(self) -> None:
        state: dict[str, Any] = {}
        provider = OpenHandsRuntimeProvider(native_store=NativeRuntimeStore(), sdk_loader=lambda: _fake_sdk(state))

        with _env("LLM_API_KEY", "test-key"):
            result = provider.run(_request(input_artifacts={"requested_artifact_type": "execution_result"}))

        self.assertEqual(result.status, "failed")
        self.assertEqual(result.error["code"], "invalid_requested_artifact_type")
        self.assertNotIn("conversation", state)

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
        event_types = [event.native_type for event in store.list_events("run-1")]
        self.assertIn("provider.selected", event_types)
        self.assertIn("sandbox.prepared", event_types)
        self.assertIn("conversation.started", event_types)
        self.assertIn("harness_loop.failed", event_types)
        self.assertIn("conversation.failed", event_types)

    def test_openhands_imports_stay_inside_provider(self) -> None:
        root = Path(__file__).resolve().parents[1] / "src" / "coder_workbench"
        offenders: list[str] = []
        pattern = re.compile(r"^\s*(from|import)\s+openhands\b", re.MULTILINE)
        for path in root.rglob("*.py"):
            relative = path.relative_to(root)
            if path.name == "openhands_provider.py" or relative.parts[:1] == ("openhands_tools",):
                continue
            if pattern.search(path.read_text(encoding="utf-8")):
                offenders.append(str(relative))

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


def _request(input_artifacts: dict[str, Any] | None = None, context_packet: dict[str, Any] | None = None) -> HarnessRunRequest:
    manager = HarnessRuntimeManager()
    context = _context()
    if context_packet is not None:
        context = context.model_copy(update={"context_packet": context_packet})
    return manager._request(
        request_id="request-1",
        contract_id="conversation-harness",
        mode="workflow_supervisor",
        profile_id="openhands-workflow-supervisor-default",
        context=context,
        input_artifacts=input_artifacts or {},
    )


def _planning_chat_request(*, interaction_mode: str = "discuss") -> HarnessRunRequest:
    manager = HarnessRuntimeManager()
    return manager._request(
        request_id="request-1",
        contract_id="conversation-harness",
        mode="planning_chat",
        profile_id="openhands-planning-chat-default",
        context=HarnessRuntimeContext(
            run_id="run-1",
            agent_id="planner",
            workflow_id="workflow-1",
            harness_id="conversation-harness",
            mode="planning_chat",
            profile_id="openhands-planning-chat-default",
            context_packet={
                "hot": {
                    "user_goal": "Plan the work.",
                    "planner_interaction_mode": interaction_mode,
                },
                "warm": {"workflow_summary": {"workflow_id": "workflow-1"}},
            },
        ),
        input_artifacts={
            "requested_artifact_type": "planner_chat_turn",
            "user_request": "Plan the work.",
            "interaction_mode": interaction_mode,
            "messages": [{"role": "user", "content": "Plan the work."}],
            "task_state": {"readiness": "not_ready"},
        },
    )


def _task_request(*, sandbox_root: str | None, repo_root: str = "F:\\repo") -> HarnessRunRequest:
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


def _execution_result_output(
    *,
    status: str = "completed",
    summary: str = "Fake OpenHands completed.",
    no_op_rationale: str | None = None,
) -> dict[str, Any]:
    artifact: dict[str, Any] = {
        "artifact_type": "execution_result",
        "round": 1,
        "work_item_id": "work-1",
        "agent_id": "executor",
        "status": status,
        "summary": summary,
        "changed_files": [],
        "created_files": [],
        "deleted_files": [],
        "patch_refs": [],
        "attempted_actions": [],
        "evidence_refs": [],
        "verification": {
            "status": "skipped" if status == "completed" else "blocked",
            "checks_run": [],
            "evidence_refs": [],
            "confidence": "medium",
            "no_check_rationale": "No checks were run for this test fixture."
            if status == "completed"
            else None,
            "remaining_work": [],
        },
    }
    if no_op_rationale is not None:
        artifact["no_op_rationale"] = no_op_rationale
    return artifact


def _planner_chat_turn_output(*, interaction_mode: str = "discuss", decision: str = "continue_chat") -> dict[str, Any]:
    return {
        "artifact_type": "planner_chat_turn",
        "assistant_message": "What scope should I use for the plan?",
        "interaction_mode": interaction_mode,
        "decision": decision,
        "visible_thinking": {"phase": "clarifying", "summary": "Clarifying scope."},
        "task_state": {
            "goal": "Plan the work.",
            "success_criteria": [],
            "open_questions": ["What scope should I use?"],
            "readiness": "needs_clarification",
        },
        "handoff": None,
    }


def json_dumps(value: Any) -> str:
    import json

    return json.dumps(value)


def _fake_sdk(
    state: dict[str, Any],
    *,
    run_error: Exception | None = None,
    run_output: Any | None = None,
    on_run: Any | None = None,
    conversation_events: list[Any] | None = None,
) -> Any:
    class FakeLLM:
        def __init__(self, **kwargs: Any) -> None:
            state["llm"] = kwargs

    class FakeTool:
        def __init__(self, *, name: str, params: dict[str, Any] | None = None) -> None:
            self.name = name
            self.params = params or {}

    class FakeAgent:
        def __init__(self, *, llm: Any, tools: list[Any]) -> None:
            state["agent"] = {"llm": llm, "tools": tools}

    class FakeConversation:
        def __init__(self, *, agent: Any, workspace: str) -> None:
            state["conversation"] = {"agent": agent, "workspace": workspace}
            self.state = SimpleNamespace(events=conversation_events or [])

        def send_message(self, prompt: str) -> None:
            state["conversation"]["prompt"] = prompt

        def run(self) -> Any:
            if run_error is not None:
                raise run_error
            if on_run is not None:
                on_run(Path(state["conversation"]["workspace"]))
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


def _native_types(store: NativeRuntimeStore) -> list[str]:
    return [event.native_type for event in store.list_events("run-1")]


def _trace_events(store: NativeRuntimeStore) -> list[Any]:
    return [event for event in store.list_events("run-1") if event.native_type.startswith("harness_loop.")]


def _event_payload_text(store: NativeRuntimeStore, event: Any) -> str:
    if event.payload_ref:
        return store.read_payload(event.payload_ref)
    return event.payload_preview or ""


if __name__ == "__main__":
    unittest.main()
