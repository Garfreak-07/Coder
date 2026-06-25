from __future__ import annotations

from typing import Any

from coder_workbench.agent_graph.cache import GraphRunCache
from coder_workbench.agent_graph.schema import (
    PlanRunSummary,
    PlanStatus,
    PlannerInputBundle,
    PlannerInputBundleItem,
    RoundSummaryItem,
)


def build_planner_input_bundle(cache: GraphRunCache) -> PlannerInputBundle:
    # GraphRunCache.execution_cache entries are normalized before bundle and summary construction.
    planner_order_ref = cache.plan_cache.planner_order_ref if cache.plan_cache else "planner_order_unknown"
    items = [
        PlannerInputBundleItem(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            task_summary=item.task_summary,
            execution_status=cache.execution_cache[item.work_item_id].status
            if item.work_item_id in cache.execution_cache
            else "not_started",
            execution_summary=cache.execution_cache[item.work_item_id].execution_summary
            if item.work_item_id in cache.execution_cache
            else "",
            verification_status=_verification_status(cache.execution_cache.get(item.work_item_id)),
            verification_summary=_verification_summary(cache.execution_cache.get(item.work_item_id)),
            refs=cache.refs_for_work_item(item.work_item_id),
        )
        for item in cache.work_items()
    ]
    return PlannerInputBundle(
        round=cache.round,
        planner_order_ref=planner_order_ref,
        plan_status=_plan_status(items, cache),
        items=items,
        effects=cache.hidden_effects,
        interrupts=cache.interrupts,
    )


def build_round_summary(cache: GraphRunCache) -> PlanRunSummary:
    planner_order_ref = cache.plan_cache.planner_order_ref if cache.plan_cache else "planner_order_unknown"
    bundle = build_planner_input_bundle(cache)
    ordered_state = [
        RoundSummaryItem(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            status=_round_item_status(item),
            summary=_round_item_summary(item),
            refs=item.refs,
        )
        for item in bundle.items
    ]
    return PlanRunSummary(
        round=cache.round,
        planner_order_ref=planner_order_ref,
        plan_status=bundle.plan_status,
        completed_count=sum(1 for item in ordered_state if item.status == "completed"),
        blocked_count=sum(1 for item in ordered_state if item.status == "blocked"),
        ordered_state=ordered_state,
        remaining_work=_remaining_work(bundle, ordered_state),
        carry_forward_constraints=[],
    )


def _remaining_work(bundle: PlannerInputBundle, ordered_state: list[RoundSummaryItem]) -> list[str]:
    remaining = [
        item.summary
        for item in ordered_state
        if item.status == "blocked"
    ]
    if bundle.plan_status == "interrupted":
        for interrupt in bundle.interrupts:
            detail = interrupt.reason
            if interrupt.planner_question:
                detail = f"{detail} Planner question: {interrupt.planner_question}"
            remaining.append(detail)
    return remaining


def _plan_status(items: list[PlannerInputBundleItem], cache: GraphRunCache) -> PlanStatus:
    if cache.interrupts:
        return "interrupted"
    if not items:
        return "completed"
    if any(item.execution_status == "blocked" or item.verification_status in {"fail", "blocked"} for item in items):
        return "blocked"
    if all(item.execution_status == "completed" for item in items):
        return "completed"
    return "running"


def _round_item_status(item: PlannerInputBundleItem) -> str:
    if item.execution_status == "blocked" or item.verification_status in {"fail", "blocked"}:
        return "blocked"
    if item.execution_status == "completed":
        return "completed"
    return "pending"


def _round_item_summary(item: PlannerInputBundleItem) -> str:
    parts = [part for part in [item.execution_summary, item.verification_summary] if part]
    return " ".join(parts) if parts else item.task_summary


def _verification_status(record: Any | None) -> str:
    artifact = getattr(record, "artifact_payload", None) if record is not None else None
    if not isinstance(artifact, dict):
        return "not_started"
    verification = artifact.get("verification")
    if not isinstance(verification, dict):
        return "not_started"
    return str(verification.get("status") or "not_started")


def _verification_summary(record: Any | None) -> str:
    artifact = getattr(record, "artifact_payload", None) if record is not None else None
    if not isinstance(artifact, dict):
        return ""
    verification = artifact.get("verification")
    if not isinstance(verification, dict):
        return ""
    checks = verification.get("checks_run")
    if isinstance(checks, list) and checks:
        summaries = [str(check.get("summary") or "") for check in checks if isinstance(check, dict)]
        return " ".join(summary for summary in summaries if summary)
    remaining = verification.get("remaining_work")
    if isinstance(remaining, list) and remaining:
        return " ".join(str(item) for item in remaining if str(item).strip())
    return str(verification.get("no_check_rationale") or "")
