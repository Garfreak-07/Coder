from __future__ import annotations

from collections import defaultdict, deque
from typing import Any

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.core.schema import WorkflowSpec


class PreflightIssue(BaseModel):
    model_config = ConfigDict(extra="forbid")

    level: str
    code: str
    message: str
    target_type: str
    target_id: str | None = None


class PreflightResult(BaseModel):
    model_config = ConfigDict(extra="forbid")

    status: str
    issues: list[PreflightIssue] = Field(default_factory=list)
    summary: dict[str, Any] = Field(default_factory=dict)


def validate_workflow_preflight(
    workflow: WorkflowSpec,
    *,
    registered_tools: list[str] | None = None,
) -> dict[str, Any]:
    """Run product-level checks before starting a workflow run."""

    registered = set(registered_tools or [])
    node_by_id = workflow.node_by_id()
    outgoing: dict[str, list[str]] = defaultdict(list)
    issues: list[PreflightIssue] = []

    for edge in workflow.edges:
        outgoing[edge.from_node].append(edge.to_node)

    starts = [node for node in workflow.nodes if node.type == "start"]
    ends = [node for node in workflow.nodes if node.type == "end"]
    if len(starts) > 1:
        issues.append(_issue("error", "multiple_start_nodes", "Workflow must have exactly one Start node.", "workflow"))

    reachable = _reachable_from([node.id for node in starts], outgoing)
    for node in workflow.nodes:
        if node.id not in reachable:
            issues.append(_issue("warning", "unreachable_node", f"Node {node.id} is not reachable from Start.", "node", node.id))

    if ends and not any(end.id in reachable for end in ends):
        issues.append(_issue("error", "end_not_reachable", "No End node is reachable from Start.", "workflow"))

    output_owners: dict[str, str] = {}
    for node in workflow.nodes:
        if node.output_key:
            existing = output_owners.get(node.output_key)
            if existing:
                issues.append(
                    _issue(
                        "warning",
                        "output_key_conflict",
                        f"Output key {node.output_key!r} is produced by both {existing} and {node.id}.",
                        "node",
                        node.id,
                    )
                )
            else:
                output_owners[node.output_key] = node.id

        if node.type == "tool" and node.tool and registered and node.tool not in registered:
            issues.append(_issue("error", "unknown_tool", f"Tool {node.tool!r} is not registered.", "tool", node.tool))
        if node.type == "mcp_tool" and node.tool and not node.input.get("server_command"):
            issues.append(
                _issue("warning", "mcp_server_missing", f"MCP tool node {node.id} has no server_command.", "node", node.id)
            )
        if node.type == "loop" and not node.max_iterations:
            issues.append(
                _issue("error", "loop_max_iterations_missing", f"Loop node {node.id} needs max_iterations.", "node", node.id)
            )

    for edge in workflow.edges:
        if edge.to_node in node_by_id and node_by_id[edge.to_node].type == "loop":
            if edge.max_traversals is None and edge.from_node == edge.to_node:
                issues.append(
                    _issue(
                        "warning",
                        "loop_back_edge_unbounded",
                        f"Loop back edge {edge.from_node}->{edge.to_node} has no max_traversals.",
                        "edge",
                        f"{edge.from_node}->{edge.to_node}",
                    )
                )

    if workflow.max_steps < len(workflow.nodes):
        issues.append(
            _issue(
                "warning",
                "max_steps_low",
                "max_steps is lower than the number of nodes; the run may block before reaching End.",
                "workflow",
            )
        )
    if workflow.token_budget is not None and workflow.token_budget < 8000:
        issues.append(
            _issue("warning", "token_budget_low", "Token budget is low for a coding workflow.", "workflow")
        )

    status = "error" if any(issue.level == "error" for issue in issues) else "warning" if issues else "pass"
    return PreflightResult(
        status=status,
        issues=issues,
        summary={
            "nodes": len(workflow.nodes),
            "edges": len(workflow.edges),
            "agents": len(workflow.agents),
            "start_nodes": len(starts),
            "end_nodes": len(ends),
            "reachable_nodes": len(reachable),
            "max_steps": workflow.max_steps,
            "max_agent_calls": workflow.max_agent_calls,
            "max_tool_calls": workflow.max_tool_calls,
            "token_budget": workflow.token_budget,
        },
    ).model_dump(mode="json")


def _reachable_from(start_ids: list[str], outgoing: dict[str, list[str]]) -> set[str]:
    seen: set[str] = set()
    queue: deque[str] = deque(start_ids)
    while queue:
        node_id = queue.popleft()
        if node_id in seen:
            continue
        seen.add(node_id)
        queue.extend(outgoing.get(node_id, []))
    return seen


def _issue(level: str, code: str, message: str, target_type: str, target_id: str | None = None) -> PreflightIssue:
    return PreflightIssue(level=level, code=code, message=message, target_type=target_type, target_id=target_id)
