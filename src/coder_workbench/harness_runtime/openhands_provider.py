from __future__ import annotations

import importlib
import importlib.util
import json
import os
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Callable

from .contracts import harness_contract_for_id
from .native_events import NativeRuntimeEvent
from .profiles import OPENHANDS_PROVIDER_ID
from .runtime_context import HarnessRunRequest, HarnessRunResult
from .sandbox import SandboxPreparationError, collect_workspace_changes, prepare_sandbox_workspace
from .store import NativeRuntimeStore


class OpenHandsRuntimeProvider:
    """Feature-flagged OpenHands SDK provider boundary.

    This module is the only place that may import OpenHands SDK modules. The
    provider dynamically loads the SDK, records native-provider lifecycle facts,
    and fails closed when the SDK, credentials, or sandbox workspace are not
    available.
    """

    provider_id = OPENHANDS_PROVIDER_ID

    def __init__(
        self,
        *,
        runtime_module_names: tuple[str, ...] | None = None,
        native_store: NativeRuntimeStore | None = None,
        sdk_loader: Callable[[], Any | None] | None = None,
    ) -> None:
        self.runtime_module_names = runtime_module_names or (
            "openhands.sdk",
            "openhands.tools.file_editor",
            "openhands.tools.task_tracker",
            "openhands.tools.terminal",
        )
        self.native_store = native_store or NativeRuntimeStore()
        self._sdk_loader = sdk_loader

    def is_available(self) -> bool:
        return self._load_sdk() is not None

    def run(self, request: HarnessRunRequest, *, emit: Any | None = None) -> HarnessRunResult:
        sdk = self._load_sdk()
        if sdk is None:
            return self._failed(
                request,
                emit=emit,
                code="openhands_sdk_unavailable",
                message="OpenHands SDK is not importable in this environment.",
                native_type="sdk.unavailable",
            )

        artifact_type, artifact_error = _artifact_type_for_request(request)
        if artifact_error is not None:
            return self._failed(
                request,
                emit=emit,
                code="invalid_requested_artifact_type",
                message=artifact_error,
                native_type="artifact_target.invalid",
            )

        self._emit(
            emit,
            "harness_runtime.openhands.started",
            "OpenHands runtime provider selected",
            mode=request.mode,
            profile_id=request.profile.id,
        )
        selected_event = self._record_event(
            request,
            native_type="provider.selected",
            status="completed",
            summary="OpenHands runtime provider selected.",
            payload={
                "mode": request.mode,
                "profile_id": request.profile.id,
            },
        )

        credentials = _llm_credentials()
        if credentials["api_key"] is None:
            return self._failed(
                request,
                emit=emit,
                code="openhands_llm_credentials_missing",
                message="OpenHands runtime requires LLM_API_KEY or DEEPSEEK_API_KEY.",
                native_type="credentials.missing",
                status="blocked",
                refs=[selected_event.event_id],
            )

        contract = harness_contract_for_id(request.contract_id)
        try:
            sandbox_context = prepare_sandbox_workspace(
                contract=contract,
                profile=request.profile,
                context=request.context,
            )
        except SandboxPreparationError as exc:
            return self._failed(
                request,
                emit=emit,
                code="openhands_sandbox_unavailable",
                message=str(exc),
                native_type="sandbox.unavailable",
                status="blocked",
                refs=[selected_event.event_id],
            )

        try:
            with sandbox_context as sandbox:
                workspace = sandbox.path
                sandbox_event = self._record_event(
                    request,
                    native_type="sandbox.prepared",
                    status="completed",
                    summary="Sandbox workspace prepared.",
                    payload={
                        "mode": request.mode,
                        "workspace_mode": sandbox.workspace_mode,
                        "temporary": sandbox.temporary,
                        "workspace": str(workspace),
                    },
                )
                started_event = self._record_event(
                    request,
                    native_type="conversation.started",
                    status="running",
                    summary="OpenHands conversation started.",
                    payload={
                        "mode": request.mode,
                        "workspace": str(workspace),
                        "tools": self._tool_names_for_request(request, sdk),
                        "model": credentials["model"],
                        "base_url_configured": bool(credentials["base_url"]),
                    },
                )
                prompt = _prompt_for_request(request)
                try:
                    tools = self._tools_for_request(request, sdk)
                    llm = sdk.LLM(
                        model=credentials["model"],
                        api_key=credentials["api_key"],
                        base_url=credentials["base_url"],
                    )
                    agent = sdk.Agent(llm=llm, tools=tools)
                    conversation = sdk.Conversation(agent=agent, workspace=str(workspace))
                    conversation.send_message(prompt)
                    run_output = conversation.run()
                except Exception as exc:
                    return self._failed(
                        request,
                        emit=emit,
                        code="openhands_run_failed",
                        message=f"OpenHands conversation failed: {exc}",
                        native_type="conversation.failed",
                        refs=[selected_event.event_id, sandbox_event.event_id, started_event.event_id],
                        payload={"error_type": type(exc).__name__, "message": str(exc)},
                    )

                summary = _summarize_run_output(run_output)
                facts = _merge_runtime_facts(
                    _runtime_facts(run_output),
                    self._sandbox_facts(request, sandbox),
                )
                facts["native_event_refs"] = _dedupe(
                    [sandbox_event.event_id, started_event.event_id, *facts["native_event_refs"]]
                )
        except SandboxPreparationError as exc:
            return self._failed(
                request,
                emit=emit,
                code="openhands_sandbox_unavailable",
                message=str(exc),
                native_type="sandbox.unavailable",
                status="blocked",
                refs=[selected_event.event_id],
            )

        completed_event = self._record_event(
            request,
            native_type="conversation.completed",
            status="completed",
            summary=summary,
            payload={
                "mode": request.mode,
                "output_type": type(run_output).__name__,
                "output_summary": summary,
                "changed_files": facts["changed_files"],
                "created_files": facts["created_files"],
                "deleted_files": facts["deleted_files"],
                "diff_refs": facts["diff_refs"],
                "log_refs": facts["log_refs"],
                "evidence_refs": facts["evidence_refs"],
            },
        )
        refs = [selected_event.event_id, *facts["native_event_refs"], completed_event.event_id]
        evidence_refs = _dedupe([*refs, *facts["evidence_refs"]])
        self._emit(
            emit,
            "harness_runtime.openhands.completed",
            summary,
            mode=request.mode,
            profile_id=request.profile.id,
            native_event_refs=refs,
        )
        return HarnessRunResult(
            status="completed",
            artifact_type=artifact_type,
            artifact=_artifact_for_success(
                request,
                artifact_type=artifact_type,
                summary=summary,
                evidence_refs=evidence_refs,
                facts=facts,
            ),
            native_event_refs=refs,
            evidence_refs=evidence_refs,
            diff_refs=facts["diff_refs"],
            log_refs=facts["log_refs"],
        )

    def _load_sdk(self) -> Any | None:
        if self._sdk_loader is not None:
            try:
                return self._sdk_loader()
            except Exception:
                return None
        for module_name in self.runtime_module_names:
            try:
                if importlib.util.find_spec(module_name) is None:
                    return None
            except (ImportError, ValueError):
                return None
        try:
            sdk = importlib.import_module("openhands.sdk")
            file_editor = importlib.import_module("openhands.tools.file_editor")
            task_tracker = importlib.import_module("openhands.tools.task_tracker")
            terminal = importlib.import_module("openhands.tools.terminal")
        except ImportError:
            return None
        return SimpleNamespace(
            LLM=sdk.LLM,
            Agent=sdk.Agent,
            Conversation=sdk.Conversation,
            Tool=sdk.Tool,
            FileEditorTool=file_editor.FileEditorTool,
            TaskTrackerTool=task_tracker.TaskTrackerTool,
            TerminalTool=terminal.TerminalTool,
        )

    def _workspace_for_request(self, request: HarnessRunRequest) -> Path | None:
        if request.mode == "task_execution":
            if request.context.sandbox_root:
                return Path(request.context.sandbox_root)
            if request.profile.sandbox_policy.get("allow_repo_root_for_tests") and request.context.repo_root:
                return Path(request.context.repo_root)
            return None
        if request.context.sandbox_root:
            return Path(request.context.sandbox_root)
        if request.context.repo_root:
            return Path(request.context.repo_root)
        return Path(".")

    def _tool_names_for_request(self, request: HarnessRunRequest, sdk: Any) -> list[str]:
        if request.mode == "task_execution":
            return [sdk.TerminalTool.name, sdk.FileEditorTool.name, sdk.TaskTrackerTool.name]
        return [sdk.TaskTrackerTool.name]

    def _tools_for_request(self, request: HarnessRunRequest, sdk: Any) -> list[Any]:
        return [sdk.Tool(name=name) for name in self._tool_names_for_request(request, sdk)]

    def _sandbox_facts(self, request: HarnessRunRequest, sandbox: Any) -> dict[str, list[str]]:
        facts = _empty_runtime_facts()
        if request.mode != "task_execution":
            return facts
        changes = collect_workspace_changes(sandbox)
        facts["changed_files"] = list(changes["changed_files"])
        facts["created_files"] = list(changes["created_files"])
        facts["deleted_files"] = list(changes["deleted_files"])
        has_changes = bool(facts["changed_files"] or facts["created_files"] or facts["deleted_files"])
        diff_text = str(changes.get("diff") or "")
        if request.profile.sandbox_policy.get("collect_diff_refs", True) and diff_text:
            diff_event = self._record_event(
                request,
                native_type="sandbox.diff",
                status="completed",
                summary="Sandbox workspace diff collected.",
                payload=diff_text,
            )
            facts["diff_refs"].append(diff_event.payload_ref or diff_event.event_id)
            facts["native_event_refs"].append(diff_event.event_id)
        if has_changes and request.profile.sandbox_policy.get("collect_log_refs", True):
            log_event = self._record_event(
                request,
                native_type="sandbox.summary",
                status="completed",
                summary="Sandbox workspace summary collected.",
                payload={
                    "workspace_mode": sandbox.workspace_mode,
                    "temporary": sandbox.temporary,
                    "changed_files": facts["changed_files"],
                    "created_files": facts["created_files"],
                    "deleted_files": facts["deleted_files"],
                    "diff_refs": facts["diff_refs"],
                },
            )
            facts["log_refs"].append(log_event.payload_ref or log_event.event_id)
            facts["native_event_refs"].append(log_event.event_id)
        facts["evidence_refs"] = _dedupe([*facts["diff_refs"], *facts["log_refs"]])
        return facts

    def _failed(
        self,
        request: HarnessRunRequest,
        *,
        emit: Any | None,
        code: str,
        message: str,
        native_type: str,
        status: str = "failed",
        refs: list[str] | None = None,
        payload: dict[str, Any] | None = None,
    ) -> HarnessRunResult:
        event = self._record_event(
            request,
            native_type=native_type,
            status=status,
            summary=message,
            payload=payload or {"code": code, "message": message},
        )
        self._emit(
            emit,
            "harness_runtime.openhands.failed",
            message,
            mode=request.mode,
            profile_id=request.profile.id,
            native_event_ref=event.event_id,
        )
        native_event_refs = [*(refs or []), event.event_id]
        return HarnessRunResult(
            status=status,
            artifact_type=_artifact_type_for_request(request)[0] if status == "blocked" else None,
            artifact=_artifact_for_blocked(request, message=message, evidence_refs=native_event_refs)
            if status == "blocked"
            else None,
            native_event_refs=native_event_refs,
            error={"code": code, "message": message},
        )

    def _record_event(
        self,
        request: HarnessRunRequest,
        *,
        native_type: str,
        status: str,
        summary: str,
        payload: dict[str, Any],
    ) -> NativeRuntimeEvent:
        return self.native_store.append_event(
            run_id=request.context.run_id,
            round=request.context.round,
            work_item_id=str(request.input_artifacts.get("work_item_id") or "") or None,
            agent_id=request.context.agent_id,
            provider_id=self.provider_id,
            harness_id=request.profile.harness_id,
            mode=request.mode,
            native_type=native_type,
            status=status,
            summary=summary,
            payload=payload,
        )

    def _emit(self, emit: Any | None, event_type: str, message: str, **payload: Any) -> None:
        if emit is None:
            return
        emit(event_type, message, **payload)


def _llm_credentials() -> dict[str, str | None]:
    base_url = os.getenv("LLM_BASE_URL") or "https://api.deepseek.com"
    model = _normalize_deepseek_model(os.getenv("LLM_MODEL") or "deepseek-v4-flash", base_url=base_url)
    return {
        "api_key": os.getenv("LLM_API_KEY") or os.getenv("DEEPSEEK_API_KEY"),
        "model": model,
        "base_url": base_url,
    }


def _normalize_deepseek_model(model: str, *, base_url: str | None) -> str:
    text = model.strip() or "deepseek-v4-flash"
    if "/" in text:
        return text
    if text.startswith("deepseek-") or text in {"deepseek-chat", "deepseek-reasoner"}:
        return f"deepseek/{text}"
    if "deepseek.com" in str(base_url or "").lower() and text.startswith("v"):
        return f"deepseek/{text}"
    return text


def _prompt_for_request(request: HarnessRunRequest) -> str:
    context_packet = request.context.context_packet or {}
    artifact_type, _artifact_error = _artifact_type_for_request(request)
    artifact_target = artifact_type or "unknown"
    lines = [
        f"Runtime mode: {request.mode}",
        f"Workflow: {request.context.workflow_id}",
        f"Agent: {request.context.agent_id}",
        f"Current Coder artifact target: {artifact_target}",
    ]
    if request.mode == "task_execution":
        lines.extend(
            [
                "You are the Task Execution Harness.",
                "Stay inside the provided workspace.",
                "Perform only bounded task execution inside the sandbox workspace.",
                "Do not ask the user any questions.",
                "Do not commit, push, deploy, publish externally, or write long-term memory.",
                "Return enough structured information for Coder to project a valid execution_result artifact.",
                "Return a concise completion summary and verification evidence.",
            ]
        )
        _append_section(lines, "Work item", request.input_artifacts.get("work_item") or _dig(context_packet, "hot", "work_item"))
        _append_section(
            lines,
            "Task envelope",
            request.input_artifacts.get("task_envelope") or _dig(context_packet, "hot", "task_envelope"),
        )
        _append_section(lines, "Constraints", _dig(context_packet, "hot", "constraints"))
        _append_section(lines, "Success criteria", request.input_artifacts.get("success_criteria"))
        return "\n\n".join(lines)
    if request.mode == "workflow_supervisor":
        lines.extend(
            [
                "You are the Workflow Supervisor Harness.",
                "Do not write files or run commands.",
                f"Return enough structured information for Coder to project a valid {artifact_target} artifact.",
                "Use execution summaries and evidence refs to decide whether the workflow should continue or finish.",
            ]
        )
        _append_section(lines, "Confirmed goal", _dig(context_packet, "hot", "confirmed_goal") or _dig(context_packet, "hot", "user_goal"))
        _append_section(lines, "Round state", request.context.round_working_set or _dig(context_packet, "warm", "run_state_summary"))
        _append_section(lines, "Round summary", _dig(context_packet, "warm", "round_summary"))
        _append_section(lines, "Execution summaries", _dig(context_packet, "warm", "execution_result_summaries"))
        _append_section(lines, "Verification summaries", _dig(context_packet, "warm", "verification_summaries"))
        _append_section(lines, "Blocked reasons", _dig(context_packet, "warm", "blocked_reasons"))
        _append_section(lines, "Changed files summary", _dig(context_packet, "warm", "changed_files_summary"))
        _append_section(lines, "Evidence refs", request.input_artifacts.get("evidence_refs") or _cold_refs(context_packet, "evidence"))
        _append_section(lines, "Native runtime refs", _cold_refs(context_packet, "native_runtime"))
        _append_section(lines, "Diff refs", _cold_refs(context_packet, "diff"))
        _append_section(lines, "Log refs", _cold_refs(context_packet, "log"))
        return "\n\n".join(lines)
    lines.extend(
        [
            "You are the Planning Chat Harness.",
            "Produce a draft only.",
            "Return enough structured information for Coder to project a valid project_plan_draft artifact.",
            "Do not execute commands, modify files, or start the live run.",
        ]
    )
    _append_section(lines, "User request", request.input_artifacts.get("user_request") or _dig(context_packet, "hot", "user_goal"))
    _append_section(lines, "Workflow summary", _dig(context_packet, "warm", "workflow_summary"))
    _append_section(lines, "Selected knowledge pack IDs", _dig(context_packet, "hot", "selected_knowledge_pack_ids"))
    _append_section(lines, "Selected skill pack IDs", _dig(context_packet, "hot", "selected_skill_pack_ids"))
    _append_section(lines, "Selected memory pack IDs", _dig(context_packet, "hot", "selected_memory_pack_ids"))
    return "\n\n".join(lines)


def _append_section(lines: list[str], title: str, value: Any) -> None:
    if value in (None, "", [], {}):
        return
    lines.append(f"{title}:\n{_safe_json(value)}")


def _safe_json(value: Any, *, limit: int = 4000) -> str:
    if isinstance(value, str):
        text = value
    else:
        text = str(value) if not isinstance(value, (dict, list)) else json.dumps(value, ensure_ascii=False, sort_keys=True, default=str)
    if len(text) <= limit:
        return text
    return f"{text[:limit]}\n...<truncated>..."


def _dig(value: dict[str, Any], *path: str) -> Any:
    current: Any = value
    for key in path:
        if not isinstance(current, dict):
            return None
        current = current.get(key)
    return current


def _cold_refs(context_packet: dict[str, Any], ref_type: str) -> list[str]:
    refs: list[str] = []
    for record in context_packet.get("cold_refs", []):
        if isinstance(record, dict) and record.get("ref_type") == ref_type and isinstance(record.get("refs"), list):
            refs.extend(str(ref) for ref in record["refs"])
    return refs


def _artifact_type_for_request(request: HarnessRunRequest) -> tuple[str | None, str | None]:
    requested = request.input_artifacts.get("requested_artifact_type")
    if requested:
        return _validate_requested_artifact_type(request.mode, str(requested))

    legacy_operation = str(request.input_artifacts.get("legacy_operation") or "")
    mapped = _artifact_type_from_legacy_operation(legacy_operation)
    if mapped:
        return _validate_requested_artifact_type(request.mode, mapped)

    if request.mode == "task_execution":
        return "execution_result", None
    if request.mode == "planning_chat":
        return "project_plan_draft", None
    return "final_report", None


def _artifact_type_from_legacy_operation(operation: str) -> str | None:
    return {
        "planner_order": "planner_order",
        "planner_decision": "planner_decision",
        "final_report": "final_report",
        "task_execution": "execution_result",
        "planning_chat": "project_plan_draft",
    }.get(operation)


def _validate_requested_artifact_type(mode: str, artifact_type: str) -> tuple[str | None, str | None]:
    allowed = {
        "planning_chat": {"project_plan_draft"},
        "task_execution": {"execution_result"},
        "workflow_supervisor": {"planner_order", "planner_decision", "final_report"},
    }.get(mode, set())
    if artifact_type in allowed:
        return artifact_type, None
    expected = ", ".join(sorted(allowed)) or "none"
    return None, f"{artifact_type!r} is not a valid artifact target for mode {mode!r}; expected one of: {expected}."


def _artifact_for_success(
    request: HarnessRunRequest,
    *,
    artifact_type: str,
    summary: str,
    evidence_refs: list[str],
    facts: dict[str, Any],
) -> dict[str, Any]:
    if artifact_type == "execution_result":
        checks_run = _check_records(facts.get("checks_run"))
        verification_status = "pass" if _has_passing_check(checks_run) else "skipped"
        no_check_rationale = (
            None
            if verification_status == "pass"
            else "OpenHands conversation completed; no explicit passing check evidence was extracted."
        )
        return {
            "artifact_type": "execution_result",
            "round": request.context.round or 1,
            "work_item_id": str(request.input_artifacts.get("work_item_id") or "") or None,
            "agent_id": request.context.agent_id,
            "status": "completed",
            "summary": summary,
            "changed_files": facts["changed_files"],
            "created_files": facts["created_files"],
            "deleted_files": facts["deleted_files"],
            "patch_refs": facts["diff_refs"],
            "attempted_actions": facts["commands_run"],
            "evidence_refs": evidence_refs,
            "no_op_rationale": None
            if _has_runtime_evidence(facts)
            else "OpenHands conversation completed without extracted file changes, command results, or check evidence.",
            "verification": {
                "status": verification_status,
                "checks_run": checks_run,
                "evidence_refs": evidence_refs,
                "confidence": "medium",
                "no_check_rationale": no_check_rationale,
            },
        }
    if artifact_type == "project_plan_draft":
        draft_id = str(request.input_artifacts.get("draft_id") or request.request_id)
        return {
            "artifact_type": "project_plan_draft",
            "draft_id": draft_id,
            "summary": summary,
            "proposed_scope": [],
            "success_criteria": ["Confirm the draft before execution."],
            "risks": [],
            "requires_confirmation": True,
        }
    if artifact_type == "planner_order":
        return {
            "artifact_type": "planner_order",
            "round": request.context.round or 1,
            "round_goal": summary or "Workflow supervisor found no executor work.",
            "plan_graph": {"work_items": []},
            "instructions_for_executor": [
                "No task_execution work item was created because OpenHands returned only supervisor-level information."
            ],
            "allowed_actions": [],
            "forbidden_actions": ["write_files", "run_commands"],
            "expected_outputs": ["No executor output is expected for this no-work planner_order."],
            "risk_level": "low",
            "requires_human_confirmation": False,
        }
    if artifact_type == "planner_decision":
        return {
            "artifact_type": "planner_decision",
            "round": request.context.round or 1,
            "task_done": True,
            "next_action": "finish",
            "final_status": "completed",
            "risk_level": "low",
            "requires_human_confirmation": False,
            "reason": summary or "OpenHands workflow supervisor completed.",
            "next_round_goal": "",
            "remaining_auto_rounds": 0,
            "human_message": None,
        }
    return {
        "artifact_type": "final_report",
        "status": "completed",
        "summary": summary,
        "checks": [],
        "completed": [summary],
        "blocked_by": [],
        "failed_by": [],
        "warnings": [],
        "notes": [],
        "next_steps": [],
        "evidence_refs": evidence_refs,
    }


def _artifact_for_blocked(request: HarnessRunRequest, *, message: str, evidence_refs: list[str]) -> dict[str, Any]:
    artifact_type = _artifact_type_for_request(request)[0]
    if artifact_type == "execution_result":
        return {
            "artifact_type": "execution_result",
            "round": request.context.round or 1,
            "work_item_id": str(request.input_artifacts.get("work_item_id") or "") or None,
            "agent_id": request.context.agent_id,
            "status": "blocked",
            "summary": message,
            "evidence_refs": evidence_refs,
            "remaining_work": ["Resolve the OpenHands runtime blocker."],
            "unexpected_issues": [message],
            "needs_planner_decision": True,
            "blocker_type": "missing_secret" if "API_KEY" in message else "sandbox_unavailable",
            "executor_recovery_exhausted": True,
            "blocker_reason": message,
            "planner_recommendation": "finish",
            "verification": {
                "status": "blocked",
                "checks_run": [],
                "evidence_refs": evidence_refs,
                "confidence": "medium",
                "remaining_work": ["Resolve the OpenHands runtime blocker."],
            },
        }
    if artifact_type == "project_plan_draft":
        draft_id = str(request.input_artifacts.get("draft_id") or request.request_id)
        return {
            "artifact_type": "project_plan_draft",
            "draft_id": draft_id,
            "summary": message,
            "proposed_scope": [],
            "success_criteria": [],
            "risks": [message],
            "requires_confirmation": True,
        }
    if artifact_type == "planner_order":
        return {
            "artifact_type": "planner_order",
            "round": request.context.round or 1,
            "round_goal": f"OpenHands workflow supervisor blocked: {message}",
            "plan_graph": {"work_items": []},
            "instructions_for_executor": [message],
            "allowed_actions": [],
            "forbidden_actions": ["write_files", "run_commands"],
            "expected_outputs": ["Resolve the OpenHands runtime blocker before creating executor work."],
            "risk_level": "medium",
            "requires_human_confirmation": False,
        }
    if artifact_type == "planner_decision":
        return {
            "artifact_type": "planner_decision",
            "round": request.context.round or 1,
            "task_done": False,
            "next_action": "finish",
            "final_status": "blocked",
            "risk_level": "medium",
            "requires_human_confirmation": False,
            "reason": message,
            "next_round_goal": "",
            "remaining_auto_rounds": 0,
            "human_message": None,
        }
    return {
        "artifact_type": "final_report",
        "status": "blocked",
        "summary": message,
        "checks": [],
        "completed": [],
        "blocked_by": [message],
        "failed_by": [],
        "warnings": [],
        "notes": [],
        "next_steps": ["Resolve the OpenHands runtime blocker."],
        "evidence_refs": evidence_refs,
    }


def _summarize_run_output(run_output: Any) -> str:
    if isinstance(run_output, str) and run_output.strip():
        return run_output.strip()
    for attr in ("summary", "final_message", "message", "content"):
        value = getattr(run_output, attr, None)
        if isinstance(value, str) and value.strip():
            return value.strip()
    if run_output is None:
        return "OpenHands conversation completed."
    return "OpenHands conversation completed."


def _runtime_facts(run_output: Any) -> dict[str, Any]:
    return {
        "changed_files": _string_list_fact(run_output, "changed_files"),
        "created_files": _string_list_fact(run_output, "created_files"),
        "deleted_files": _string_list_fact(run_output, "deleted_files"),
        "diff_refs": _string_list_fact(run_output, "diff_refs", "patch_refs"),
        "log_refs": _string_list_fact(run_output, "log_refs"),
        "evidence_refs": _string_list_fact(run_output, "evidence_refs"),
        "commands_run": _string_list_fact(run_output, "commands_run", "attempted_actions"),
        "checks_run": _check_records(_fact_value(run_output, "checks_run")),
        "native_event_refs": [],
    }


def _empty_runtime_facts() -> dict[str, Any]:
    return {
        "changed_files": [],
        "created_files": [],
        "deleted_files": [],
        "diff_refs": [],
        "log_refs": [],
        "evidence_refs": [],
        "commands_run": [],
        "checks_run": [],
        "native_event_refs": [],
    }


def _merge_runtime_facts(*records: dict[str, Any]) -> dict[str, Any]:
    merged = _empty_runtime_facts()
    for record in records:
        for key in merged:
            if key == "checks_run":
                merged[key] = _dedupe_checks([*merged[key], *record.get(key, [])])
            else:
                merged[key] = _dedupe([*merged[key], *record.get(key, [])])
    return merged


def _has_runtime_evidence(facts: dict[str, Any]) -> bool:
    return any(
        facts.get(key)
        for key in (
            "changed_files",
            "created_files",
            "deleted_files",
            "diff_refs",
            "log_refs",
            "evidence_refs",
            "commands_run",
            "checks_run",
        )
    )


def _string_list_fact(run_output: Any, *names: str) -> list[str]:
    for name in names:
        value = _fact_value(run_output, name)
        if isinstance(value, list):
            return [str(item) for item in value if str(item)]
        if isinstance(value, tuple):
            return [str(item) for item in value if str(item)]
    return []


def _fact_value(run_output: Any, name: str) -> Any:
    if isinstance(run_output, dict):
        return run_output.get(name)
    return getattr(run_output, name, None)


def _check_records(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, list):
        return []
    records: list[dict[str, Any]] = []
    for index, item in enumerate(value, start=1):
        if isinstance(item, str):
            records.append(
                {
                    "check_id": f"check-{index}",
                    "kind": "command",
                    "status": "pass" if "pass" in item.lower() else "skipped",
                    "summary": item,
                }
            )
            continue
        if not isinstance(item, dict):
            continue
        record = dict(item)
        record["status"] = _check_status(record.get("status"))
        record.setdefault("kind", "command" if record.get("command") else "model")
        record.setdefault("summary", str(record.get("command") or record.get("check_id") or f"Check {index}"))
        records.append(record)
    return records


def _check_status(value: Any) -> str:
    status = str(value or "").strip().lower()
    if status in {"pass", "passed", "success", "succeeded", "ok"}:
        return "pass"
    if status in {"fail", "failed", "failure", "error"}:
        return "fail"
    if status == "blocked":
        return "blocked"
    return "skipped"


def _has_passing_check(checks_run: list[dict[str, Any]]) -> bool:
    return any(check.get("status") == "pass" for check in checks_run)


def _dedupe_checks(values: list[Any]) -> list[dict[str, Any]]:
    seen: set[str] = set()
    output: list[dict[str, Any]] = []
    for record in _check_records(values):
        key = json.dumps(record, sort_keys=True, default=str)
        if key in seen:
            continue
        seen.add(key)
        output.append(record)
    return output


def _dedupe(values: list[str]) -> list[str]:
    seen: set[str] = set()
    output: list[str] = []
    for value in values:
        if value in seen:
            continue
        seen.add(value)
        output.append(value)
    return output


__all__ = ["OpenHandsRuntimeProvider"]
