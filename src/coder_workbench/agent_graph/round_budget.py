from __future__ import annotations

from typing import Any

from coder_workbench.budget import BudgetBroker
from coder_workbench.runtime_kernel import RunController


def evaluate_round_budget_preflight(
    *,
    broker: BudgetBroker,
    controller: RunController,
    run_id: str,
    planner_order: Any,
    estimated_model_calls: int,
    estimated_tool_calls: int,
    estimated_context_tokens_per_call: int,
) -> tuple[dict[str, object], Any]:
    report = broker.preflight_round(
        run_id=run_id,
        planner_order=planner_order,
        estimated_model_calls=estimated_model_calls,
        estimated_tool_calls=estimated_tool_calls,
        estimated_context_tokens_per_call=estimated_context_tokens_per_call,
    )
    decision = controller.evaluate_budget_preflight(report)
    return report.as_dict(), decision
