from __future__ import annotations

import importlib
import importlib.util
import json
import os
import re
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Callable

from coder_workbench.core.artifacts import ArtifactValidationError, validate_artifact

from .contracts import harness_contract_for_id
from .native_events import NativeRuntimeEvent
from .permissions import HarnessPermissionDecision, evaluate_harness_permission
from .profiles import OPENHANDS_PROVIDER_ID, normalize_llm_model, resolve_llm_provider_profile
from .runtime_context import HarnessRunRequest, HarnessRunResult
from .sandbox import SandboxPreparationError, collect_workspace_changes, prepare_sandbox_workspace
from .store import NativeRuntimeStore


_INSUFFICIENT_PLANNER_OUTPUT_MESSAGE = (
    "OpenHands workflow supervisor did not return an actionable planner_order or an explicit no-work rationale."
)
_INSUFFICIENT_EXECUTION_RESULT_MESSAGE = (
    "OpenHands task executor did not return exactly one structured execution_result artifact."
)
_INSUFFICIENT_PLANNER_CHAT_TURN_MESSAGE = (
    "OpenHands planning chat did not return exactly one structured planner_chat_turn artifact."
)
_INSUFFICIENT_WORKFLOW_ACTIVITY_UPDATE_MESSAGE = (
    "OpenHands workflow supervisor did not return exactly one structured workflow_activity_update artifact."
)

_BLOCKER_TYPES = {
    "test_failed",
    "command_failed",
    "schema_validation_failed",
    "command_unavailable",
    "missing_dependency",
    "missing_file",
    "scope_violation",
    "risk_path_blocked",
    "permission_boundary",
    "missing_secret",
    "network_required",
    "external_account_required",
    "timeout",
    "context_missing",
    "tool_unavailable",
    "sandbox_unavailable",
    "unknown_error",
}
_LEGACY_BLOCKER_TYPES = {
    "dependency_missing": "missing_dependency",
    "technical_blocker": "unknown_error",
    "verification_failed": "test_failed",
    "permission_blocked": "permission_boundary",
    "tool_error": "tool_unavailable",
    "out_of_contract": "scope_violation",
    "scope_boundary": "scope_violation",
    "risk_boundary": "risk_path_blocked",
    "unsafe_action": "risk_path_blocked",
    "patch_rejected": "risk_path_blocked",
    "transient_error_exhausted": "unknown_error",
    "ambiguity": "context_missing",
    "plan_conflict": "unknown_error",
}
_TRACE_REDACTIONS = (
    "test-key",
    "DEEPSEEK_API_KEY",
    "LLM_API_KEY",
    "BEGIN RSA",
    "api_key",
    "password",
    "secret",
    "token",
)


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
        loop_started_event = self._record_loop_event(
            request,
            phase="started",
            status="completed",
            summary="OpenHands harness loop started.",
            payload={
                "artifact_target": artifact_type,
                "profile_id": request.profile.id,
                "contract_id": request.contract_id,
            },
        )
        loop_refs = [loop_started_event.event_id]

        try:
            credentials = _llm_credentials()
        except ValueError as exc:
            return self._failed(
                request,
                emit=emit,
                code="openhands_llm_provider_profile_invalid",
                message=str(exc),
                native_type="credentials.profile_invalid",
                status="blocked",
                refs=[selected_event.event_id, *loop_refs],
            )
        if credentials["api_key"] is None:
            return self._failed(
                request,
                emit=emit,
                code="openhands_llm_credentials_missing",
                message="OpenHands runtime requires LLM_API_KEY or DEEPSEEK_API_KEY.",
                native_type="credentials.missing",
                status="blocked",
                refs=[selected_event.event_id, *loop_refs],
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
                refs=[selected_event.event_id, *loop_refs],
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
                prompt = _prompt_for_request(request)
                prompt_contract_event = self._record_loop_event(
                    request,
                    phase="prompt_contract",
                    status="completed",
                    summary="OpenHands prompt output contract assembled.",
                    payload={
                        "artifact_target": artifact_type,
                        "has_output_contract": artifact_output_contract_available(str(artifact_type or "")),
                        "contract_kind": artifact_type,
                        "prompt_length_chars": len(prompt),
                    },
                )
                loop_refs.append(prompt_contract_event.event_id)
                tool_names = self._tool_names_for_request(request, sdk)
                permission_refs: list[str] = []
                for tool_name in tool_names:
                    decision = evaluate_harness_permission(
                        mode=request.mode,
                        harness_id=request.profile.harness_id,
                        role=contract.role,
                        tool_name=tool_name,
                        sandbox_root=str(workspace),
                    )
                    permission_event = self._record_permission_event(
                        request,
                        decision=decision,
                        tool_name=tool_name,
                        summary="Harness permission preflight allowed tool."
                        if decision.allowed
                        else "Harness permission preflight denied tool.",
                    )
                    permission_refs.append(permission_event.event_id)
                    if not decision.allowed:
                        return self._failed(
                            request,
                            emit=emit,
                            code=decision.code,
                            message=decision.reason,
                            native_type="permission.denied",
                            status="blocked",
                            refs=[
                                selected_event.event_id,
                                sandbox_event.event_id,
                                *loop_refs,
                                *permission_refs,
                            ],
                            payload={
                                "code": decision.code,
                                "message": decision.reason,
                                "tool_name": tool_name,
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
                        "tools": tool_names,
                        "model": credentials["model"],
                        "base_url_configured": bool(credentials["base_url"]),
                    },
                )
                conversation_trace_event = self._record_loop_event(
                    request,
                    phase="conversation_started",
                    status="running",
                    summary="OpenHands conversation started.",
                    payload={
                        "artifact_target": artifact_type,
                        "workspace_mode": sandbox.workspace_mode,
                        "tools": tool_names,
                        "model": credentials["model"],
                        "base_url_configured": bool(credentials["base_url"]),
                    },
                )
                loop_refs.append(conversation_trace_event.event_id)
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
                    conversation_facts = _conversation_runtime_facts(conversation)
                    if artifact_type in {"planner_order", "execution_result", "planner_chat_turn"} and _extract_structured_artifact(
                        run_output,
                        artifact_type=artifact_type,
                    ) is None:
                        run_output = _with_conversation_event_sources(run_output, conversation)
                except Exception as exc:
                    return self._failed(
                        request,
                        emit=emit,
                        code="openhands_run_failed",
                        message=f"OpenHands conversation failed: {exc}",
                        native_type="conversation.failed",
                        refs=[
                            selected_event.event_id,
                            sandbox_event.event_id,
                            started_event.event_id,
                            *loop_refs,
                        ],
                        payload={"error_type": type(exc).__name__, "message": str(exc)},
                    )

                summary = _summarize_run_output(run_output)
                if artifact_type in {"execution_result", "planner_chat_turn", "workflow_activity_update"}:
                    structured_artifact, execution_extract_error = _extract_single_structured_artifact(
                        run_output,
                        artifact_type=str(artifact_type),
                    )
                else:
                    structured_artifact = _extract_structured_artifact(run_output, artifact_type=str(artifact_type or ""))
                    execution_extract_error = None
                candidate_event = self._record_loop_event(
                    request,
                    phase="artifact_candidate",
                    status="completed",
                    summary="OpenHands output inspected for a structured artifact.",
                    payload={
                        "artifact_target": artifact_type,
                        "output_type": type(run_output).__name__,
                        "structured_artifact_found": structured_artifact is not None,
                        "candidate_artifact_type": _candidate_artifact_type(structured_artifact),
                    },
                )
                loop_refs.append(candidate_event.event_id)
                facts = _merge_runtime_facts(
                    _runtime_facts(run_output),
                    conversation_facts,
                    self._sandbox_facts(request, sandbox),
                )
                facts["native_event_refs"] = _dedupe(
                    [
                        sandbox_event.event_id,
                        *permission_refs,
                        started_event.event_id,
                        *loop_refs,
                        *facts["native_event_refs"],
                    ]
                )
                denied_command = _denied_runtime_command(
                    request=request,
                    commands=facts["commands_run"],
                    sandbox_root=str(workspace),
                )
                if denied_command is not None:
                    denied_decision, denied_command_text = denied_command
                    permission_event = self._record_permission_event(
                        request,
                        decision=denied_decision,
                        command=denied_command_text,
                        summary="Harness permission post-run inspection denied command.",
                    )
                    facts["native_event_refs"] = _dedupe([*facts["native_event_refs"], permission_event.event_id])
                    return self._failed(
                        request,
                        emit=emit,
                        code=denied_decision.code,
                        message=denied_decision.reason,
                        native_type="permission.denied",
                        status="blocked",
                        refs=[
                            selected_event.event_id,
                            sandbox_event.event_id,
                            *facts["native_event_refs"],
                        ],
                        payload={
                            "code": denied_decision.code,
                            "message": denied_decision.reason,
                        },
                    )
                execution_result_status = "completed"
                if artifact_type == "execution_result":
                    if execution_extract_error is not None:
                        validation_event = self._record_loop_event(
                            request,
                            phase="artifact_validation",
                            status="failed",
                            summary="OpenHands execution_result validation failed.",
                            payload={
                                "artifact_target": artifact_type,
                                "validation_status": "failed",
                                "reason": execution_extract_error,
                            },
                        )
                        facts["native_event_refs"] = _dedupe([*facts["native_event_refs"], validation_event.event_id])
                        return self._failed(
                            request,
                            emit=emit,
                            code="insufficient_structured_execution_result",
                            message=_INSUFFICIENT_EXECUTION_RESULT_MESSAGE,
                            native_type="execution_result.insufficient",
                            status="blocked",
                            refs=[
                                selected_event.event_id,
                                sandbox_event.event_id,
                                started_event.event_id,
                                *facts["native_event_refs"],
                            ],
                            payload={
                                "code": "insufficient_structured_execution_result",
                                "message": _INSUFFICIENT_EXECUTION_RESULT_MESSAGE,
                                "reason": execution_extract_error,
                                "structured_artifact_found": structured_artifact is not None,
                                "output_type": type(run_output).__name__,
                            },
                        )
                    provisional_refs = _dedupe(
                        [
                            selected_event.event_id,
                            sandbox_event.event_id,
                            started_event.event_id,
                            *facts["native_event_refs"],
                            *facts["evidence_refs"],
                        ]
                    )
                    execution_artifact, execution_error = _execution_result_artifact_from_structured(
                        structured_artifact,
                        request=request,
                        summary=summary,
                        evidence_refs=provisional_refs,
                        facts=facts,
                    )
                    if execution_error is not None:
                        validation_event = self._record_loop_event(
                            request,
                            phase="artifact_validation",
                            status="failed",
                            summary="OpenHands execution_result validation failed.",
                            payload={
                                "artifact_target": artifact_type,
                                "validation_status": "failed",
                                "reason": execution_error,
                            },
                        )
                        facts["native_event_refs"] = _dedupe([*facts["native_event_refs"], validation_event.event_id])
                        return self._failed(
                            request,
                            emit=emit,
                            code="insufficient_structured_execution_result",
                            message=_INSUFFICIENT_EXECUTION_RESULT_MESSAGE,
                            native_type="execution_result.insufficient",
                            status="blocked",
                            refs=[
                                selected_event.event_id,
                                sandbox_event.event_id,
                                started_event.event_id,
                                *facts["native_event_refs"],
                            ],
                            payload={
                                "code": "insufficient_structured_execution_result",
                                "message": _INSUFFICIENT_EXECUTION_RESULT_MESSAGE,
                                "reason": execution_error,
                                "structured_artifact_found": structured_artifact is not None,
                                "output_type": type(run_output).__name__,
                            },
                        )
                    structured_artifact = execution_artifact
                    execution_result_status = str(execution_artifact.get("status") or "completed")
                    validation_event = self._record_loop_event(
                        request,
                        phase="artifact_validation",
                        status="completed",
                        summary="OpenHands execution_result validation passed.",
                        payload={
                            "artifact_target": artifact_type,
                            "validation_status": "passed",
                            "artifact_type": "execution_result",
                            "status": execution_result_status,
                        },
                    )
                    facts["native_event_refs"] = _dedupe([*facts["native_event_refs"], validation_event.event_id])
                if artifact_type == "planner_order":
                    planner_artifact, planner_error = _planner_order_artifact_from_structured(
                        structured_artifact,
                        request=request,
                        summary=summary,
                    )
                    if planner_error is not None:
                        validation_event = self._record_loop_event(
                            request,
                            phase="artifact_validation",
                            status="failed",
                            summary="OpenHands planner_order validation failed.",
                            payload={
                                "artifact_target": artifact_type,
                                "validation_status": "failed",
                                "reason": planner_error,
                            },
                        )
                        facts["native_event_refs"] = _dedupe([*facts["native_event_refs"], validation_event.event_id])
                        return self._failed(
                            request,
                            emit=emit,
                            code="insufficient_structured_planner_output",
                            message=_INSUFFICIENT_PLANNER_OUTPUT_MESSAGE,
                            native_type="planner_output.insufficient",
                            status="blocked",
                            refs=[
                                selected_event.event_id,
                                sandbox_event.event_id,
                                started_event.event_id,
                                *facts["native_event_refs"],
                            ],
                            payload={
                                "code": "insufficient_structured_planner_output",
                                "message": _INSUFFICIENT_PLANNER_OUTPUT_MESSAGE,
                                "reason": planner_error,
                                "structured_artifact_found": structured_artifact is not None,
                                "output_type": type(run_output).__name__,
                            },
                        )
                    structured_artifact = planner_artifact
                    validation_event = self._record_loop_event(
                        request,
                        phase="artifact_validation",
                        status="completed",
                        summary="OpenHands planner_order validation passed.",
                        payload={
                            "artifact_target": artifact_type,
                            "validation_status": "passed",
                            "artifact_type": "planner_order",
                            "status": "completed",
                        },
                    )
                    facts["native_event_refs"] = _dedupe([*facts["native_event_refs"], validation_event.event_id])
                if artifact_type == "planner_chat_turn":
                    if execution_extract_error is not None:
                        validation_event = self._record_loop_event(
                            request,
                            phase="artifact_validation",
                            status="failed",
                            summary="OpenHands planner_chat_turn validation failed.",
                            payload={
                                "artifact_target": artifact_type,
                                "validation_status": "failed",
                                "reason": execution_extract_error,
                            },
                        )
                        facts["native_event_refs"] = _dedupe([*facts["native_event_refs"], validation_event.event_id])
                        return self._failed(
                            request,
                            emit=emit,
                            code="insufficient_structured_planner_chat_turn",
                            message=_INSUFFICIENT_PLANNER_CHAT_TURN_MESSAGE,
                            native_type="planner_chat_turn.insufficient",
                            status="blocked",
                            refs=[
                                selected_event.event_id,
                                sandbox_event.event_id,
                                started_event.event_id,
                                *facts["native_event_refs"],
                            ],
                            payload={
                                "code": "insufficient_structured_planner_chat_turn",
                                "message": _INSUFFICIENT_PLANNER_CHAT_TURN_MESSAGE,
                                "reason": execution_extract_error,
                                "structured_artifact_found": structured_artifact is not None,
                                "output_type": type(run_output).__name__,
                            },
                        )
                    chat_artifact, chat_error = _planner_chat_turn_artifact_from_structured(
                        structured_artifact,
                        request=request,
                    )
                    if chat_error is not None:
                        validation_event = self._record_loop_event(
                            request,
                            phase="artifact_validation",
                            status="failed",
                            summary="OpenHands planner_chat_turn validation failed.",
                            payload={
                                "artifact_target": artifact_type,
                                "validation_status": "failed",
                                "reason": chat_error,
                            },
                        )
                        facts["native_event_refs"] = _dedupe([*facts["native_event_refs"], validation_event.event_id])
                        return self._failed(
                            request,
                            emit=emit,
                            code="insufficient_structured_planner_chat_turn",
                            message=_INSUFFICIENT_PLANNER_CHAT_TURN_MESSAGE,
                            native_type="planner_chat_turn.insufficient",
                            status="blocked",
                            refs=[
                                selected_event.event_id,
                                sandbox_event.event_id,
                                started_event.event_id,
                                *facts["native_event_refs"],
                            ],
                            payload={
                                "code": "insufficient_structured_planner_chat_turn",
                                "message": _INSUFFICIENT_PLANNER_CHAT_TURN_MESSAGE,
                                "reason": chat_error,
                                "structured_artifact_found": structured_artifact is not None,
                                "output_type": type(run_output).__name__,
                            },
                        )
                    structured_artifact = chat_artifact
                    validation_event = self._record_loop_event(
                        request,
                        phase="artifact_validation",
                        status="completed",
                        summary="OpenHands planner_chat_turn validation passed.",
                        payload={
                            "artifact_target": artifact_type,
                            "validation_status": "passed",
                            "artifact_type": "planner_chat_turn",
                            "decision": chat_artifact.get("decision"),
                            "interaction_mode": chat_artifact.get("interaction_mode"),
                        },
                    )
                    facts["native_event_refs"] = _dedupe([*facts["native_event_refs"], validation_event.event_id])
                if artifact_type == "workflow_activity_update":
                    if execution_extract_error is not None:
                        validation_event = self._record_loop_event(
                            request,
                            phase="artifact_validation",
                            status="failed",
                            summary="OpenHands workflow_activity_update validation failed.",
                            payload={
                                "artifact_target": artifact_type,
                                "validation_status": "failed",
                                "reason": execution_extract_error,
                            },
                        )
                        facts["native_event_refs"] = _dedupe([*facts["native_event_refs"], validation_event.event_id])
                        return self._failed(
                            request,
                            emit=emit,
                            code="insufficient_structured_workflow_activity_update",
                            message=_INSUFFICIENT_WORKFLOW_ACTIVITY_UPDATE_MESSAGE,
                            native_type="workflow_activity_update.insufficient",
                            status="blocked",
                            refs=[
                                selected_event.event_id,
                                sandbox_event.event_id,
                                started_event.event_id,
                                *facts["native_event_refs"],
                            ],
                            payload={
                                "code": "insufficient_structured_workflow_activity_update",
                                "message": _INSUFFICIENT_WORKFLOW_ACTIVITY_UPDATE_MESSAGE,
                                "reason": execution_extract_error,
                                "structured_artifact_found": structured_artifact is not None,
                                "output_type": type(run_output).__name__,
                            },
                        )
                    activity_artifact, activity_error = _workflow_activity_update_artifact_from_structured(
                        structured_artifact
                    )
                    if activity_error is not None:
                        validation_event = self._record_loop_event(
                            request,
                            phase="artifact_validation",
                            status="failed",
                            summary="OpenHands workflow_activity_update validation failed.",
                            payload={
                                "artifact_target": artifact_type,
                                "validation_status": "failed",
                                "reason": activity_error,
                            },
                        )
                        facts["native_event_refs"] = _dedupe([*facts["native_event_refs"], validation_event.event_id])
                        return self._failed(
                            request,
                            emit=emit,
                            code="insufficient_structured_workflow_activity_update",
                            message=_INSUFFICIENT_WORKFLOW_ACTIVITY_UPDATE_MESSAGE,
                            native_type="workflow_activity_update.insufficient",
                            status="blocked",
                            refs=[
                                selected_event.event_id,
                                sandbox_event.event_id,
                                started_event.event_id,
                                *facts["native_event_refs"],
                            ],
                            payload={
                                "code": "insufficient_structured_workflow_activity_update",
                                "message": _INSUFFICIENT_WORKFLOW_ACTIVITY_UPDATE_MESSAGE,
                                "reason": activity_error,
                                "structured_artifact_found": structured_artifact is not None,
                                "output_type": type(run_output).__name__,
                            },
                        )
                    structured_artifact = activity_artifact
                    validation_event = self._record_loop_event(
                        request,
                        phase="artifact_validation",
                        status="completed",
                        summary="OpenHands workflow_activity_update validation passed.",
                        payload={
                            "artifact_target": artifact_type,
                            "validation_status": "passed",
                            "artifact_type": "workflow_activity_update",
                            "visible_phase": activity_artifact.get("visible_phase"),
                        },
                    )
                    facts["native_event_refs"] = _dedupe([*facts["native_event_refs"], validation_event.event_id])
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
        result_status = execution_result_status if artifact_type == "execution_result" else "completed"
        final_phase = "completed" if result_status == "completed" else "blocked"
        loop_final_event = self._record_loop_event(
            request,
            phase=final_phase,
            status=result_status,
            summary="OpenHands harness loop completed." if final_phase == "completed" else "OpenHands harness loop blocked.",
            payload={
                "artifact_type": artifact_type,
                "status": result_status,
                "evidence_refs_count": len(facts["evidence_refs"]),
                "diff_refs_count": len(facts["diff_refs"]),
                "log_refs_count": len(facts["log_refs"]),
                "changed_files_count": len(facts["changed_files"]),
                "created_files_count": len(facts["created_files"]),
                "deleted_files_count": len(facts["deleted_files"]),
            },
        )
        refs = [
            selected_event.event_id,
            *facts["native_event_refs"],
            completed_event.event_id,
            loop_final_event.event_id,
        ]
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
            status=result_status,
            artifact_type=artifact_type,
            artifact=_artifact_for_success(
                request,
                artifact_type=artifact_type,
                summary=summary,
                evidence_refs=evidence_refs,
                facts=facts,
                structured_artifact=structured_artifact,
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
        os.environ.setdefault("OPENHANDS_SUPPRESS_BANNER", "1")
        os.environ.setdefault("LITELLM_LOCAL_MODEL_COST_MAP", "True")
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
            return [
                sdk.TerminalTool.name,
                sdk.FileEditorTool.name,
                sdk.TaskTrackerTool.name,
                "coder_hybrid_rag_search",
            ]
        return [sdk.TaskTrackerTool.name, "coder_hybrid_rag_search"]

    def _tools_for_request(self, request: HarnessRunRequest, sdk: Any) -> list[Any]:
        if str(getattr(sdk.Tool, "__module__", "")).startswith("openhands."):
            _ensure_coder_openhands_tools_registered()
        tools: list[Any] = []
        for name in self._tool_names_for_request(request, sdk):
            if name == "coder_hybrid_rag_search":
                tools.append(sdk.Tool(name=name, params=_rag_tool_params_for_request(request)))
            else:
                tools.append(sdk.Tool(name=name))
        return tools

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
        artifact_target = _artifact_type_for_request(request)[0]
        loop_event = self._record_loop_event(
            request,
            phase="blocked" if status == "blocked" else "failed",
            status=status,
            summary=message,
            payload={
                "code": code,
                "message": message,
                "artifact_target": artifact_target,
            },
        )
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
        native_event_refs = _dedupe([*(refs or []), loop_event.event_id, event.event_id])
        return HarnessRunResult(
            status=status,
            artifact_type=artifact_target if status == "blocked" else None,
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

    def _record_loop_event(
        self,
        request: HarnessRunRequest,
        *,
        phase: str,
        status: str,
        summary: str,
        payload: dict[str, Any],
    ) -> NativeRuntimeEvent:
        artifact_target = payload.get("artifact_target") or _artifact_type_for_request(request)[0]
        safe_payload = _safe_trace_payload(
            {
                "mode": request.mode,
                "artifact_target": artifact_target,
                "harness_id": request.profile.harness_id,
                "provider_id": self.provider_id,
                **payload,
            }
        )
        return self._record_event(
            request,
            native_type=f"harness_loop.{phase}",
            status=status,
            summary=summary,
            payload=safe_payload,
        )

    def _record_permission_event(
        self,
        request: HarnessRunRequest,
        *,
        decision: HarnessPermissionDecision,
        summary: str,
        tool_name: str | None = None,
        action_type: str | None = None,
        file_path: str | None = None,
        command: str | None = None,
    ) -> NativeRuntimeEvent:
        payload = _safe_permission_payload(
            {
                "mode": request.mode,
                "harness_id": request.profile.harness_id,
                "provider_id": self.provider_id,
                "tool_name": tool_name,
                "action_type": action_type,
                "file_path": file_path,
                "command": command,
                "allowed": decision.allowed,
                "requires_confirmation": decision.requires_confirmation,
                "code": decision.code,
                "reason": decision.reason,
            }
        )
        return self._record_event(
            request,
            native_type="harness_permission.allowed" if decision.allowed else "harness_permission.denied",
            status="completed" if decision.allowed else "blocked",
            summary=summary,
            payload=payload,
        )

    def _emit(self, emit: Any | None, event_type: str, message: str, **payload: Any) -> None:
        if emit is None:
            return
        emit(event_type, message, **payload)


def _llm_credentials() -> dict[str, str | None]:
    profile = resolve_llm_provider_profile()
    base_url = os.getenv("LLM_BASE_URL") or profile.base_url
    model = normalize_llm_model(os.getenv("LLM_MODEL") or profile.default_model, profile=profile, base_url=base_url)
    return {
        "api_key": _credential_for_profile(profile.auth_env_candidates),
        "model": model,
        "base_url": base_url,
    }


def _credential_for_profile(candidates: list[str]) -> str | None:
    for name in candidates:
        if value := os.getenv(name):
            return value
    return None


def _normalize_deepseek_model(model: str, *, base_url: str | None) -> str:
    text = model.strip() or "deepseek-v4-flash"
    if "/" in text:
        return text
    if text.startswith("deepseek-") or text in {"deepseek-chat", "deepseek-reasoner"}:
        return f"deepseek/{text}"
    if "deepseek.com" in str(base_url or "").lower() and text.startswith("v"):
        return f"deepseek/{text}"
    return text


def _safe_trace_payload(payload: dict[str, Any]) -> dict[str, Any]:
    allowed_keys = {
        "artifact_target",
        "artifact_type",
        "base_url_configured",
        "candidate_artifact_type",
        "changed_files_count",
        "code",
        "contract_id",
        "contract_kind",
        "created_files_count",
        "deleted_files_count",
        "diff_refs",
        "diff_refs_count",
        "evidence_refs",
        "evidence_refs_count",
        "has_output_contract",
        "harness_id",
        "log_refs",
        "log_refs_count",
        "message",
        "mode",
        "model",
        "native_event_refs",
        "output_type",
        "profile_id",
        "prompt_length_chars",
        "provider_id",
        "reason",
        "reason_code",
        "status",
        "structured_artifact_found",
        "summary",
        "tools",
        "validation_status",
        "workspace_mode",
    }
    safe: dict[str, Any] = {}
    for key, value in payload.items():
        if key not in allowed_keys:
            continue
        safe[key] = _compact_trace_value(value)
    return safe


def _safe_permission_payload(payload: dict[str, Any]) -> dict[str, Any]:
    allowed_keys = {
        "action_type",
        "allowed",
        "code",
        "command",
        "file_path",
        "harness_id",
        "mode",
        "provider_id",
        "reason",
        "requires_confirmation",
        "tool_name",
    }
    safe: dict[str, Any] = {}
    for key, value in payload.items():
        if key not in allowed_keys or value in (None, "", [], {}):
            continue
        if key == "command":
            safe[key] = _command_summary(str(value))
            continue
        safe[key] = _compact_trace_value(value)
    return safe


def _compact_trace_value(value: Any) -> Any:
    if isinstance(value, str):
        text = value
        for marker in _TRACE_REDACTIONS:
            text = re.sub(re.escape(marker), "<redacted>", text, flags=re.IGNORECASE)
        if len(text) > 500:
            return f"{text[:500]}...<truncated>"
        return text
    if isinstance(value, bool) or value is None or isinstance(value, int):
        return value
    if isinstance(value, (list, tuple)):
        return [_compact_trace_value(item) for item in list(value)[:20]]
    if isinstance(value, dict):
        return {
            str(key): _compact_trace_value(item)
            for key, item in list(value.items())[:20]
            if str(key) not in {"api_key", "secret", "token", "password"}
        }
    return _compact_trace_value(str(value))


def _command_summary(command: str) -> str:
    text = " ".join(command.strip().split())
    text = re.sub(r"(?i)(api[_-]?key|token|secret|password)(\s*=\s*|\s+)[^\s]+", r"\1\2<redacted>", text)
    text = re.sub(r"(?i)--(api-key|token|secret|password)(=|\s+)[^\s]+", r"--\1\2<redacted>", text)
    return _compact_trace_value(text)


def _candidate_artifact_type(structured_artifact: Any) -> str | None:
    if isinstance(structured_artifact, dict):
        artifact_type = _string_value(structured_artifact.get("artifact_type"))
        return artifact_type or None
    return None


def _denied_runtime_command(
    *,
    request: HarnessRunRequest,
    commands: list[str],
    sandbox_root: str,
) -> tuple[HarnessPermissionDecision, str] | None:
    if request.mode != "task_execution":
        return None
    for command in commands:
        decision = evaluate_harness_permission(
            mode=request.mode,
            harness_id=request.profile.harness_id,
            role="executor",
            action_type="run_command",
            command=str(command),
            sandbox_root=sandbox_root,
        )
        if not decision.allowed:
            return decision, str(command)
    return None


def _ensure_coder_openhands_tools_registered() -> None:
    import coder_workbench.openhands_tools.hybrid_rag_search  # noqa: F401


def _rag_tool_params_for_request(request: HarnessRunRequest) -> dict[str, Any]:
    role, requested_context, max_tokens = {
        "planning_chat": ("planning_chat", "assistant_message", 4000),
        "workflow_supervisor": ("workflow_supervisor", "workflow_supervision", 3000),
        "task_execution": ("task_execution", "execution_prompt", 2000),
    }[request.mode]
    context_packet = request.context.context_packet or {}
    initial_data = request.context.initial_data or {}
    session_id = None
    if request.mode != "task_execution":
        session_id = _optional_string(
            initial_data.get("planner_chat_session_id")
            or _dig(context_packet, "hot", "planner_chat_session_id")
        )
    return {
        "memory_root": str(_memory_root_for_request(request)),
        "role": role,
        "requested_context": requested_context,
        "project_id": _optional_string(initial_data.get("project_id") or request.context.repo_root),
        "session_id": session_id,
        "run_id": _optional_string(request.context.run_id or initial_data.get("run_id")),
        "scope_paths": _scope_paths_for_request(request),
        "max_tokens": max_tokens,
    }


def _memory_root_for_request(request: HarnessRunRequest) -> Path:
    initial_data = request.context.initial_data or {}
    for key in ("memory_store_root", "coder_store_root", "skill_store_root"):
        value = initial_data.get(key)
        if isinstance(value, str) and value.strip():
            return Path(value)
    if request.context.repo_root:
        return Path(request.context.repo_root) / ".coder"
    return Path(".coder")


def _scope_paths_for_request(request: HarnessRunRequest) -> list[str]:
    initial_data = request.context.initial_data or {}
    value = initial_data.get("scopes") or initial_data.get("scope")
    if value is None and request.mode != "task_execution":
        value = _dig(request.context.context_packet or {}, "hot", "scope")
    if isinstance(value, str):
        return [value]
    if isinstance(value, list):
        return [str(item) for item in value if str(item).strip()]
    return []


def _optional_string(value: Any) -> str | None:
    text = str(value or "").strip()
    return text or None


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
                "Do not read or write sensitive credential files such as .env, .ssh, .aws, .kube, cloud, or Docker credentials.",
                "Do not open editors, pagers, or interactive commands.",
                "Return enough structured information for Coder to project a valid execution_result artifact.",
                "Return a concise completion summary and verification evidence.",
            ]
        )
        _append_artifact_output_contract(lines, artifact_target)
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
        _append_section(lines, "Planner task state", _dig(context_packet, "warm", "planner_task_state"))
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
        if artifact_target == "final_report":
            lines.extend(
                [
                    "Final report evidence rules:",
                    "- Do not claim tests passed unless execution_result verification has pass evidence.",
                    "- If verification was skipped, report it as skipped or unchecked, not passed.",
                    "- If any executor result is blocked, produce a clear user-facing blocker.",
                ]
            )
        _append_artifact_output_contract(lines, artifact_target)
        return "\n\n".join(lines)
    lines.extend(
        [
            "You are the Planning Chat Harness.",
            "Do not execute commands, modify files, or start the live run.",
        ]
    )
    if artifact_target == "planner_chat_turn":
        lines.extend(
            [
                "Talk with the user, maintain task state, and decide whether the task is ready for Work mode handoff.",
                "Return enough structured information for Coder to project a valid planner_chat_turn artifact.",
            ]
        )
        _append_artifact_output_contract(lines, artifact_target)
        _append_section(lines, "Interaction mode", _planner_interaction_mode(request))
        _append_section(lines, "Recent messages", request.input_artifacts.get("messages"))
        _append_section(lines, "Current task state", request.input_artifacts.get("task_state"))
    else:
        lines.extend(
            [
                "Produce a draft only.",
                "Return enough structured information for Coder to project a valid project_plan_draft artifact.",
            ]
        )
    _append_section(lines, "User request", request.input_artifacts.get("user_request") or _dig(context_packet, "hot", "user_goal"))
    _append_section(lines, "Workflow summary", _dig(context_packet, "warm", "workflow_summary"))
    _append_section(lines, "Selected knowledge pack IDs", _dig(context_packet, "hot", "selected_knowledge_pack_ids"))
    _append_section(lines, "Selected skill pack IDs", _dig(context_packet, "hot", "selected_skill_pack_ids"))
    _append_section(lines, "Selected memory pack IDs", _dig(context_packet, "hot", "selected_memory_pack_ids"))
    return "\n\n".join(lines)


def _append_artifact_output_contract(lines: list[str], artifact_target: str) -> None:
    if artifact_target == "planner_order":
        _append_planner_order_output_contract(lines)
    elif artifact_target == "execution_result":
        _append_execution_result_output_contract(lines)
    elif artifact_target == "planner_chat_turn":
        _append_planner_chat_turn_output_contract(lines)
    elif artifact_target == "workflow_activity_update":
        _append_workflow_activity_update_output_contract(lines)


def artifact_output_contract_available(artifact_target: str) -> bool:
    return artifact_target in {"planner_order", "execution_result", "planner_chat_turn", "workflow_activity_update"}


def _append_workflow_activity_update_output_contract(lines: list[str]) -> None:
    lines.extend(
        [
            "OpenHands Workflow Activity Update Contract v1:",
            "Return exactly one JSON object.",
            "Do not return prose before or after the JSON object.",
            'The JSON object must have "artifact_type": "workflow_activity_update".',
            "Use concise user-facing status. Do not include full logs, full diffs, raw prompts, raw runtime JSON, or model outputs.",
            _safe_json(
                {
                    "artifact_type": "workflow_activity_update",
                    "visible_phase": "executing",
                    "user_message": "Executor work is in progress.",
                    "steps": [
                        {"id": "plan", "label": "Plan", "status": "done"},
                        {"id": "execute", "label": "Execute", "status": "active"},
                    ],
                    "safety": [{"policy": "readonly_supervisor", "status": "enforced"}],
                    "technical_refs": {"evidence_refs": ["native-event-ref"]},
                }
            ),
        ]
    )


def _append_planner_chat_turn_output_contract(lines: list[str]) -> None:
    lines.extend(
        [
            "OpenHands Structured Planner Chat Turn Contract v1:",
            "Return exactly one JSON object.",
            "Do not return prose before or after the JSON object.",
            "Do not wrap the JSON in Markdown unless the runtime forces Markdown formatting.",
            'The JSON object must have "artifact_type": "planner_chat_turn".',
            "interaction_mode is provided by Coder and must be preserved exactly.",
            'In discuss mode, never return decision = "start_workflow".',
            'In work mode, return decision = "start_workflow" only when task_state.readiness = "ready_to_execute".',
            'If the task is not ready, ask a concise clarifying question and use decision = "continue_chat" or "blocked_needs_clarification".',
            "visible_thinking is a short user-visible status summary, not private chain-of-thought.",
            "Do not include private chain-of-thought, hidden scratchpads, raw prompts, logs, diffs, secrets, or model internals.",
            "task_state is the source of truth for the evolving plan.",
            _safe_json(
                {
                    "artifact_type": "planner_chat_turn",
                    "assistant_message": "Concise user-facing response.",
                    "interaction_mode": "discuss",
                    "decision": "continue_chat",
                    "visible_thinking": {
                        "phase": "clarifying",
                        "summary": "Clarifying the task scope.",
                    },
                    "task_state": {
                        "goal": "User goal if known.",
                        "user_intent": "Brief interpretation of the user's intent.",
                        "scope": [],
                        "constraints": [],
                        "success_criteria": [],
                        "known_context": [],
                        "missing_context": [],
                        "open_questions": ["One concise question if clarification is needed."],
                        "assumptions": [],
                        "risks": [],
                        "plan_steps": [],
                        "readiness": "needs_clarification",
                    },
                    "handoff": None,
                }
            ),
        ]
    )


def _append_planner_order_output_contract(lines: list[str]) -> None:
    lines.extend(
        [
            "Output contract for Coder:",
            "Return exactly one JSON object.",
            "Do not return prose before or after the JSON object.",
            "Do not wrap the JSON in Markdown unless the runtime forces Markdown formatting.",
            "The JSON object must be one of the following two shapes.",
            "",
            "Shape A: actionable planner_order with executor work:",
            _safe_json(
                {
                    "artifact_type": "planner_order",
                    "round": 1,
                    "round_goal": "One concise sentence describing the current execution round.",
                    "plan_graph": {
                        "work_items": [
                            {
                                "work_item_id": "executor-work-1",
                                "merge_index": 1,
                                "assignee_agent_id": "executor",
                                "task_summary": "Concrete executor task summary.",
                                "depends_on": [],
                            }
                        ]
                    },
                    "instructions_for_executor": ["Concrete bounded implementation instructions."],
                    "allowed_actions": ["modify_files", "run_commands"],
                    "forbidden_actions": ["commit", "push", "deploy"],
                    "expected_outputs": ["execution_result with evidence refs"],
                    "risk_level": "low",
                    "requires_human_confirmation": False,
                }
            ),
            "",
            "Shape B: explicit no-work planner_order:",
            _safe_json(
                {
                    "artifact_type": "planner_order",
                    "round": 1,
                    "round_goal": "No executor action is required.",
                    "plan_graph": {"work_items": []},
                    "no_work_rationale": (
                        "Explain why the request is already satisfied or why no executor work is needed."
                    ),
                    "instructions_for_executor": [],
                    "allowed_actions": [],
                    "forbidden_actions": ["write_files", "run_commands", "commit", "push", "deploy"],
                    "expected_outputs": ["No executor output is expected."],
                    "risk_level": "low",
                    "requires_human_confirmation": False,
                }
            ),
            "",
            "Rules:",
            "- Use Shape A when the user request requires repository changes, checks, investigation, or implementation.",
            "- Use Shape B only when no executor work is genuinely needed.",
            "- Never return an empty work_items list unless no_work_rationale is present.",
            "- If you cannot produce one of these JSON objects, return the closest valid blocked planner_order JSON instead of prose.",
            "- The JSON is the answer. Do not add explanation outside it.",
        ]
    )


def _append_execution_result_output_contract(lines: list[str]) -> None:
    lines.extend(
        [
            "OpenHands Structured Execution Result Contract v1:",
            "Return exactly one JSON object.",
            "Do not return prose before or after the JSON object.",
            "Do not wrap the JSON in Markdown unless the runtime forces Markdown formatting.",
            'The JSON object must include "artifact_type": "execution_result".',
            "The JSON object must be one of the following three shapes.",
            "",
            "Shape A: completed execution_result with evidence:",
            _safe_json(
                {
                    "artifact_type": "execution_result",
                    "round": 1,
                    "work_item_id": "executor-work-1",
                    "agent_id": "executor",
                    "status": "completed",
                    "summary": "Implemented the requested change.",
                    "changed_files": ["path/to/file.py"],
                    "created_files": [],
                    "deleted_files": [],
                    "patch_refs": [],
                    "attempted_actions": ["python -m unittest discover tests"],
                    "evidence_refs": [],
                    "verification": {
                        "status": "pass",
                        "checks_run": [
                            {
                                "check_id": "check-1",
                                "kind": "command",
                                "command": "python -m unittest discover tests",
                                "status": "pass",
                                "summary": "All tests passed.",
                            }
                        ],
                        "evidence_refs": [],
                        "confidence": "medium",
                    },
                }
            ),
            "",
            "Shape B: completed no-op execution_result:",
            _safe_json(
                {
                    "artifact_type": "execution_result",
                    "round": 1,
                    "work_item_id": "executor-work-1",
                    "agent_id": "executor",
                    "status": "completed",
                    "summary": "No changes were needed.",
                    "changed_files": [],
                    "created_files": [],
                    "deleted_files": [],
                    "patch_refs": [],
                    "attempted_actions": [],
                    "evidence_refs": [],
                    "no_op_rationale": "Explain why no changes were needed.",
                    "verification": {
                        "status": "skipped",
                        "checks_run": [],
                        "no_check_rationale": "Explain why no checks were run.",
                        "evidence_refs": [],
                        "confidence": "medium",
                    },
                }
            ),
            "",
            "Shape C: blocked execution_result:",
            _safe_json(
                {
                    "artifact_type": "execution_result",
                    "round": 1,
                    "work_item_id": "executor-work-1",
                    "agent_id": "executor",
                    "status": "blocked",
                    "summary": "Execution could not proceed.",
                    "blocker_type": "test_failed",
                    "blocker_reason": "Explain the blocker.",
                    "executor_recovery_exhausted": True,
                    "planner_recommendation": "finish",
                    "remaining_work": ["Describe remaining work."],
                    "unexpected_issues": ["Describe unexpected issue."],
                    "evidence_refs": [],
                    "verification": {
                        "status": "blocked",
                        "checks_run": [],
                        "evidence_refs": [],
                        "confidence": "medium",
                        "remaining_work": ["Describe remaining work."],
                    },
                }
            ),
            "",
            "Rules:",
            "- Runtime-observed file changes, patch refs, logs, evidence refs, commands, and checks are the source of truth.",
            "- verification.status may be pass only when an explicit check command actually passed.",
            "- If no checks were run, use verification.status skipped and include no_check_rationale.",
            "- If no files changed, no commands ran, no checks ran, and no no_op_rationale applies, return Shape C.",
            "- Do not claim commits, pushes, deploys, user interaction, or long-term memory writes.",
            "- The JSON is the answer. Do not add explanation outside it.",
        ]
    )


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


def _with_conversation_event_sources(run_output: Any, conversation: Any) -> Any:
    event_texts = _conversation_agent_event_texts(conversation)
    if not event_texts:
        return run_output
    if run_output is None:
        return event_texts
    return [run_output, *event_texts]


def _conversation_agent_event_texts(conversation: Any) -> list[str]:
    event_list = _conversation_events(conversation)
    if not event_list:
        return []

    texts: list[str] = []
    for event in reversed(event_list):
        if str(getattr(event, "source", "")).lower() != "agent":
            continue
        finish_message = _finish_action_message(event)
        if finish_message:
            texts.append(finish_message)
            continue
        message = getattr(event, "llm_message", None)
        text = _content_text(getattr(message, "content", None))
        if text:
            texts.append(text)
    return texts


def _conversation_runtime_facts(conversation: Any) -> dict[str, Any]:
    facts = _empty_runtime_facts()
    action_by_id: dict[str, str] = {}
    for event in _conversation_events(conversation):
        event_id = _string_value(getattr(event, "id", None))
        tool_name = str(getattr(event, "tool_name", "") or "").strip().lower()
        source = str(getattr(event, "source", "") or "").strip().lower()
        action = getattr(event, "action", None)
        if source == "agent" and action is not None:
            action_summary = _action_attempt_summary(tool_name, action)
            if action_summary:
                facts["commands_run"].append(action_summary)
                if event_id:
                    action_by_id[event_id] = action_summary
            continue

        observation = getattr(event, "observation", None)
        if observation is None:
            continue
        if tool_name != "terminal":
            continue
        command = _string_value(getattr(observation, "command", None)) or action_by_id.get(
            _string_value(getattr(event, "action_id", None)),
            "",
        )
        if not command:
            continue
        facts["checks_run"].append(_terminal_check_record(command, observation, event_id=event_id))
    facts["commands_run"] = _dedupe(facts["commands_run"])
    facts["checks_run"] = _dedupe_checks(facts["checks_run"])
    return facts


def _conversation_events(conversation: Any) -> list[Any]:
    state = getattr(conversation, "state", None)
    events = getattr(state, "events", None)
    if events is None:
        return []
    try:
        return list(events)
    except TypeError:
        return []


def _finish_action_message(event: Any) -> str:
    if str(getattr(event, "tool_name", "")).lower() != "finish":
        return ""
    action = getattr(event, "action", None)
    return _string_value(getattr(action, "message", None))


def _action_attempt_summary(tool_name: str, action: Any) -> str:
    if tool_name == "terminal":
        command = _string_value(getattr(action, "command", None))
        if not command or getattr(action, "is_input", False):
            return ""
        return command
    if tool_name == "file_editor":
        command = _string_value(getattr(action, "command", None))
        path = _string_value(getattr(action, "path", None))
        if not command or command == "view":
            return ""
        return " ".join(part for part in ("file_editor", command, path) if part)
    return ""


def _terminal_check_record(command: str, observation: Any, *, event_id: str) -> dict[str, Any]:
    exit_code = _observation_exit_code(observation)
    is_error = bool(getattr(observation, "is_error", False))
    timeout = bool(getattr(observation, "timeout", False))
    if timeout or exit_code == -1:
        status = "blocked"
    elif is_error or (exit_code is not None and exit_code != 0):
        status = "fail"
    elif exit_code == 0:
        status = "pass"
    else:
        status = "skipped"
    return {
        "check_id": event_id or None,
        "kind": "command",
        "command": command,
        "status": status,
        "summary": _observation_summary(observation) or command,
    }


def _observation_exit_code(observation: Any) -> int | None:
    value = getattr(observation, "exit_code", None)
    if value is None:
        metadata = getattr(observation, "metadata", None)
        value = getattr(metadata, "exit_code", None)
    try:
        return int(value) if value is not None else None
    except (TypeError, ValueError):
        return None


def _observation_summary(observation: Any) -> str:
    text = _string_value(getattr(observation, "text", None))
    if not text:
        return ""
    first_line = next((line.strip() for line in text.splitlines() if line.strip()), "")
    return first_line[:240]


def _content_text(content: Any) -> str:
    if isinstance(content, str):
        return content.strip()
    if not isinstance(content, list):
        return ""
    parts: list[str] = []
    for item in content:
        if isinstance(item, str):
            parts.append(item)
            continue
        if isinstance(item, dict):
            text = item.get("text")
            if isinstance(text, str):
                parts.append(text)
            continue
        text = getattr(item, "text", None)
        if isinstance(text, str):
            parts.append(text)
    return "\n".join(part.strip() for part in parts if part.strip())


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
        "planning_chat": {"project_plan_draft", "planner_chat_turn"},
        "task_execution": {"execution_result"},
        "workflow_supervisor": {"planner_order", "planner_decision", "final_report", "workflow_activity_update"},
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
    structured_artifact: dict[str, Any] | None = None,
) -> dict[str, Any]:
    if structured_artifact is not None:
        if artifact_type == "planner_order":
            return structured_artifact
        if artifact_type == "execution_result":
            return structured_artifact
        if artifact_type == "planner_decision":
            return _planner_decision_artifact_from_structured(structured_artifact, request=request, summary=summary)
        if artifact_type == "final_report":
            return _final_report_artifact_from_structured(
                structured_artifact,
                request=request,
                summary=summary,
                evidence_refs=evidence_refs,
            )
        if artifact_type == "project_plan_draft":
            return _project_plan_draft_from_structured(structured_artifact, request=request, summary=summary)
        if artifact_type == "planner_chat_turn":
            return dict(structured_artifact)
        if artifact_type == "workflow_activity_update":
            return dict(structured_artifact)

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
    if _is_model_declared_execution_result(run_output):
        return _empty_runtime_facts()
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


def _is_model_declared_execution_result(run_output: Any) -> bool:
    return isinstance(run_output, dict) and str(run_output.get("artifact_type") or "") == "execution_result"


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


def _extract_structured_artifact(run_output: Any, *, artifact_type: str) -> dict[str, Any] | None:
    seen: set[int] = set()
    for source in _structured_sources(run_output):
        candidate = _structured_candidate(source, artifact_type=artifact_type, seen=seen)
        if candidate is not None:
            return candidate
    return None


def _extract_single_structured_artifact(
    run_output: Any,
    *,
    artifact_type: str,
) -> tuple[dict[str, Any] | None, str | None]:
    candidates = _structured_artifacts(run_output, artifact_type=artifact_type)
    if len(candidates) == 1:
        return candidates[0], None
    if not candidates:
        return None, f"no structured {artifact_type} artifact was found"
    return None, f"expected exactly one structured {artifact_type} artifact, found {len(candidates)}"


def _structured_artifacts(run_output: Any, *, artifact_type: str) -> list[dict[str, Any]]:
    seen_ids: set[int] = set()
    seen_payloads: set[str] = set()
    artifacts: list[dict[str, Any]] = []
    for source in _structured_sources(run_output):
        for candidate in _structured_candidates(source, artifact_type=artifact_type, seen=seen_ids):
            payload_key = json.dumps(candidate, sort_keys=True, default=str)
            if payload_key in seen_payloads:
                continue
            seen_payloads.add(payload_key)
            artifacts.append(candidate)
    return artifacts


def _structured_sources(run_output: Any) -> list[Any]:
    sources = [run_output]
    for attr in (
        "artifact",
        "artifacts",
        "structured_output",
        "output",
        "result",
        "final_message",
        "message",
        "content",
        "summary",
    ):
        if isinstance(run_output, dict):
            if attr in run_output:
                sources.append(run_output[attr])
            continue
        value = getattr(run_output, attr, None)
        if value is not None:
            sources.append(value)
    return sources


def _structured_candidates(value: Any, *, artifact_type: str, seen: set[int]) -> list[dict[str, Any]]:
    if value is None:
        return []
    value_id = id(value)
    if value_id in seen:
        return []
    seen.add(value_id)

    if isinstance(value, dict):
        artifact = _artifact_like_dict(value, artifact_type=artifact_type)
        if artifact is not None:
            return [artifact]
        candidates: list[dict[str, Any]] = []
        for key in ("artifact", "structured_output", "output", "result", "final_message", "message"):
            candidates.extend(_structured_candidates(value.get(key), artifact_type=artifact_type, seen=seen))
        candidates.extend(_structured_candidates(value.get("artifacts"), artifact_type=artifact_type, seen=seen))
        return candidates

    if isinstance(value, list):
        candidates = []
        for item in value:
            candidates.extend(_structured_candidates(item, artifact_type=artifact_type, seen=seen))
        return candidates

    if isinstance(value, tuple):
        candidates = []
        for item in value:
            candidates.extend(_structured_candidates(item, artifact_type=artifact_type, seen=seen))
        return candidates

    if isinstance(value, str):
        final_message_candidates = _final_message_structured_candidates(
            value,
            artifact_type=artifact_type,
            seen=seen,
        )
        if final_message_candidates:
            return final_message_candidates
        candidates = []
        for record in _json_objects_from_text(value):
            candidates.extend(_structured_candidates(record, artifact_type=artifact_type, seen=seen))
        return candidates

    return []


def _final_message_structured_candidates(text: str, *, artifact_type: str, seen: set[int]) -> list[dict[str, Any]]:
    marker = "Finish with message:"
    marker_index = text.rfind(marker)
    if marker_index < 0:
        return []
    final_text = text[marker_index + len(marker) :].strip()
    if not final_text:
        return []
    candidates: list[dict[str, Any]] = []
    for record in _json_objects_from_text(final_text):
        candidates.extend(_structured_candidates(record, artifact_type=artifact_type, seen=seen))
    return candidates


def _structured_candidate(value: Any, *, artifact_type: str, seen: set[int]) -> dict[str, Any] | None:
    if value is None:
        return None
    value_id = id(value)
    if value_id in seen:
        return None
    seen.add(value_id)

    if isinstance(value, dict):
        artifact = _artifact_like_dict(value, artifact_type=artifact_type)
        if artifact is not None:
            return artifact
        for key in ("artifact", "structured_output", "output", "result"):
            nested = _structured_candidate(value.get(key), artifact_type=artifact_type, seen=seen)
            if nested is not None:
                return nested
        artifacts = value.get("artifacts")
        nested = _structured_candidate(artifacts, artifact_type=artifact_type, seen=seen)
        if nested is not None:
            return nested
        return None

    if isinstance(value, list):
        for item in value:
            nested = _structured_candidate(item, artifact_type=artifact_type, seen=seen)
            if nested is not None:
                return nested
        return None

    if isinstance(value, tuple):
        for item in value:
            nested = _structured_candidate(item, artifact_type=artifact_type, seen=seen)
            if nested is not None:
                return nested
        return None

    if isinstance(value, str):
        for record in _json_objects_from_text(value):
            nested = _structured_candidate(record, artifact_type=artifact_type, seen=seen)
            if nested is not None:
                return nested
    return None


def _artifact_like_dict(value: dict[str, Any], *, artifact_type: str) -> dict[str, Any] | None:
    current_type = str(value.get("artifact_type") or "").strip()
    if current_type:
        return dict(value) if current_type == artifact_type else None
    if artifact_type == "planner_order" and any(
        key in value for key in ("plan_graph", "work_items", "no_work_rationale", "no_work_reason", "no_executor_work_rationale")
    ):
        artifact = dict(value)
        artifact["artifact_type"] = "planner_order"
        return artifact
    if artifact_type == "planner_decision" and any(key in value for key in ("next_action", "task_done", "final_status")):
        artifact = dict(value)
        artifact["artifact_type"] = "planner_decision"
        return artifact
    if artifact_type == "final_report" and any(key in value for key in ("completed", "blocked_by", "failed_by", "checks")):
        artifact = dict(value)
        artifact["artifact_type"] = "final_report"
        return artifact
    if artifact_type == "project_plan_draft" and any(key in value for key in ("draft_id", "proposed_scope", "success_criteria")):
        artifact = dict(value)
        artifact["artifact_type"] = "project_plan_draft"
        return artifact
    if artifact_type == "planner_chat_turn" and any(
        key in value for key in ("assistant_message", "interaction_mode", "decision", "task_state")
    ):
        artifact = dict(value)
        artifact["artifact_type"] = "planner_chat_turn"
        return artifact
    return None


def _json_objects_from_text(text: str) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for match in re.finditer(r"```(?:json)?\s*(\{.*?\})\s*```", text, flags=re.IGNORECASE | re.DOTALL):
        parsed = _parse_json_object(match.group(1))
        if parsed is not None:
            records.append(parsed)

    decoder = json.JSONDecoder()
    for index, char in enumerate(text):
        if char != "{":
            continue
        try:
            parsed, _end = decoder.raw_decode(text[index:])
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict):
            records.append(parsed)
    return records


def _parse_json_object(text: str) -> dict[str, Any] | None:
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        return None
    return parsed if isinstance(parsed, dict) else None


def _planner_order_artifact_from_structured(
    structured_artifact: dict[str, Any] | None,
    *,
    request: HarnessRunRequest,
    summary: str,
) -> tuple[dict[str, Any] | None, str | None]:
    if structured_artifact is None:
        return None, "no structured planner_order artifact was found"

    work_items, work_item_error = _planner_order_work_items(structured_artifact)
    no_work_rationale = _no_work_rationale(structured_artifact)
    if work_items is None:
        return None, work_item_error or "planner_order work_items are invalid"
    if not work_items and not no_work_rationale:
        return None, "planner_order has no work_items and no explicit no-work rationale"

    round_goal = _string_value(structured_artifact.get("round_goal")) or no_work_rationale or summary
    artifact = {
        "artifact_type": "planner_order",
        "round": _positive_int(structured_artifact.get("round"), request.context.round or 1),
        "round_goal": round_goal or "OpenHands workflow supervisor produced a planner_order.",
        "plan_graph": {"work_items": work_items},
        "instructions_for_executor": _string_list_value(structured_artifact.get("instructions_for_executor")),
        "allowed_actions": _string_list_value(structured_artifact.get("allowed_actions")),
        "forbidden_actions": _string_list_value(structured_artifact.get("forbidden_actions")),
        "target_files_or_outputs": _string_list_value(structured_artifact.get("target_files_or_outputs")),
        "expected_outputs": _string_list_value(structured_artifact.get("expected_outputs")),
        "risk_level": _risk_level(structured_artifact.get("risk_level")),
        "requires_human_confirmation": bool(structured_artifact.get("requires_human_confirmation") or False),
        "stop_and_return_to_planner_when": _string_list_value(
            structured_artifact.get("stop_and_return_to_planner_when")
        ),
    }
    if no_work_rationale and not work_items:
        artifact["no_work_rationale"] = no_work_rationale
        artifact["instructions_for_executor"] = _dedupe(
            [*artifact["instructions_for_executor"], no_work_rationale]
        )
        artifact["expected_outputs"] = _dedupe(
            [*artifact["expected_outputs"], "No executor output is expected because no executor work is needed."]
        )
    return artifact, None


def _planner_chat_turn_artifact_from_structured(
    structured_artifact: dict[str, Any] | None,
    *,
    request: HarnessRunRequest,
) -> tuple[dict[str, Any] | None, str | None]:
    if structured_artifact is None:
        return None, "no structured planner_chat_turn artifact was found"
    expected_mode = _planner_interaction_mode(request)
    actual_mode = _string_value(structured_artifact.get("interaction_mode"))
    if actual_mode != expected_mode:
        return None, "planner_chat_turn.interaction_mode must preserve the requested interaction mode"
    try:
        artifact = validate_artifact(dict(structured_artifact), expected_type="planner_chat_turn")
    except ArtifactValidationError as exc:
        return None, str(exc.errors)
    return artifact, None


def _planner_interaction_mode(request: HarnessRunRequest) -> str:
    for value in (
        request.input_artifacts.get("interaction_mode"),
        _dig(request.context.context_packet or {}, "hot", "planner_interaction_mode"),
    ):
        text = _string_value(value)
        if text in {"discuss", "work"}:
            return text
    return "discuss"


def _planner_order_work_items(structured_artifact: dict[str, Any]) -> tuple[list[dict[str, Any]] | None, str | None]:
    plan_graph = structured_artifact.get("plan_graph")
    if isinstance(plan_graph, dict):
        raw_items = plan_graph.get("work_items")
    else:
        raw_items = structured_artifact.get("work_items")
    if raw_items is None:
        return [], None
    if not isinstance(raw_items, list):
        return None, "planner_order work_items must be a list"

    items: list[dict[str, Any]] = []
    for index, item in enumerate(raw_items, start=1):
        if not isinstance(item, dict):
            return None, f"work_item {index} is not an object"
        work_item_id = _string_value(item.get("work_item_id"))
        assignee_agent_id = _string_value(item.get("assignee_agent_id"))
        task_summary = _string_value(item.get("task_summary"))
        if not (work_item_id and assignee_agent_id and task_summary):
            return None, f"work_item {index} missing work_item_id, assignee_agent_id, or task_summary"
        merge_index = _positive_int(item.get("merge_index", item.get("order_index")), index)
        depends_on = _string_list_value(item.get("depends_on"))
        items.append(
            {
                "work_item_id": work_item_id,
                "merge_index": merge_index,
                "assignee_agent_id": assignee_agent_id,
                "task_summary": task_summary,
                "depends_on": depends_on,
            }
        )
    return items, None


def _no_work_rationale(value: dict[str, Any]) -> str:
    for key in ("no_work_rationale", "no_work_reason", "no_executor_work_rationale"):
        text = _string_value(value.get(key))
        if text:
            return text
    return ""


def _execution_result_artifact_from_structured(
    structured_artifact: dict[str, Any] | None,
    *,
    request: HarnessRunRequest,
    summary: str,
    evidence_refs: list[str],
    facts: dict[str, Any],
) -> tuple[dict[str, Any] | None, str | None]:
    if structured_artifact is None:
        return None, "no structured execution_result artifact was found"

    status = str(structured_artifact.get("status") or "").strip().lower()
    if status not in {"completed", "blocked"}:
        return None, "execution_result.status must be completed or blocked"

    checks_run = _check_records(facts.get("checks_run"))
    failed_checks = [check for check in checks_run if check.get("status") in {"fail", "blocked"}]
    if status == "blocked":
        return _blocked_execution_result_artifact_from_structured(
            structured_artifact,
            request=request,
            summary=summary,
            evidence_refs=evidence_refs,
            facts=facts,
            checks_run=checks_run,
        )
    if failed_checks:
        return _runtime_failed_check_artifact(
            request=request,
            summary=summary,
            evidence_refs=evidence_refs,
            facts=facts,
            checks_run=checks_run,
            failed_checks=failed_checks,
        ), None

    no_op_rationale = _string_value(structured_artifact.get("no_op_rationale"))
    if not _has_runtime_action_facts(facts) and not no_op_rationale:
        return None, (
            "completed execution_result has no runtime file changes, commands, checks, "
            "or explicit no_op_rationale"
        )

    verification_status = "pass" if _has_passing_check(checks_run) else "skipped"
    no_check_rationale = None
    if verification_status == "skipped":
        no_check_rationale = _execution_no_check_rationale(structured_artifact, checks_run=checks_run)

    runtime_evidence_refs = _runtime_evidence_refs(evidence_refs=evidence_refs, facts=facts)
    artifact = {
        "artifact_type": "execution_result",
        "round": _positive_int(structured_artifact.get("round"), request.context.round or 1),
        "work_item_id": _execution_work_item_id(structured_artifact, request),
        "merge_index": _execution_merge_index(structured_artifact, request),
        "agent_id": request.context.agent_id,
        "status": "completed",
        "summary": _string_value(structured_artifact.get("summary")) or summary or "OpenHands task execution completed.",
        "changed_files": list(facts["changed_files"]),
        "created_files": list(facts["created_files"]),
        "deleted_files": list(facts["deleted_files"]),
        "patch_refs": list(facts["diff_refs"]),
        "attempted_actions": list(facts["commands_run"]),
        "evidence_refs": runtime_evidence_refs,
        "no_op_rationale": no_op_rationale or None,
        "verification": {
            "status": verification_status,
            "checks_run": checks_run,
            "evidence_refs": runtime_evidence_refs,
            "confidence": _confidence(structured_artifact.get("verification")),
            "remaining_work": [],
            "no_check_rationale": no_check_rationale,
            "repair_attempted": False,
            "repair_summary": None,
        },
    }
    return artifact, None


def _blocked_execution_result_artifact_from_structured(
    structured_artifact: dict[str, Any],
    *,
    request: HarnessRunRequest,
    summary: str,
    evidence_refs: list[str],
    facts: dict[str, Any],
    checks_run: list[dict[str, Any]],
) -> tuple[dict[str, Any] | None, str | None]:
    blocker_type = _blocker_type_value(structured_artifact.get("blocker_type"))
    blocker_reason = _string_value(structured_artifact.get("blocker_reason")) or _string_value(
        structured_artifact.get("summary")
    )
    remaining_work = _string_list_or_single(structured_artifact.get("remaining_work"))
    executor_recovery_exhausted = structured_artifact.get("executor_recovery_exhausted") is True
    planner_recommendation = _planner_recommendation(structured_artifact.get("planner_recommendation"))

    if not blocker_type:
        return None, "blocked execution_result requires blocker_type"
    if not blocker_reason:
        return None, "blocked execution_result requires blocker_reason"
    if not remaining_work:
        return None, "blocked execution_result requires remaining_work"
    if not executor_recovery_exhausted:
        return None, "blocked execution_result requires executor_recovery_exhausted=true"
    if not planner_recommendation:
        return None, "blocked execution_result requires planner_recommendation"

    runtime_evidence_refs = _runtime_evidence_refs(evidence_refs=evidence_refs, facts=facts)
    artifact = {
        "artifact_type": "execution_result",
        "round": _positive_int(structured_artifact.get("round"), request.context.round or 1),
        "work_item_id": _execution_work_item_id(structured_artifact, request),
        "merge_index": _execution_merge_index(structured_artifact, request),
        "agent_id": request.context.agent_id,
        "status": "blocked",
        "summary": _string_value(structured_artifact.get("summary")) or summary or blocker_reason,
        "changed_files": list(facts["changed_files"]),
        "created_files": list(facts["created_files"]),
        "deleted_files": list(facts["deleted_files"]),
        "patch_refs": list(facts["diff_refs"]),
        "attempted_actions": list(facts["commands_run"]),
        "evidence_refs": runtime_evidence_refs,
        "remaining_work": remaining_work,
        "unexpected_issues": _dedupe([blocker_reason, *_string_list_or_single(structured_artifact.get("unexpected_issues"))]),
        "needs_planner_decision": True,
        "blocker_type": blocker_type,
        "executor_recovery_exhausted": True,
        "blocker_reason": blocker_reason,
        "planner_recommendation": planner_recommendation,
        "affected_files": _dedupe(
            [
                *facts["changed_files"],
                *facts["created_files"],
                *facts["deleted_files"],
                *_string_list_or_single(structured_artifact.get("affected_files")),
            ]
        ),
        "verification": {
            "status": "blocked",
            "checks_run": checks_run,
            "evidence_refs": runtime_evidence_refs,
            "confidence": _confidence(structured_artifact.get("verification")),
            "remaining_work": remaining_work,
            "no_check_rationale": None,
            "repair_attempted": False,
            "repair_summary": None,
        },
    }
    return artifact, None


def _runtime_failed_check_artifact(
    *,
    request: HarnessRunRequest,
    summary: str,
    evidence_refs: list[str],
    facts: dict[str, Any],
    checks_run: list[dict[str, Any]],
    failed_checks: list[dict[str, Any]],
) -> dict[str, Any]:
    reason = str(failed_checks[0].get("summary") or "Runtime check evidence did not pass.")
    runtime_evidence_refs = _runtime_evidence_refs(evidence_refs=evidence_refs, facts=facts)
    return {
        "artifact_type": "execution_result",
        "round": request.context.round or 1,
        "work_item_id": str(request.input_artifacts.get("work_item_id") or "") or None,
        "agent_id": request.context.agent_id,
        "status": "blocked",
        "summary": summary or reason,
        "changed_files": list(facts["changed_files"]),
        "created_files": list(facts["created_files"]),
        "deleted_files": list(facts["deleted_files"]),
        "patch_refs": list(facts["diff_refs"]),
        "attempted_actions": list(facts["commands_run"]),
        "evidence_refs": runtime_evidence_refs,
        "remaining_work": ["Return to the planner because runtime verification did not pass."],
        "unexpected_issues": [reason],
        "needs_planner_decision": True,
        "blocker_type": "test_failed",
        "executor_recovery_exhausted": True,
        "blocker_reason": reason,
        "planner_recommendation": "replan_once",
        "verification": {
            "status": "blocked",
            "checks_run": checks_run,
            "evidence_refs": runtime_evidence_refs,
            "confidence": "medium",
            "remaining_work": ["Return to the planner because runtime verification did not pass."],
            "no_check_rationale": None,
            "repair_attempted": False,
            "repair_summary": None,
        },
    }


def _planner_decision_artifact_from_structured(
    structured_artifact: dict[str, Any],
    *,
    request: HarnessRunRequest,
    summary: str,
) -> dict[str, Any]:
    artifact = dict(structured_artifact)
    artifact["artifact_type"] = "planner_decision"
    artifact.setdefault("round", request.context.round or 1)
    artifact.setdefault("task_done", artifact.get("next_action") == "finish")
    artifact.setdefault("next_action", "finish")
    artifact.setdefault("risk_level", "low")
    artifact.setdefault("requires_human_confirmation", False)
    artifact.setdefault("reason", summary or "OpenHands workflow supervisor completed.")
    artifact.setdefault("next_round_goal", "")
    artifact.setdefault("remaining_auto_rounds", 0)
    artifact.setdefault("human_message", None)
    return artifact


def _workflow_activity_update_artifact_from_structured(
    structured_artifact: dict[str, Any] | None,
) -> tuple[dict[str, Any] | None, str | None]:
    if structured_artifact is None:
        return None, "no structured workflow_activity_update artifact was found"
    try:
        artifact = validate_artifact(dict(structured_artifact), expected_type="workflow_activity_update")
    except ArtifactValidationError as exc:
        return None, str(exc.errors)
    return artifact, None


def _final_report_artifact_from_structured(
    structured_artifact: dict[str, Any],
    *,
    request: HarnessRunRequest,
    summary: str,
    evidence_refs: list[str],
) -> dict[str, Any]:
    artifact = dict(structured_artifact)
    artifact["artifact_type"] = "final_report"
    artifact.setdefault("status", "completed")
    artifact.setdefault("summary", summary or "OpenHands workflow supervisor completed.")
    artifact.setdefault("checks", [])
    artifact.setdefault("completed", [artifact["summary"]] if artifact.get("status") == "completed" else [])
    artifact.setdefault("blocked_by", [])
    artifact.setdefault("failed_by", [])
    artifact.setdefault("warnings", [])
    artifact.setdefault("notes", [])
    artifact.setdefault("next_steps", [])
    artifact.setdefault("evidence_refs", evidence_refs)
    return _normalize_final_report_artifact(artifact, request=request)


def _normalize_final_report_artifact(artifact: dict[str, Any], *, request: HarnessRunRequest) -> dict[str, Any]:
    context_packet = request.context.context_packet or {}
    verification_summaries = _dig(context_packet, "warm", "verification_summaries")
    blocked_reasons = _string_list_or_single(_dig(context_packet, "warm", "blocked_reasons"))

    if isinstance(verification_summaries, list) and verification_summaries:
        has_pass_evidence = any(
            isinstance(item, dict)
            and item.get("status") == "pass"
            and bool(_string_list_or_single(item.get("evidence_refs")))
            for item in verification_summaries
        )
        if not has_pass_evidence:
            checks = []
            downgraded = False
            for check in artifact.get("checks") or []:
                if not isinstance(check, dict):
                    continue
                normalized = dict(check)
                if str(normalized.get("status") or "").lower() in {"pass", "passed"}:
                    normalized["status"] = "skipped"
                    normalized["summary"] = (
                        "Verification was skipped or unchecked; no passing execution evidence was available."
                    )
                    downgraded = True
                checks.append(normalized)
            artifact["checks"] = checks
            if downgraded:
                warnings = _string_list_or_single(artifact.get("warnings"))
                warning = "Skipped verification was not reported as passed."
                if warning not in warnings:
                    warnings.append(warning)
                artifact["warnings"] = warnings

    if blocked_reasons:
        artifact["status"] = "blocked"
        blocked_by = _string_list_or_single(artifact.get("blocked_by"))
        for reason in blocked_reasons:
            if reason not in blocked_by:
                blocked_by.append(reason)
        artifact["blocked_by"] = blocked_by
        if not artifact.get("summary"):
            artifact["summary"] = blocked_by[0]
        next_steps = _string_list_or_single(artifact.get("next_steps"))
        next_step = "Resolve the executor blocker before treating the workflow as complete."
        if next_step not in next_steps:
            next_steps.append(next_step)
        artifact["next_steps"] = next_steps
    return artifact


def _project_plan_draft_from_structured(
    structured_artifact: dict[str, Any],
    *,
    request: HarnessRunRequest,
    summary: str,
) -> dict[str, Any]:
    artifact = dict(structured_artifact)
    artifact["artifact_type"] = "project_plan_draft"
    artifact.setdefault("draft_id", request.request_id)
    artifact.setdefault("summary", summary or "OpenHands produced a planning draft.")
    artifact.setdefault("proposed_scope", [])
    artifact.setdefault("success_criteria", ["Confirm the draft before execution."])
    artifact.setdefault("risks", [])
    artifact.setdefault("requires_confirmation", True)
    return artifact


def _string_value(value: Any) -> str:
    return str(value or "").strip()


def _string_list_value(value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    return [str(item).strip() for item in value if str(item).strip()]


def _string_list_or_single(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        text = value.strip()
        return [text] if text else []
    if isinstance(value, (list, tuple, set)):
        return [str(item).strip() for item in value if str(item).strip()]
    text = str(value).strip()
    return [text] if text else []


def _positive_int(value: Any, default: int) -> int:
    try:
        return max(1, int(value))
    except (TypeError, ValueError):
        return max(1, int(default))


def _risk_level(value: Any) -> str:
    text = str(value or "").strip().lower()
    return text if text in {"low", "medium", "high"} else "low"


def _confidence(value: Any) -> str:
    if isinstance(value, dict):
        value = value.get("confidence")
    text = str(value or "").strip().lower()
    return text if text in {"low", "medium", "high"} else "medium"


def _blocker_type_value(value: Any) -> str | None:
    text = str(value or "").strip().lower()
    if not text:
        return None
    text = _LEGACY_BLOCKER_TYPES.get(text, text)
    return text if text in _BLOCKER_TYPES else None


def _planner_recommendation(value: Any) -> str | None:
    text = str(value or "").strip().lower()
    return text if text in {"replan_once", "finish"} else None


def _execution_work_item_id(structured_artifact: dict[str, Any], request: HarnessRunRequest) -> str | None:
    for value in (
        structured_artifact.get("work_item_id"),
        request.input_artifacts.get("work_item_id"),
        _dig(request.context.context_packet or {}, "hot", "work_item", "work_item_id"),
        _dig(request.context.context_packet or {}, "hot", "task_envelope", "work_item_id"),
    ):
        text = _string_value(value)
        if text:
            return text
    return None


def _execution_merge_index(structured_artifact: dict[str, Any], request: HarnessRunRequest) -> int | None:
    for value in (
        structured_artifact.get("merge_index", structured_artifact.get("order_index")),
        request.input_artifacts.get("merge_index"),
        _dig(request.context.context_packet or {}, "hot", "work_item", "merge_index"),
        _dig(request.context.context_packet or {}, "hot", "task_envelope", "merge_index"),
    ):
        if value is None:
            continue
        return _positive_int(value, 1)
    return None


def _execution_no_check_rationale(structured_artifact: dict[str, Any], *, checks_run: list[dict[str, Any]]) -> str:
    verification = structured_artifact.get("verification")
    if isinstance(verification, dict):
        rationale = _string_value(verification.get("no_check_rationale"))
        if rationale:
            return rationale
    rationale = _string_value(structured_artifact.get("no_check_rationale"))
    if rationale:
        return rationale
    if checks_run:
        return "OpenHands did not provide explicit passing check evidence."
    return "OpenHands did not run or report any verification checks."


def _has_runtime_action_facts(facts: dict[str, Any]) -> bool:
    return any(
        facts.get(key)
        for key in (
            "changed_files",
            "created_files",
            "deleted_files",
            "commands_run",
            "checks_run",
        )
    )


def _runtime_evidence_refs(*, evidence_refs: list[str], facts: dict[str, Any]) -> list[str]:
    return _dedupe(
        [
            *evidence_refs,
            *facts.get("evidence_refs", []),
            *facts.get("diff_refs", []),
            *facts.get("log_refs", []),
            *facts.get("native_event_refs", []),
        ]
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


__all__ = ["OpenHandsRuntimeProvider", "artifact_output_contract_available"]
