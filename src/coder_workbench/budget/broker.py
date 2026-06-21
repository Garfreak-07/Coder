from __future__ import annotations

from collections import defaultdict
from dataclasses import asdict
from dataclasses import dataclass
from uuid import uuid4

from coder_workbench.budget.reservation import BudgetLimit, BudgetReservation


@dataclass
class BudgetUsage:
    estimated_tokens_reserved: int = 0
    actual_tokens_committed: int = 0
    model_calls_reserved: int = 0
    tool_calls_reserved: int = 0
    tool_calls_committed: int = 0


class BudgetBroker:
    """Pre-execution reservation broker for run resources."""

    def __init__(self, limit: BudgetLimit | None = None) -> None:
        self.limit = limit or BudgetLimit()
        self._usage: defaultdict[str, BudgetUsage] = defaultdict(BudgetUsage)
        self._reservations: dict[str, BudgetReservation] = {}

    def reserve_model_call(
        self,
        *,
        run_id: str,
        agent_id: str | None = None,
        estimated_tokens: int = 0,
        action_type: str = "model_call",
    ) -> BudgetReservation:
        return self.reserve(
            run_id=run_id,
            agent_id=agent_id,
            action_type=action_type,
            estimated_tokens=estimated_tokens,
            estimated_model_calls=1,
        )

    def reserve_context(
        self,
        *,
        run_id: str,
        agent_id: str | None = None,
        estimated_tokens: int = 0,
        action_type: str = "build_context",
    ) -> BudgetReservation:
        if estimated_tokens > self.limit.max_context_tokens_per_call:
            return self._denied(
                run_id=run_id,
                agent_id=agent_id,
                action_type=action_type,
                estimated_tokens=estimated_tokens,
                reason="context_budget_exceeded",
            )
        return self.reserve(
            run_id=run_id,
            agent_id=agent_id,
            action_type=action_type,
            estimated_tokens=estimated_tokens,
        )

    def reserve_tool_call(
        self,
        *,
        run_id: str,
        agent_id: str | None = None,
        action_type: str,
        estimated_tokens: int = 0,
        estimated_tool_calls: int = 1,
    ) -> BudgetReservation:
        return self.reserve(
            run_id=run_id,
            agent_id=agent_id,
            action_type=action_type,
            estimated_tokens=estimated_tokens,
            estimated_tool_calls=estimated_tool_calls,
        )

    def reserve(
        self,
        *,
        run_id: str,
        agent_id: str | None = None,
        action_type: str,
        estimated_tokens: int = 0,
        estimated_tool_calls: int = 0,
        estimated_model_calls: int = 0,
    ) -> BudgetReservation:
        usage = self._usage[run_id]
        if usage.estimated_tokens_reserved + estimated_tokens > self.limit.max_estimated_tokens:
            return self._denied(
                run_id=run_id,
                agent_id=agent_id,
                action_type=action_type,
                estimated_tokens=estimated_tokens,
                estimated_tool_calls=estimated_tool_calls,
                estimated_model_calls=estimated_model_calls,
                reason="estimated_token_budget_exceeded",
            )
        if usage.model_calls_reserved + estimated_model_calls > self.limit.max_model_calls:
            return self._denied(
                run_id=run_id,
                agent_id=agent_id,
                action_type=action_type,
                estimated_tokens=estimated_tokens,
                estimated_tool_calls=estimated_tool_calls,
                estimated_model_calls=estimated_model_calls,
                reason="model_call_budget_exceeded",
            )
        if usage.tool_calls_reserved + estimated_tool_calls > self.limit.max_tool_calls:
            return self._denied(
                run_id=run_id,
                agent_id=agent_id,
                action_type=action_type,
                estimated_tokens=estimated_tokens,
                estimated_tool_calls=estimated_tool_calls,
                estimated_model_calls=estimated_model_calls,
                reason="tool_call_budget_exceeded",
            )
        reservation = BudgetReservation(
            reservation_id=str(uuid4()),
            run_id=run_id,
            agent_id=agent_id,
            action_type=action_type,
            estimated_tokens=estimated_tokens,
            estimated_tool_calls=estimated_tool_calls,
            estimated_model_calls=estimated_model_calls,
            approved=True,
        )
        self._reservations[reservation.reservation_id] = reservation
        usage.estimated_tokens_reserved += estimated_tokens
        usage.tool_calls_reserved += estimated_tool_calls
        usage.model_calls_reserved += estimated_model_calls
        return reservation

    def commit(
        self,
        reservation_id: str,
        *,
        actual_tokens: int = 0,
        actual_tool_calls: int | None = None,
    ) -> BudgetReservation:
        reservation = self._reservations[reservation_id]
        if not reservation.approved or reservation.committed or reservation.released:
            return reservation
        actual_tools = reservation.estimated_tool_calls if actual_tool_calls is None else max(0, actual_tool_calls)
        updated = reservation.model_copy(
            update={
                "committed": True,
                "actual_tokens": max(0, actual_tokens),
                "actual_tool_calls": actual_tools,
            }
        )
        self._reservations[reservation_id] = updated
        usage = self._usage[updated.run_id]
        usage.actual_tokens_committed += updated.actual_tokens
        usage.tool_calls_committed += updated.actual_tool_calls
        return updated

    def release(self, reservation_id: str) -> BudgetReservation:
        reservation = self._reservations[reservation_id]
        if not reservation.approved or reservation.committed or reservation.released:
            return reservation
        updated = reservation.model_copy(update={"released": True})
        self._reservations[reservation_id] = updated
        usage = self._usage[updated.run_id]
        usage.estimated_tokens_reserved = max(0, usage.estimated_tokens_reserved - updated.estimated_tokens)
        usage.tool_calls_reserved = max(0, usage.tool_calls_reserved - updated.estimated_tool_calls)
        usage.model_calls_reserved = max(0, usage.model_calls_reserved - updated.estimated_model_calls)
        return updated

    def usage(self, run_id: str) -> BudgetUsage:
        current = self._usage[run_id]
        return BudgetUsage(
            estimated_tokens_reserved=current.estimated_tokens_reserved,
            actual_tokens_committed=current.actual_tokens_committed,
            model_calls_reserved=current.model_calls_reserved,
            tool_calls_reserved=current.tool_calls_reserved,
            tool_calls_committed=current.tool_calls_committed,
        )

    def reservations(self, run_id: str | None = None) -> list[dict[str, object]]:
        records = [
            reservation.model_dump(mode="json")
            for reservation in self._reservations.values()
            if run_id is None or reservation.run_id == run_id
        ]
        return sorted(records, key=lambda item: str(item["reservation_id"]))

    def diagnostics(self, run_id: str) -> dict[str, object]:
        reservations = self.reservations(run_id)
        return {
            "usage": asdict(self.usage(run_id)),
            "approved": [item for item in reservations if item.get("approved") is True],
            "denied": [item for item in reservations if item.get("approved") is False],
            "committed": [item for item in reservations if item.get("committed") is True],
            "released": [item for item in reservations if item.get("released") is True],
            "reservations": reservations,
        }

    def _denied(
        self,
        *,
        run_id: str,
        agent_id: str | None,
        action_type: str,
        estimated_tokens: int,
        reason: str,
        estimated_tool_calls: int = 0,
        estimated_model_calls: int = 0,
    ) -> BudgetReservation:
        reservation = BudgetReservation(
            reservation_id=str(uuid4()),
            run_id=run_id,
            agent_id=agent_id,
            action_type=action_type,
            estimated_tokens=estimated_tokens,
            estimated_tool_calls=estimated_tool_calls,
            estimated_model_calls=estimated_model_calls,
            approved=False,
            reason=reason,
        )
        self._reservations[reservation.reservation_id] = reservation
        return reservation
