from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Protocol

from coder_workbench.agent_graph.schema import PlannerInputBundle, PlannerOrder
from coder_workbench.core import AgentWorkflowAgent, AgentWorkflowSpec
from coder_workbench.core.artifacts import validate_artifact


@dataclass(frozen=True)
class PlannerStrategyContext:
    agent_workflow: AgentWorkflowSpec
    request: str = ""
    round_number: int = 1
    previous_bundle: PlannerInputBundle | None = None
    previous_round_summary: dict[str, Any] | None = None
    planner_human_response: dict[str, Any] | None = None
    skill_index: Any | None = None
    repo_intelligence: dict[str, Any] | None = None
    initial_data: dict[str, Any] | None = None
    bundle: PlannerInputBundle | None = None


class PlannerStrategy(Protocol):
    def create_order(self, context: PlannerStrategyContext) -> PlannerOrder | None:
        ...

    def create_decision(self, context: PlannerStrategyContext) -> dict[str, Any] | None:
        ...


class FullPlannerStrategy:
    def create_order(self, context: PlannerStrategyContext) -> PlannerOrder | None:
        return None

    def create_decision(self, context: PlannerStrategyContext) -> dict[str, Any] | None:
        return None


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
        payload.setdefault("task_done", action in {"finish", "stop"})
        payload.setdefault("reason", "Replay PlannerDecision.")
        return validate_artifact(payload, expected_type="planner_decision")


class SimplePlannerStrategy:
    def __init__(self, *, single_worker: bool = False) -> None:
        self.single_worker = single_worker

    def create_order(self, context: PlannerStrategyContext) -> PlannerOrder | None:
        workers = _worker_agents(context.agent_workflow)
        if self.single_worker:
            workers = workers[:1]
        testers = _tester_agents(context.agent_workflow)
        tester_ids = [agent.id for agent in testers]
        final_tester_id = _final_tester_id(testers)
        repo_hint = _repo_hint(context.repo_intelligence)
        work_items = [
            {
                "work_item_id": f"{_safe_id(agent.id)}-work",
                "merge_index": index,
                "assignee_agent_id": agent.id,
                "task_summary": f"Local planner task for {agent.name or agent.id}. {repo_hint}".strip(),
                "depends_on": [],
                "tester_agent_ids": tester_ids,
            }
            for index, agent in enumerate(workers, start=1)
        ]
        return PlannerOrder.model_validate(
            {
                "artifact_type": "planner_order",
                "round": context.round_number,
                "round_goal": context.request,
                "plan_graph": {
                    "work_items": work_items,
                    "final_tester_agent_id": final_tester_id,
                },
            }
        )

    def create_decision(self, context: PlannerStrategyContext) -> dict[str, Any] | None:
        if context.bundle is None:
            return None
        return _local_decision(context.bundle, planner_human_response=context.planner_human_response)


def planner_strategy_for_mode(mode: str | None) -> PlannerStrategy:
    normalized = (mode or "full").strip().lower()
    if normalized == "replay":
        return ReplayPlannerStrategy()
    if normalized == "simple":
        return SimplePlannerStrategy()
    if normalized == "single_worker":
        return SimplePlannerStrategy(single_worker=True)
    return FullPlannerStrategy()


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


def _local_decision(
    bundle: PlannerInputBundle,
    *,
    planner_human_response: dict[str, Any] | None = None,
) -> dict[str, Any]:
    has_interrupts = bool(bundle.interrupts)
    has_failed_tests = any(item.execution_status == "failed" or item.test_status == "fail" for item in bundle.items)
    has_blocked_work = any(item.execution_status == "blocked" or item.test_status == "blocked" for item in bundle.items)
    has_debug_findings = any(effect.get("effect_type") == "debug_finding" for effect in bundle.effects)
    has_failed_runtime_actions = any(
        effect.get("effect_type") == "runtime_action" and effect.get("status") == "failed"
        for effect in bundle.effects
    )
    has_blocked_runtime_actions = any(
        effect.get("effect_type") == "runtime_action" and effect.get("status") == "blocked"
        for effect in bundle.effects
    )
    can_continue_from_interrupts = has_interrupts and all(
        interrupt.continue_without_human_possible is True
        for interrupt in bundle.interrupts
    )
    next_action = (
        "continue"
        if can_continue_from_interrupts or has_failed_tests or has_debug_findings or has_failed_runtime_actions
        else "ask_human"
        if has_interrupts or has_blocked_work or has_blocked_runtime_actions
        else "finish"
    )
    reason = (
        "DebugFinding is inside the current RunContract; local PlannerStrategy will replan."
        if has_debug_findings
        else "Runtime action failed; local PlannerStrategy will replan inside the current RunContract."
        if has_failed_runtime_actions
        else "Runtime action requires approval before it can continue."
        if has_blocked_runtime_actions
        else "Tests failed; local PlannerStrategy will replan inside the existing RunContract."
        if has_failed_tests
        else "Worker requested Planner intervention."
        if has_interrupts
        else "Work is blocked and requires Planner or user judgment."
        if has_blocked_work
        else (
            "Planner human response recorded; local PlannerStrategy resume completed."
            if planner_human_response
            else "Local PlannerStrategy execution and test artifacts are complete."
        )
    )
    payload = {
        "artifact_type": "planner_decision",
        "round": bundle.round,
        "task_done": next_action == "finish",
        "next_action": next_action,
        "risk_level": "medium" if has_interrupts or has_debug_findings or has_blocked_runtime_actions or has_failed_runtime_actions else "low",
        "requires_human_confirmation": next_action == "ask_human",
        "reason": reason,
        "next_round_goal": (
            "Fix debug finding evidence and rerun checks."
            if has_debug_findings
            else "Replan around failed runtime action evidence."
            if has_failed_runtime_actions
            else "Fix failing test evidence and rerun checks."
            if has_failed_tests
            else "Resolve the blocked work item."
            if next_action == "continue"
            else ""
        ),
        "remaining_auto_rounds": 2 if next_action == "continue" else 0,
        "human_message": (
            "Planner needs user input to resolve the blocked work item."
            if next_action == "ask_human"
            else None
        ),
    }
    return validate_artifact(payload, expected_type="planner_decision")


def _worker_agents(agent_workflow: AgentWorkflowSpec) -> list[AgentWorkflowAgent]:
    return [
        agent
        for agent in agent_workflow.agents
        if agent.id != agent_workflow.primary_planner_id and not _is_tester(agent)
    ]


def _tester_agents(agent_workflow: AgentWorkflowSpec) -> list[AgentWorkflowAgent]:
    return [agent for agent in agent_workflow.agents if _is_tester(agent)]


def _is_tester(agent: AgentWorkflowAgent) -> bool:
    return agent.role in {"tester", "reviewer"} or any(
        capability in agent.capabilities
        for capability in {"model_review", "optional_check_command", "aggregate_tests", "return_test_result"}
    )


def _final_tester_id(testers: list[AgentWorkflowAgent]) -> str | None:
    if len(testers) <= 1:
        return None
    aggregate = next((agent for agent in testers if "aggregate_tests" in agent.capabilities), None)
    return (aggregate or testers[-1]).id


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
