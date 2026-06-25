from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Protocol

from coder_workbench.agent_graph.schema import PlannerInputBundle, PlannerOrder
from coder_workbench.core import AgentWorkflowAgent, AgentWorkflowSpec
from coder_workbench.core.artifacts import validate_artifact


POLICY_BLOCKER_TYPES = {
    "scope_violation",
    "risk_path_blocked",
    "permission_boundary",
    "sandbox_unavailable",
}


@dataclass(frozen=True)
class PlannerStrategyContext:
    agent_workflow: AgentWorkflowSpec
    request: str = ""
    round_number: int = 1
    previous_bundle: PlannerInputBundle | None = None
    previous_round_summary: dict[str, Any] | None = None
    skill_index: Any | None = None
    repo_intelligence: dict[str, Any] | None = None
    initial_data: dict[str, Any] | None = None
    bundle: PlannerInputBundle | None = None


class PlannerStrategy(Protocol):
    def create_order(self, context: PlannerStrategyContext) -> PlannerOrder | None:
        ...

    def create_decision(self, context: PlannerStrategyContext) -> dict[str, Any] | None:
        ...


class ReplayPlannerStrategy:
    def create_order(self, context: PlannerStrategyContext) -> PlannerOrder | None:
        value = (context.initial_data or {}).get("planner_order")
        if not isinstance(value, dict):
            return None
        payload = {key: value[key] for key in ("artifact_type", "round", "round_goal", "plan_graph") if key in value}
        return PlannerOrder.model_validate(payload)

    def create_decision(self, context: PlannerStrategyContext) -> dict[str, Any] | None:
        value = (context.initial_data or {}).get("planner_decision")
        if not isinstance(value, dict):
            return None
        payload = dict(value)
        payload.setdefault("artifact_type", "planner_decision")
        payload.setdefault("round", context.bundle.round if context.bundle is not None else context.round_number)
        action = str(payload.get("next_action") or "")
        payload.setdefault("task_done", action in {"finish", "stop"} and payload.get("final_status") in {None, "completed"})
        payload.setdefault("reason", "Replay PlannerDecision.")
        return validate_artifact(payload, expected_type="planner_decision")


class SimplePlannerStrategy:
    def __init__(self, *, single_executor: bool = False) -> None:
        self.single_executor = single_executor

    def create_order(self, context: PlannerStrategyContext) -> PlannerOrder | None:
        executors = _executor_agents(context.agent_workflow)
        if self.single_executor:
            executors = executors[:1]
        repo_hint = _repo_hint(context.repo_intelligence)
        work_items = [
            {
                "work_item_id": f"{_safe_id(agent.id)}-work",
                "merge_index": index,
                "assignee_agent_id": agent.id,
                "task_summary": f"Local planner task for {agent.name or agent.id}. {repo_hint}".strip(),
                "depends_on": [],
            }
            for index, agent in enumerate(executors, start=1)
        ]
        return PlannerOrder.model_validate(
            {
                "artifact_type": "planner_order",
                "round": context.round_number,
                "round_goal": context.request,
                "plan_graph": {
                    "work_items": work_items,
                },
            }
        )

    def create_decision(self, context: PlannerStrategyContext) -> dict[str, Any] | None:
        if context.bundle is None:
            return None
        return _local_decision(context.bundle)


def planner_strategy_for_mode(mode: str | None) -> PlannerStrategy:
    normalized = (mode or "full").strip().lower()
    if normalized == "replay":
        return ReplayPlannerStrategy()
    if normalized in {"full", "simple"}:
        return SimplePlannerStrategy()
    if normalized == "single_executor":
        return SimplePlannerStrategy(single_executor=True)
    return SimplePlannerStrategy()


def planner_mode_from(initial_data: dict[str, Any] | None, runtime_settings: Any | None) -> str:
    data_mode = (initial_data or {}).get("planner_mode")
    if isinstance(data_mode, str) and data_mode.strip():
        return data_mode.strip().lower()
    settings_mode = getattr(runtime_settings, "planner_mode", None)
    if isinstance(settings_mode, str) and settings_mode.strip():
        return settings_mode.strip().lower()
    if isinstance(runtime_settings, dict):
        dict_mode = runtime_settings.get("planner_mode")
        if isinstance(dict_mode, str) and dict_mode.strip():
            return dict_mode.strip().lower()
    return "full"


def _local_decision(bundle: PlannerInputBundle) -> dict[str, Any]:
    has_interrupts = bool(bundle.interrupts)
    has_failed_verification = any(item.verification_status == "fail" for item in bundle.items)
    has_blocked_work = any(item.execution_status == "blocked" or item.verification_status == "blocked" for item in bundle.items)
    has_debug_findings = any(effect.get("effect_type") == "debug_finding" for effect in bundle.effects)
    has_failed_check_effects = any(
        effect.get("effect_type") == "optional_check_command"
        and (effect.get("status") == "failed" or effect.get("passed") is False)
        for effect in bundle.effects
    )
    has_blocked_check_effects = any(
        effect.get("effect_type") == "optional_check_command"
        and effect.get("status") == "check_requires_planner_confirmation"
        for effect in bundle.effects
    )
    has_failed_runtime_actions = any(
        effect.get("effect_type") == "runtime_action" and effect.get("status") == "failed"
        for effect in bundle.effects
    )
    has_blocked_runtime_actions = any(
        effect.get("effect_type") == "runtime_action" and effect.get("status") == "blocked"
        for effect in bundle.effects
    )
    has_policy_blockers = any(interrupt.blocker_type in POLICY_BLOCKER_TYPES for interrupt in bundle.interrupts) or any(
        effect.get("blocker_type") in POLICY_BLOCKER_TYPES
        or effect.get("policy") in {"sandbox", "security"}
        or effect.get("status") in {"sandbox_policy_blocked", "security_policy_blocked"}
        for effect in bundle.effects
    )
    can_continue_from_interrupts = has_interrupts and all(
        interrupt.continue_without_human_possible is True
        for interrupt in bundle.interrupts
    ) and not has_policy_blockers
    blocked_requires_finish = (
        has_policy_blockers
        or has_interrupts
        or has_blocked_work
        or has_blocked_check_effects
        or has_blocked_runtime_actions
    ) and not can_continue_from_interrupts
    next_action = (
        "continue"
        if not has_policy_blockers
        and (
            can_continue_from_interrupts
            or has_failed_verification
            or has_debug_findings
            or has_failed_check_effects
            or has_failed_runtime_actions
        )
        else "finish"
    )
    final_status = "blocked" if blocked_requires_finish else None
    reason = (
        "Sandbox or security policy blocked progress; local PlannerStrategy will finish blocked."
        if has_policy_blockers
        else "DebugFinding is inside the current RunContract; local PlannerStrategy will retry within the current RunContract."
        if has_debug_findings
        else "Check result failed; local PlannerStrategy will retry inside the current RunContract."
        if has_failed_check_effects
        else "Check command requires Planner confirmation before it can continue."
        if has_blocked_check_effects
        else "Runtime action failed; local PlannerStrategy will retry inside the current RunContract."
        if has_failed_runtime_actions
        else "Runtime action requires approval before it can continue."
        if has_blocked_runtime_actions
        else "Execution verification failed; local PlannerStrategy will retry inside the existing RunContract."
        if has_failed_verification
        else "Executor requested Planner intervention."
        if has_interrupts
        else "Work is blocked and requires Planner or user judgment."
        if has_blocked_work
        else "Local PlannerStrategy execution artifacts are complete."
    )
    payload = {
        "artifact_type": "planner_decision",
        "round": bundle.round,
        "task_done": next_action == "finish" and final_status is None,
        "next_action": next_action,
        "final_status": final_status or ("completed" if next_action == "finish" else None),
        "risk_level": "medium"
        if has_policy_blockers
        or has_interrupts
        or has_debug_findings
        or has_failed_check_effects
        or has_blocked_check_effects
        or has_blocked_runtime_actions
        or has_failed_runtime_actions
        else "low",
        "requires_human_confirmation": False,
        "reason": reason,
        "next_round_goal": (
            "Fix debug finding evidence and rerun checks."
            if has_debug_findings
            else "Fix failed check evidence and rerun checks."
            if has_failed_check_effects
            else "Replan around failed runtime action evidence."
            if has_failed_runtime_actions
            else "Fix failed execution verification and rerun checks."
            if has_failed_verification
            else "Resolve the blocked work item."
            if next_action == "continue"
            else ""
        ),
        "remaining_auto_rounds": 2 if next_action == "continue" else 0,
        "human_message": None,
    }
    return validate_artifact(payload, expected_type="planner_decision")


def _executor_agents(agent_workflow: AgentWorkflowSpec) -> list[AgentWorkflowAgent]:
    return [
        agent
        for agent in agent_workflow.agents
        if agent.id != agent_workflow.primary_planner_id
    ]


def _repo_hint(repo_intelligence: dict[str, Any] | None) -> str:
    if not repo_intelligence:
        return ""
    repo_index = repo_intelligence.get("repo_index") if isinstance(repo_intelligence.get("repo_index"), dict) else {}
    important = [str(item) for item in repo_index.get("important_files", [])][:2]
    if not important:
        return ""
    return f"Use repo intelligence files: {', '.join(important)}."


def _safe_id(value: str) -> str:
    safe = "".join(char if char.isalnum() or char == "_" else "_" for char in value.strip())
    return safe or "agent"
