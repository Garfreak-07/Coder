from __future__ import annotations

import hashlib
import json
import time
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict

from coder_workbench.agent_graph.schema import PlannerOrder
from coder_workbench.runtime_kernel.round_state import RoundState
from coder_workbench.runtime_kernel.run_guard import RunGuard


class RunControllerDecision(BaseModel):
    model_config = ConfigDict(extra="forbid")

    action: Literal["continue", "finish", "blocked"]
    reason: str = ""
    status_code: str | None = None
    final_status: Literal["completed", "blocked", "failed", "cancelled"] | None = None


class RunController:
    """Owns run-level loop decisions outside AgentGraphRunner internals."""

    def __init__(self, *, guard: RunGuard | None = None, started_at: float | None = None) -> None:
        self.guard = guard or RunGuard()
        self.started_at = started_at or time.monotonic()
        self.rounds: list[RoundState] = []
        self.recent_plan_fingerprints: list[str] = []
        self.agent_calls = 0
        self.tool_calls = 0
        self.estimated_tokens = 0

    def record_round(
        self,
        outcome: Any | None = None,
        *,
        round_number: int | None = None,
        planner_order: PlannerOrder | None = None,
        planner_order_ref: str | None = None,
        planner_decision_ref: str | None = None,
        planner_input_bundle_ref: str | None = None,
        status: str = "decision",
        agent_calls: int = 0,
        tool_calls: int = 0,
        estimated_tokens: int = 0,
    ) -> RoundState:
        if outcome is not None:
            round_number = round_number or int(getattr(outcome, "round", 0) or 0)
            planner_order = planner_order or getattr(outcome, "planner_order", None)
        if round_number is None or round_number < 1:
            raise ValueError("round_number is required")

        plan_fingerprint = fingerprint_planner_order(planner_order) if planner_order is not None else None
        if plan_fingerprint:
            self.recent_plan_fingerprints.append(plan_fingerprint)
        self.agent_calls += max(0, agent_calls)
        self.tool_calls += max(0, tool_calls)
        self.estimated_tokens += max(0, estimated_tokens)

        state = RoundState(
            round=round_number,
            planner_order_ref=planner_order_ref,
            planner_decision_ref=planner_decision_ref,
            planner_input_bundle_ref=planner_input_bundle_ref,
            plan_fingerprint=plan_fingerprint,
            status=status,  # type: ignore[arg-type]
        )
        self.rounds.append(state)
        return state

    def evaluate_planner_decision(
        self,
        planner_decision: dict[str, Any],
        *,
        round_number: int | None = None,
    ) -> RunControllerDecision:
        action = str(planner_decision.get("next_action") or "")
        reason = str(planner_decision.get("reason") or "")
        if action == "finish":
            return RunControllerDecision(
                action="finish",
                reason=reason,
                final_status=_final_status(planner_decision.get("final_status")),
            )
        if action == "stop":
            return RunControllerDecision(action="finish", reason=reason, final_status="completed")
        if action in {"ask_human", "blocked"}:
            return RunControllerDecision(
                action="finish",
                reason=reason,
                status_code="legacy_planner_blocked_action",
                final_status="blocked",
            )
        if action != "continue":
            return RunControllerDecision(
                action="blocked",
                reason=f"Unsupported PlannerDecision next_action: {action or '<empty>'}",
                status_code="planner_decision_invalid_action",
            )

        guard_decision = self._guard_decision(round_number=round_number, reason=reason)
        if guard_decision is not None:
            return guard_decision
        return RunControllerDecision(action="continue", reason=reason)

    def evaluate_budget_preflight(self, report: Any) -> RunControllerDecision:
        approved = bool(getattr(report, "approved", False))
        if isinstance(report, dict):
            approved = bool(report.get("approved"))
            reason = str(report.get("reason") or "")
        else:
            reason = str(getattr(report, "reason", "") or "")
        if approved:
            return RunControllerDecision(action="continue")
        status_code = reason or "round_budget_preflight_denied"
        return RunControllerDecision(
            action="blocked",
            reason=f"Round budget preflight denied: {status_code}.",
            status_code=status_code,
        )

    def _guard_decision(self, *, round_number: int | None, reason: str) -> RunControllerDecision | None:
        active_round = round_number if round_number is not None else (self.rounds[-1].round if self.rounds else 0)
        if active_round >= self.guard.max_rounds:
            return RunControllerDecision(
                action="blocked",
                reason="Planner requested another round, but max_auto_rounds has been reached.",
                status_code="max_auto_rounds_reached",
            )
        if self.agent_calls > self.guard.max_agent_calls:
            return RunControllerDecision(
                action="blocked",
                reason="RunGuard blocked the run after exceeding max_agent_calls.",
                status_code="max_agent_calls_reached",
            )
        if self.tool_calls > self.guard.max_tool_calls:
            return RunControllerDecision(
                action="blocked",
                reason="RunGuard blocked the run after exceeding max_tool_calls.",
                status_code="max_tool_calls_reached",
            )
        if self.estimated_tokens > self.guard.max_total_estimated_tokens:
            return RunControllerDecision(
                action="blocked",
                reason="RunGuard blocked the run after exceeding max_total_estimated_tokens.",
                status_code="max_total_estimated_tokens_reached",
            )
        if time.monotonic() - self.started_at > self.guard.max_wall_seconds:
            return RunControllerDecision(
                action="blocked",
                reason="RunGuard blocked the run after exceeding max_wall_seconds.",
                status_code="max_wall_seconds_reached",
            )
        if self._same_plan_repeat_count() > self.guard.max_same_plan_repeats:
            return RunControllerDecision(
                action="blocked",
                reason=reason or "Planner produced the same plan too many times.",
                status_code="repeated_plan_fingerprint",
            )
        return None

    def _same_plan_repeat_count(self) -> int:
        if not self.recent_plan_fingerprints:
            return 0
        latest = self.recent_plan_fingerprints[-1]
        count = 0
        for fingerprint in reversed(self.recent_plan_fingerprints):
            if fingerprint != latest:
                break
            count += 1
        return count

    def diagnostics(self) -> dict[str, Any]:
        return {
            "guard": self.guard.model_dump(mode="json"),
            "rounds": [round_state.model_dump(mode="json") for round_state in self.rounds],
            "recent_plan_fingerprints": self.recent_plan_fingerprints[-5:],
            "agent_calls": self.agent_calls,
            "tool_calls": self.tool_calls,
            "estimated_tokens": self.estimated_tokens,
        }


def fingerprint_planner_order(order: PlannerOrder) -> str:
    work_items = [
        {
            "work_item_id": item.work_item_id,
            "merge_index": item.merge_index,
            "assignee_agent_id": item.assignee_agent_id,
            "task_summary": item.task_summary,
            "depends_on": item.depends_on,
        }
        for item in order.plan_graph.work_items
    ]
    payload = {
        "round_goal": order.round_goal,
        "work_items": work_items,
    }
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _final_status(value: Any) -> Literal["completed", "blocked", "failed", "cancelled"] | None:
    if value in {"completed", "blocked", "failed", "cancelled"}:
        return value
    return None
