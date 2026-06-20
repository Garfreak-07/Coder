from __future__ import annotations

from coder_workbench.agent_graph.cache import GraphRunCache
from coder_workbench.agent_graph.schema import (
    PlanRunSummary,
    PlanStatus,
    PlannerInputBundle,
    PlannerInputBundleItem,
    RoundSummaryItem,
    TestRecord,
)


def build_planner_input_bundle(cache: GraphRunCache) -> PlannerInputBundle:
    planner_order_ref = cache.plan_cache.planner_order_ref if cache.plan_cache else "memory:planner_order:unknown"
    items = [
        PlannerInputBundleItem(
            work_item_id=item.work_item_id,
            order_index=item.order_index,
            task_summary=item.task_summary,
            execution_status=cache.execution_cache[item.work_item_id].status
            if item.work_item_id in cache.execution_cache
            else "not_started",
            execution_summary=cache.execution_cache[item.work_item_id].execution_summary
            if item.work_item_id in cache.execution_cache
            else "",
            test_status=_merged_test_status(cache.test_cache.get(item.work_item_id, []), item.tester_agent_ids),
            test_summary=_merged_test_summary(cache.test_cache.get(item.work_item_id, []), item.tester_agent_ids),
            refs=cache.refs_for_work_item(item.work_item_id),
        )
        for item in cache.work_items()
    ]
    return PlannerInputBundle(
        round=cache.round,
        planner_order_ref=planner_order_ref,
        plan_status=_plan_status(items),
        items=items,
        final_test_summary=cache.final_test_cache.summary if cache.final_test_cache else None,
        final_test_ref=cache.final_test_cache.final_test_result_ref if cache.final_test_cache else None,
        effects=cache.hidden_effects,
    )


def build_round_summary(cache: GraphRunCache) -> PlanRunSummary:
    planner_order_ref = cache.plan_cache.planner_order_ref if cache.plan_cache else "memory:planner_order:unknown"
    bundle = build_planner_input_bundle(cache)
    ordered_state = [
        RoundSummaryItem(
            work_item_id=item.work_item_id,
            order_index=item.order_index,
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
        failed_count=sum(1 for item in ordered_state if item.status in {"failed_execution", "failed_test"}),
        blocked_count=sum(1 for item in ordered_state if item.status == "blocked"),
        ordered_state=ordered_state,
        remaining_work=[
            item.summary
            for item in ordered_state
            if item.status in {"failed_execution", "failed_test", "blocked"}
        ],
        carry_forward_constraints=[],
    )


def _plan_status(items: list[PlannerInputBundleItem]) -> PlanStatus:
    if not items:
        return "completed"
    if any(item.execution_status == "failed" or item.test_status == "fail" for item in items):
        return "partial_failed"
    if any(item.execution_status == "blocked" or item.test_status == "blocked" for item in items):
        return "blocked"
    if all(item.execution_status == "completed" for item in items):
        return "completed"
    return "running"


def _round_item_status(item: PlannerInputBundleItem) -> str:
    if item.execution_status == "blocked" or item.test_status == "blocked":
        return "blocked"
    if item.execution_status == "failed":
        return "failed_execution"
    if item.test_status == "fail":
        return "failed_test"
    if item.execution_status == "completed":
        return "completed"
    return "pending"


def _round_item_summary(item: PlannerInputBundleItem) -> str:
    parts = [part for part in [item.execution_summary, item.test_summary] if part]
    return " ".join(parts) if parts else item.task_summary


def _merged_test_status(records: list[TestRecord], tester_agent_ids: list[str]) -> str:
    if not tester_agent_ids:
        return "not_requested"
    if any(record.status == "fail" for record in records):
        return "fail"
    if any(record.status == "blocked" for record in records):
        return "blocked"
    return "pass" if records else "not_requested"


def _merged_test_summary(records: list[TestRecord], tester_agent_ids: list[str]) -> str:
    if not tester_agent_ids:
        return ""
    if not records:
        return "Test was requested but no TestRecord was written."
    return " ".join(record.test_summary for record in records)
