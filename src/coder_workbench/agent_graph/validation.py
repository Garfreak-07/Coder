from __future__ import annotations

from collections import deque

from coder_workbench.agent_graph.schema import PlannerOrder
from coder_workbench.core import (
    AgentWorkflowSpec,
    AgentWorkflowValidationError,
    AgentWorkflowValidationIssue,
    AgentWorkflowValidationResult,
)


def validate_planner_order(agent_workflow: AgentWorkflowSpec, planner_order: PlannerOrder) -> AgentWorkflowValidationResult:
    issues: list[AgentWorkflowValidationIssue] = []
    agent_by_id = {agent.id: agent for agent in agent_workflow.agents}
    reachable_from_planner = _reachable_agent_ids(agent_workflow, agent_workflow.primary_planner_id)
    seen_work_items: set[str] = set()

    for item in planner_order.plan_graph.work_items:
        if item.work_item_id in seen_work_items:
            issues.append(
                _issue(
                    "duplicate_work_item_id",
                    f'PlannerOrder work_item_id "{item.work_item_id}" is duplicated.',
                    "work_item",
                    item.work_item_id,
                )
            )
        seen_work_items.add(item.work_item_id)
        if item.assignee_agent_id not in agent_by_id:
            issues.append(
                _issue(
                    "planner_order_assignee_not_found",
                    f'PlannerOrder assigns "{item.work_item_id}" to unknown Agent "{item.assignee_agent_id}".',
                    "work_item",
                    item.work_item_id,
                )
            )
        elif item.assignee_agent_id not in reachable_from_planner:
            issues.append(
                _issue(
                    "planner_order_assignee_not_reachable",
                    f'PlannerOrder assigns "{item.work_item_id}" to an Agent outside the Planner reachable graph.',
                    "work_item",
                    item.work_item_id,
                )
            )

        reachable_from_assignee = _reachable_agent_ids(agent_workflow, item.assignee_agent_id)
        for tester_id in item.tester_agent_ids:
            if tester_id not in agent_by_id:
                issues.append(
                    _issue(
                        "planner_order_tester_not_found",
                        f'PlannerOrder references unknown tester "{tester_id}".',
                        "work_item",
                        item.work_item_id,
                    )
                )
            elif tester_id not in reachable_from_assignee:
                issues.append(
                    _issue(
                        "planner_order_tester_not_connected",
                        f'Tester "{tester_id}" is not reachable from assignee "{item.assignee_agent_id}".',
                        "work_item",
                        item.work_item_id,
                    )
                )

        for upstream_id in item.depends_on:
            if upstream_id not in seen_work_items and upstream_id not in {candidate.work_item_id for candidate in planner_order.plan_graph.work_items}:
                issues.append(
                    _issue(
                        "planner_order_dependency_not_found",
                        f'Work item "{item.work_item_id}" depends on unknown work item "{upstream_id}".',
                        "work_item",
                        item.work_item_id,
                    )
                )

    distinct_testers = sorted(
        {
            tester_id
            for item in planner_order.plan_graph.work_items
            for tester_id in item.tester_agent_ids
        }
    )
    if len(distinct_testers) > 1:
        final_tester_id = planner_order.plan_graph.final_tester_agent_id
        if not final_tester_id:
            issues.append(
                _issue(
                    "missing_final_tester",
                    "PlannerOrder with multiple tester Agents must include final_tester_agent_id.",
                    "plan_graph",
                )
            )
        elif final_tester_id not in agent_by_id:
            issues.append(
                _issue(
                    "final_tester_not_found",
                    f'Final tester "{final_tester_id}" is not a user-added Agent.',
                    "plan_graph",
                    final_tester_id,
                )
            )
        elif "aggregate_tests" not in agent_by_id[final_tester_id].capabilities:
            issues.append(
                _issue(
                    "final_tester_missing_aggregate_tests",
                    f'Final tester "{final_tester_id}" must use aggregate_tests.',
                    "agent",
                    final_tester_id,
                )
            )

    return _validation_result(issues)


def assert_valid_planner_order(agent_workflow: AgentWorkflowSpec, planner_order: PlannerOrder) -> None:
    result = validate_planner_order(agent_workflow, planner_order)
    if result.status == "error":
        raise AgentWorkflowValidationError(result)


def _reachable_agent_ids(spec: AgentWorkflowSpec, start_id: str) -> set[str]:
    graph: dict[str, list[str]] = {}
    for edge in spec.edges:
        if edge.loop:
            continue
        graph.setdefault(edge.from_agent, []).append(edge.to_agent)
    reachable: set[str] = set()
    queue: deque[str] = deque(graph.get(start_id, []))
    while queue:
        agent_id = queue.popleft()
        if agent_id in reachable:
            continue
        reachable.add(agent_id)
        queue.extend(graph.get(agent_id, []))
    return reachable


def _validation_result(issues: list[AgentWorkflowValidationIssue]) -> AgentWorkflowValidationResult:
    status = "error" if any(issue.level == "error" for issue in issues) else "pass"
    return AgentWorkflowValidationResult(status=status, issues=issues, summary={})


def _issue(
    code: str,
    message: str,
    target_type: str,
    target_id: str | None = None,
) -> AgentWorkflowValidationIssue:
    return AgentWorkflowValidationIssue(
        level="error",
        code=code,
        message=message,
        target_type=target_type,
        target_id=target_id,
    )
