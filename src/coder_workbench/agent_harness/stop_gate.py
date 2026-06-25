from __future__ import annotations

from typing import Any

from pydantic import BaseModel, ConfigDict

from coder_workbench.agent_graph.schema import AgentTaskEnvelope, WorkItem
from coder_workbench.agent_harness.action_protocol import HarnessObservation
from coder_workbench.agent_harness.session import CodeWorkerLoopState


class StopGateDecision(BaseModel):
    model_config = ConfigDict(extra="forbid")

    accepted: bool
    recoverable: bool = False
    reason: str = ""
    error_code: str | None = None
    observation: HarnessObservation | None = None
    artifact: dict[str, Any] | None = None


class StopGate:
    def evaluate(
        self,
        payload: dict[str, Any],
        *,
        state: CodeWorkerLoopState,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
    ) -> StopGateDecision:
        identity_issue = _identity_issue(payload, item, envelope)
        if identity_issue:
            return _blocked(identity_issue, "schema_validation_failed")
        forbidden = _forbidden_field(payload)
        if forbidden:
            return _blocked(f"Executor attempted to include forbidden field: {forbidden}", "permission_boundary")
        if payload.get("artifact_type") != "execution_result":
            return _recoverable("Candidate final artifact is not an execution_result.", "stop_gate_failed", state)

        status = str(payload.get("status") or "")
        if status == "completed":
            latest_command_status = _latest_command_status(state)
            if latest_command_status in {"fail", "blocked"}:
                return _recoverable(
                    "Completed execution_result cannot ignore the latest failed or blocked command.",
                    "command_failed" if latest_command_status == "fail" else "stop_gate_failed",
                    state,
                )
            unsupported_claim = _unsupported_session_claim(payload, state)
            if unsupported_claim:
                return _recoverable(unsupported_claim, "stop_gate_failed", state)
            if not _has_completion_signal(payload, state):
                return _recoverable(
                    "Completed execution_result requires runtime-backed evidence or an explicit no-op rationale.",
                    "stop_gate_failed",
                    state,
                )
        elif status == "blocked":
            if not payload.get("blocker_type"):
                return _recoverable("Blocked execution_result requires blocker_type.", "stop_gate_failed", state)
        elif status:
            return _recoverable(f"Unsupported execution_result status: {status}", "stop_gate_failed", state)
        else:
            return _recoverable("execution_result is missing status.", "stop_gate_failed", state)
        return StopGateDecision(accepted=True, reason="Stop gate accepted candidate execution_result.")


def _identity_issue(payload: dict[str, Any], item: WorkItem, envelope: AgentTaskEnvelope) -> str:
    if payload.get("work_item_id") not in {None, "", item.work_item_id}:
        return "execution_result work_item_id does not match assigned WorkItem"
    if payload.get("merge_index") not in {None, item.merge_index}:
        return "execution_result merge_index does not match assigned WorkItem"
    if payload.get("agent_id") not in {None, "", item.assignee_agent_id, envelope.assigned_agent_id}:
        return "execution_result agent_id does not match assigned agent"
    return ""


def _forbidden_field(payload: dict[str, Any]) -> str:
    for key in ("planner_decision", "final_report", "ask_human", "human_message"):
        if key in payload:
            return key
    artifact_type = str(payload.get("artifact_type") or "")
    if artifact_type in {"planner_decision", "final_report"}:
        return artifact_type
    return ""


def _latest_command_status(state: CodeWorkerLoopState) -> str:
    checks = state.session.command_checks
    if not checks:
        return ""
    return str(checks[-1].get("status") or "")


def _unsupported_session_claim(payload: dict[str, Any], state: CodeWorkerLoopState) -> str:
    for key, supported in (
        ("changed_files", set(state.session.changed_files)),
        ("created_files", set(state.session.created_files)),
        ("deleted_files", set(state.session.deleted_files)),
        ("patch_refs", set(state.session.patch_refs)),
    ):
        claimed = {str(item) for item in payload.get(key) or [] if str(item).strip()}
        unsupported = sorted(claimed - supported)
        if unsupported:
            return f"Model claimed unsupported {key}: {', '.join(unsupported[:5])}"
    return ""


def _has_completion_signal(payload: dict[str, Any], state: CodeWorkerLoopState) -> bool:
    verification = payload.get("verification") if isinstance(payload.get("verification"), dict) else {}
    return any(
        [
            state.session.changed_files,
            state.session.created_files,
            state.session.deleted_files,
            state.session.patch_refs,
            state.session.evidence_refs,
            state.session.command_checks,
            payload.get("no_op_rationale"),
            payload.get("outputs"),
            payload.get("evidence_refs"),
            verification.get("checks_run"),
            verification.get("evidence_refs"),
            verification.get("no_check_rationale") if verification.get("status") == "skipped" else None,
        ]
    )


def _recoverable(reason: str, error_code: str, state: CodeWorkerLoopState) -> StopGateDecision:
    observation = HarnessObservation(
        action_id=f"stop-gate-{state.turn_count}",
        action_type="stop_gate",
        status="blocked",
        summary=reason,
        evidence_refs=[f"harness_observation:stop-gate-{state.turn_count}"],
        error_code=error_code,
    )
    return StopGateDecision(
        accepted=False,
        recoverable=True,
        reason=reason,
        error_code=error_code,
        observation=observation,
    )


def _blocked(reason: str, error_code: str) -> StopGateDecision:
    return StopGateDecision(
        accepted=False,
        recoverable=False,
        reason=reason,
        error_code=error_code,
    )
