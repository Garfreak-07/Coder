from __future__ import annotations

import importlib
import importlib.util
import json
import os
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Callable

from .native_events import NativeRuntimeEvent
from .profiles import OPENHANDS_PROVIDER_ID
from .runtime_context import HarnessRunRequest, HarnessRunResult
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

        workspace = self._workspace_for_request(request)
        if workspace is None:
            return self._failed(
                request,
                emit=emit,
                code="openhands_sandbox_unavailable",
                message="Task Execution Harness requires sandbox_root for OpenHands execution.",
                native_type="sandbox.unavailable",
                status="blocked",
                refs=[selected_event.event_id],
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
                refs=[selected_event.event_id, started_event.event_id],
                payload={"error_type": type(exc).__name__, "message": str(exc)},
            )

        summary = _summarize_run_output(run_output)
        facts = _runtime_facts(run_output)
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
        refs = [selected_event.event_id, started_event.event_id, completed_event.event_id]
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
            artifact_type=_artifact_type_for_request(request),
            artifact=_artifact_for_success(request, summary=summary, evidence_refs=evidence_refs, facts=facts),
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
            artifact_type=_artifact_type_for_request(request) if status == "blocked" else None,
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
    return {
        "api_key": os.getenv("LLM_API_KEY") or os.getenv("DEEPSEEK_API_KEY"),
        "model": os.getenv("LLM_MODEL") or "deepseek-v4-flash",
        "base_url": os.getenv("LLM_BASE_URL") or "https://api.deepseek.com",
    }


def _prompt_for_request(request: HarnessRunRequest) -> str:
    context_packet = request.context.context_packet or {}
    lines = [
        f"Runtime mode: {request.mode}",
        f"Workflow: {request.context.workflow_id}",
        f"Agent: {request.context.agent_id}",
    ]
    if request.mode == "task_execution":
        lines.extend(
            [
                "You are the Task Execution Harness.",
                "Stay inside the provided workspace.",
                "Do not ask the user any questions.",
                "Do not commit, push, deploy, publish externally, or write long-term memory.",
                "Return a concise completion summary with verification evidence.",
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
                "Use execution summaries and evidence refs to decide whether the workflow should continue or finish.",
            ]
        )
        _append_section(lines, "Confirmed goal", _dig(context_packet, "hot", "confirmed_goal") or _dig(context_packet, "hot", "user_goal"))
        _append_section(lines, "Round state", request.context.round_working_set or _dig(context_packet, "warm", "run_state_summary"))
        _append_section(lines, "Evidence refs", request.input_artifacts.get("evidence_refs") or _cold_refs(context_packet, "evidence"))
        return "\n\n".join(lines)
    lines.extend(
        [
            "You are the Planning Chat Harness.",
            "Produce a draft only.",
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


def _artifact_type_for_request(request: HarnessRunRequest) -> str:
    if request.mode == "task_execution":
        return "execution_result"
    if request.mode == "planning_chat":
        return "project_plan_draft"
    return "final_report"


def _artifact_for_success(
    request: HarnessRunRequest,
    *,
    summary: str,
    evidence_refs: list[str],
    facts: dict[str, list[str]],
) -> dict[str, Any]:
    if request.mode == "task_execution":
        verification_status = "pass" if _has_runtime_evidence(facts) else "skipped"
        no_check_rationale = None if verification_status == "pass" else "OpenHands conversation completed; explicit check extraction is not wired yet."
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
            "evidence_refs": evidence_refs,
            "verification": {
                "status": verification_status,
                "checks_run": [],
                "evidence_refs": evidence_refs,
                "confidence": "medium",
                "no_check_rationale": no_check_rationale,
            },
        }
    if request.mode == "planning_chat":
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
    if request.mode == "task_execution":
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
    if request.mode == "planning_chat":
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


def _runtime_facts(run_output: Any) -> dict[str, list[str]]:
    return {
        "changed_files": _string_list_fact(run_output, "changed_files"),
        "created_files": _string_list_fact(run_output, "created_files"),
        "deleted_files": _string_list_fact(run_output, "deleted_files"),
        "diff_refs": _string_list_fact(run_output, "diff_refs", "patch_refs"),
        "log_refs": _string_list_fact(run_output, "log_refs"),
        "evidence_refs": _string_list_fact(run_output, "evidence_refs"),
    }


def _has_runtime_evidence(facts: dict[str, list[str]]) -> bool:
    return any(
        facts.get(key)
        for key in ("changed_files", "created_files", "deleted_files", "diff_refs", "log_refs", "evidence_refs")
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
