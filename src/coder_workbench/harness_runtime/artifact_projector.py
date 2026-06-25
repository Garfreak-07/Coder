from __future__ import annotations

from typing import Any

from coder_workbench.core.artifacts import ArtifactValidationError, validate_artifact

from .runtime_context import HarnessRunResult


class ArtifactProjectionError(ValueError):
    pass


class ArtifactProjector:
    """Project provider facts into validated Coder boundary artifacts."""

    def project(
        self,
        result: HarnessRunResult,
        *,
        artifact_type: str | None = None,
        artifact_id: str | None = None,
    ) -> dict[str, Any]:
        target_type = artifact_type or result.artifact_type
        if not target_type:
            raise ArtifactProjectionError("artifact_type is required for projection")

        payload = dict(result.artifact or self._synthesize_artifact(target_type, result))
        payload["artifact_type"] = target_type
        payload = self._merge_runtime_refs(target_type, payload, result)
        payload = self._apply_runtime_status(target_type, payload, result)

        try:
            return validate_artifact(payload, expected_type=target_type, artifact_id=artifact_id)
        except ArtifactValidationError as exc:
            raise ArtifactProjectionError(f"{target_type} projection failed validation: {exc.errors}") from exc

    def _synthesize_artifact(self, artifact_type: str, result: HarnessRunResult) -> dict[str, Any]:
        if artifact_type == "planner_order":
            return {
                "artifact_type": "planner_order",
                "round": 1,
                "round_goal": _summary(result, "Planner should inspect runtime facts and produce work items."),
                "plan_graph": {"work_items": []},
                "instructions_for_executor": [],
                "allowed_actions": [],
                "forbidden_actions": [],
                "expected_outputs": [],
                "risk_level": "low",
                "requires_human_confirmation": False,
            }
        if artifact_type == "planner_decision":
            finished = result.status in {"completed", "failed", "blocked", "cancelled"}
            final_status = result.status if result.status in {"completed", "failed", "blocked", "cancelled"} else None
            return {
                "artifact_type": "planner_decision",
                "round": 1,
                "task_done": result.status == "completed",
                "next_action": "finish" if finished else "continue",
                "final_status": final_status,
                "risk_level": "low" if result.status == "completed" else "medium",
                "requires_human_confirmation": False,
                "reason": _summary(result, "Runtime provider returned no planner decision artifact."),
                "next_round_goal": "" if finished else "Continue with the next runtime step.",
                "remaining_auto_rounds": 0 if finished else 1,
                "human_message": None,
            }
        if artifact_type == "execution_result":
            evidence_refs = _combined_refs(result)
            blocked = result.status in {"blocked", "failed", "cancelled"}
            verification_status = "pass" if result.status == "completed" else "blocked"
            return {
                "artifact_type": "execution_result",
                "round": 1,
                "status": "blocked" if blocked else "completed",
                "summary": _summary(result, "Runtime provider completed execution."),
                "changed_files": [],
                "patch_refs": list(result.diff_refs),
                "evidence_refs": evidence_refs,
                "remaining_work": ["Review runtime error and decide next step."] if blocked else [],
                "unexpected_issues": [_error_summary(result)] if blocked else [],
                "needs_planner_decision": blocked,
                "blocker_type": "unknown_error" if blocked else None,
                "executor_recovery_exhausted": True if blocked else None,
                "blocker_reason": _error_summary(result) if blocked else None,
                "planner_recommendation": "finish" if blocked else None,
                "verification": {
                    "status": verification_status,
                    "checks_run": [],
                    "evidence_refs": evidence_refs,
                    "confidence": "medium",
                    "remaining_work": ["Review runtime error and decide next step."] if blocked else [],
                    "no_check_rationale": "Provider supplied runtime evidence but no explicit check command."
                    if result.status == "completed"
                    else None,
                },
            }
        if artifact_type == "final_report":
            return {
                "artifact_type": "final_report",
                "status": result.status,
                "summary": _summary(result, "Runtime provider finished."),
                "checks": [],
                "completed": [_summary(result, "Runtime provider completed execution.")]
                if result.status == "completed"
                else [],
                "blocked_by": [_error_summary(result)] if result.status == "blocked" else [],
                "failed_by": [_error_summary(result)] if result.status == "failed" else [],
                "warnings": [],
                "notes": [],
                "next_steps": [] if result.status == "completed" else ["Inspect runtime evidence refs."],
                "evidence_refs": _combined_refs(result),
            }
        raise ArtifactProjectionError(f"unsupported projection artifact_type {artifact_type!r}")

    def _merge_runtime_refs(
        self,
        artifact_type: str,
        payload: dict[str, Any],
        result: HarnessRunResult,
    ) -> dict[str, Any]:
        refs = _combined_refs(result)
        if refs:
            payload["evidence_refs"] = _dedupe([*payload.get("evidence_refs", []), *refs])
        if artifact_type == "execution_result" and result.diff_refs:
            payload["patch_refs"] = _dedupe([*payload.get("patch_refs", []), *result.diff_refs])
            verification = dict(payload.get("verification") or {})
            verification["evidence_refs"] = _dedupe([*verification.get("evidence_refs", []), *refs])
            payload["verification"] = verification
        if artifact_type == "final_report" and result.log_refs:
            checks = list(payload.get("checks") or [])
            if not checks:
                checks.append(
                    {
                        "status": "unknown",
                        "summary": "Runtime logs are available as evidence refs.",
                        "evidence_refs": list(result.log_refs),
                    }
                )
                payload["checks"] = checks
        return payload

    def _apply_runtime_status(
        self,
        artifact_type: str,
        payload: dict[str, Any],
        result: HarnessRunResult,
    ) -> dict[str, Any]:
        if artifact_type == "execution_result" and result.status in {"blocked", "failed", "cancelled"}:
            payload["status"] = "blocked"
            payload.setdefault("unexpected_issues", [])
            if _error_summary(result) not in payload["unexpected_issues"]:
                payload["unexpected_issues"].append(_error_summary(result))
            payload.setdefault("remaining_work", ["Inspect runtime evidence refs."])
            payload["needs_planner_decision"] = True
            payload.setdefault("blocker_type", "unknown_error")
            payload.setdefault("executor_recovery_exhausted", True)
            payload.setdefault("blocker_reason", _error_summary(result))
            payload.setdefault("planner_recommendation", "finish")
            verification = dict(payload.get("verification") or {})
            verification["status"] = "blocked"
            verification.setdefault("confidence", "medium")
            verification.setdefault("checks_run", [])
            verification.setdefault("evidence_refs", _combined_refs(result))
            verification.setdefault("remaining_work", ["Inspect runtime evidence refs."])
            payload["verification"] = verification
        if artifact_type == "final_report" and result.status in {"blocked", "failed", "cancelled"}:
            payload["status"] = result.status
        return payload


def _combined_refs(result: HarnessRunResult) -> list[str]:
    return _dedupe([*result.evidence_refs, *result.native_event_refs, *result.diff_refs, *result.log_refs])


def _dedupe(values: list[str]) -> list[str]:
    seen: set[str] = set()
    output: list[str] = []
    for value in values:
        if value in seen:
            continue
        seen.add(value)
        output.append(value)
    return output


def _summary(result: HarnessRunResult, default: str) -> str:
    if result.artifact and isinstance(result.artifact.get("summary"), str) and result.artifact["summary"].strip():
        return str(result.artifact["summary"])
    if result.error and result.error.get("message"):
        return str(result.error["message"])
    return default


def _error_summary(result: HarnessRunResult) -> str:
    if result.error:
        return str(result.error.get("message") or result.error.get("code") or "Runtime provider failed.")
    return f"Runtime provider returned status {result.status}."


__all__ = ["ArtifactProjectionError", "ArtifactProjector"]
