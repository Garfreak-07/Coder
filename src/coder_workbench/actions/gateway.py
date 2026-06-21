from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable

from coder_workbench.actions.schema import ACTION_TYPES, ActionResult, ActionSpec
from coder_workbench.budget import BudgetBroker, BudgetLimit
from coder_workbench.core.artifacts import ArtifactValidationError, validate_artifact
from coder_workbench.coding.command_service import CommandService
from coder_workbench.coding.patch_service import PatchService
from coder_workbench.skills import SkillIndex, estimate_tokens


PatchServiceFactory = Callable[[str | Path, list[str], dict[str, Any]], PatchService]
CommandServiceFactory = Callable[[str | Path, list[str], dict[str, Any]], CommandService]


@dataclass
class RunContext:
    run_id: str
    repo_root: str | Path
    scopes: list[str] | None = None
    data: dict[str, Any] | None = None
    cache: Any | None = None
    item: Any | None = None
    planner_order_ref: str | None = None
    upstream_refs: list[str] | None = None
    user_request: str = ""
    role: str = ""
    skill_index: SkillIndex | None = None
    skill_store_root: Path | None = None
    repo_intelligence: dict[str, Any] | None = None
    artifact_type: str = "execution_result"
    emit: Any | None = None
    model: Any | None = None

    @property
    def mutable_data(self) -> dict[str, Any]:
        if self.data is None:
            self.data = {}
        return self.data

    @property
    def active_scopes(self) -> list[str]:
        return list(self.scopes or [])


class ActionGateway:
    """Single entry point for low-level runtime actions."""

    def __init__(
        self,
        *,
        budget_broker: BudgetBroker | None = None,
        context_service: Any | None = None,
        repair_service: Any | None = None,
        patch_service_factory: PatchServiceFactory | None = None,
        command_service_factory: CommandServiceFactory | None = None,
    ) -> None:
        self.budget_broker = budget_broker or BudgetBroker(BudgetLimit())
        if context_service is None:
            from coder_workbench.context import ContextService

            context_service = ContextService()
        self.context_service = context_service
        if repair_service is None:
            from coder_workbench.agent_harness.repair import ArtifactRepairService

            repair_service = ArtifactRepairService()
        self.repair_service = repair_service
        self.patch_service_factory = patch_service_factory or (
            lambda repo_root, scopes, data: PatchService(repo_root, scopes=scopes, data=data)
        )
        self.command_service_factory = command_service_factory or (
            lambda repo_root, scopes, data: CommandService(repo_root, scopes=scopes, data=data)
        )

    def run(self, spec: ActionSpec, *, run_context: RunContext) -> ActionResult:
        if spec.action_type not in ACTION_TYPES:
            return ActionResult(
                status="failed",
                summary=f"Unknown action_type: {spec.action_type}",
                error_code="unknown_action_type",
            )
        try:
            if spec.action_type == "build_context":
                return self._build_context(spec, run_context)
            if spec.action_type == "propose_patch":
                return self._propose_patch(spec, run_context)
            if spec.action_type in {"run_command", "run_command_sandbox"}:
                return self._run_command(spec, run_context)
            if spec.action_type == "validate_artifact":
                return self._validate_artifact(spec)
            if spec.action_type == "repair_artifact":
                return self._repair_artifact(spec, run_context)
            return ActionResult(
                status="failed",
                summary=f"Action type {spec.action_type} is not implemented yet.",
                error_code="action_not_implemented",
            )
        except Exception as exc:  # pragma: no cover - defensive gateway boundary
            return ActionResult(status="failed", summary=str(exc), error_code="action_gateway_exception")

    def _build_context(self, spec: ActionSpec, run_context: RunContext) -> ActionResult:
        skill_index = _input_or_context(spec, run_context, "skill_index")
        if skill_index is None:
            skill_index = SkillIndex()
        estimated = spec.estimated_tokens or _estimate_context_tokens(spec, run_context, skill_index)
        reservation = self.budget_broker.reserve_context(
            run_id=run_context.run_id,
            agent_id=_agent_id(run_context),
            estimated_tokens=estimated,
            action_type=spec.action_type,
        )
        budget_compressed = False
        if not reservation.approved and reservation.reason == "context_budget_exceeded" and skill_index.enabled():
            skill_index = SkillIndex(skills=[])
            estimated = _estimate_context_tokens(spec, run_context, skill_index)
            reservation = self.budget_broker.reserve_context(
                run_id=run_context.run_id,
                agent_id=_agent_id(run_context),
                estimated_tokens=estimated,
                action_type=spec.action_type,
            )
            budget_compressed = True
        if not reservation.approved:
            return ActionResult(
                status="blocked",
                summary="BudgetBroker denied context construction.",
                error_code=reservation.reason,
                payload={"reservation": reservation.model_dump(mode="json")},
            )

        context = self.context_service.build_for_work_item(
            cache=_required(_input_or_context(spec, run_context, "cache"), "cache"),
            item=_required(_input_or_context(spec, run_context, "item"), "item"),
            planner_order_ref=_required(_input_or_context(spec, run_context, "planner_order_ref"), "planner_order_ref"),
            upstream_refs=list(_input_or_context(spec, run_context, "upstream_refs") or []),
            user_request=str(_input_or_context(spec, run_context, "user_request") or ""),
            role=str(_input_or_context(spec, run_context, "role") or ""),
            skill_index=skill_index,
            skill_store_root=Path(_required(_input_or_context(spec, run_context, "skill_store_root"), "skill_store_root")),
            run_id=run_context.run_id,
            repo_root=str(run_context.repo_root),
            repo_intelligence=dict(_input_or_context(spec, run_context, "repo_intelligence") or {}),
            artifact_type=str(_input_or_context(spec, run_context, "artifact_type") or "execution_result"),
        )
        self.budget_broker.commit(reservation.reservation_id, actual_tokens=context.token_ledger_entry.estimated_input_tokens)
        return ActionResult(
            status="ok",
            summary="ContextService built work-item context.",
            token_used=context.token_ledger_entry.estimated_input_tokens,
            payload={
                "reservation": reservation.model_dump(mode="json"),
                "budget_compressed": budget_compressed,
                "context": context,
                "envelope": context.envelope,
                "skill_route": context.skill_route,
                "context_packet": context.context_packet,
                "token_ledger_entry": context.token_ledger_entry,
                "coding_context_packet": context.coding_context_packet,
            },
        )

    def _propose_patch(self, spec: ActionSpec, run_context: RunContext) -> ActionResult:
        reservation = self.budget_broker.reserve_tool_call(
            run_id=run_context.run_id,
            agent_id=_agent_id(run_context),
            action_type=spec.action_type,
            estimated_tokens=spec.estimated_tokens,
        )
        if not reservation.approved:
            return _budget_blocked(reservation)
        changes = spec.input.get("changes", spec.input.get("proposed_changes", spec.input))
        preview = self.patch_service_factory(
            run_context.repo_root,
            run_context.active_scopes,
            run_context.mutable_data,
        ).preview(changes)
        self.budget_broker.commit(reservation.reservation_id, actual_tool_calls=1)
        status = "blocked" if preview.get("status") == "blocked" else "ok"
        return ActionResult(
            status=status,
            summary=str(preview.get("message") or "Patch preview generated."),
            error_code=preview.get("error_code") if status == "blocked" else None,
            payload={"reservation": reservation.model_dump(mode="json"), "preview": preview},
        )

    def _run_command(self, spec: ActionSpec, run_context: RunContext) -> ActionResult:
        reservation = self.budget_broker.reserve_tool_call(
            run_id=run_context.run_id,
            agent_id=_agent_id(run_context),
            action_type=spec.action_type,
            estimated_tokens=spec.estimated_tokens,
        )
        if not reservation.approved:
            return _budget_blocked(reservation)
        command = str(spec.input.get("command") or "")
        result = self.command_service_factory(
            run_context.repo_root,
            run_context.active_scopes,
            run_context.mutable_data,
        ).run_check(
            command,
            cwd=str(spec.input.get("cwd") or "."),
            timeout_seconds=int(spec.input.get("timeout_seconds") or 120),
            require_approval=bool(spec.input.get("require_approval", True)),
        )
        self.budget_broker.commit(reservation.reservation_id, actual_tool_calls=1)
        return ActionResult(
            status="blocked" if result.get("blocked") else "ok",
            summary=str(result.get("message") or result.get("output") or "Command completed."),
            error_code="command_requires_approval" if result.get("blocked") else None,
            payload={"reservation": reservation.model_dump(mode="json"), "result": result},
        )

    def _validate_artifact(self, spec: ActionSpec) -> ActionResult:
        artifact_type = str(spec.input.get("expected_type") or spec.input.get("artifact_type") or "")
        try:
            artifact = validate_artifact(dict(spec.input.get("artifact") or {}), expected_type=artifact_type)
        except (ArtifactValidationError, ValueError) as exc:
            return ActionResult(status="failed", summary=str(exc), error_code="artifact_validation_failed")
        return ActionResult(status="ok", summary="Artifact validated.", payload={"artifact": artifact})

    def _repair_artifact(self, spec: ActionSpec, run_context: RunContext) -> ActionResult:
        if run_context.model is None:
            return ActionResult(status="blocked", summary="Repair requires a model.", error_code="model_required")
        repaired = self.repair_service.repair_once(
            run_context.model,
            expected_type=str(spec.input.get("expected_type") or ""),
            invalid_output=str(spec.input.get("invalid_output") or ""),
            agent_id=str(spec.input.get("agent_id") or _agent_id(run_context) or "agent"),
            emit=run_context.emit,
            work_item_id=str(spec.input.get("work_item_id") or "") or None,
            merge_index=spec.input.get("merge_index"),
            schema_notes=str(spec.input.get("schema_notes") or ""),
        )
        if repaired is None:
            return ActionResult(status="failed", summary="Artifact repair failed.", error_code="artifact_repair_failed")
        return ActionResult(status="ok", summary="Artifact repaired.", payload={"artifact": repaired})


def _budget_blocked(reservation: Any) -> ActionResult:
    return ActionResult(
        status="blocked",
        summary="BudgetBroker denied the action.",
        error_code=reservation.reason,
        payload={"reservation": reservation.model_dump(mode="json")},
    )


def _agent_id(run_context: RunContext) -> str | None:
    item = run_context.item
    return str(getattr(item, "assignee_agent_id", "") or "") or None


def _estimate_context_tokens(spec: ActionSpec, run_context: RunContext, skill_index: SkillIndex) -> int:
    item = _input_or_context(spec, run_context, "item")
    task_summary = str(getattr(item, "task_summary", "") or "")
    upstream_refs = " ".join(str(ref) for ref in (_input_or_context(spec, run_context, "upstream_refs") or []))
    text = " ".join(
        [
            str(_input_or_context(spec, run_context, "user_request") or ""),
            task_summary,
            upstream_refs,
            str(_input_or_context(spec, run_context, "planner_order_ref") or ""),
        ]
    )
    return estimate_tokens(text) + sum(skill.max_skill_tokens for skill in skill_index.enabled())


def _input_or_context(spec: ActionSpec, run_context: RunContext, key: str) -> Any:
    if key in spec.input:
        return spec.input[key]
    return getattr(run_context, key)


def _required(value: Any, name: str) -> Any:
    if value is None:
        raise ValueError(f"{name} is required")
    return value
