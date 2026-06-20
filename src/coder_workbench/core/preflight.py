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
    tool_capabilities: dict[str, dict[str, Any]] | None = None,
    provider_status: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Run product-level checks before starting a workflow run."""

    capabilities = tool_capabilities or {}
    registered = set(registered_tools or capabilities.keys())
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

        runtime_tool = _runtime_tool_name(node.type, node.tool)
        if node.type in {"tool", "mcp_tool"} and runtime_tool and registered and runtime_tool not in registered:
            issues.append(_issue("error", "unknown_tool", f"Tool {node.tool!r} is not registered.", "tool", node.tool))
        if node.type == "mcp_tool" and node.tool and not node.input.get("server_command"):
            issues.append(
                _issue("warning", "mcp_server_missing", f"MCP tool node {node.id} has no server_command.", "node", node.id)
            )
        if node.type == "loop" and not node.max_iterations:
            issues.append(
                _issue("error", "loop_max_iterations_missing", f"Loop node {node.id} needs max_iterations.", "node", node.id)
            )

    for agent in workflow.agents:
        if provider_status:
            provider_issue = _provider_issue_for_agent(agent, provider_status)
            if provider_issue:
                issues.append(provider_issue)
        for tool_name in agent.tools:
            capability = capabilities.get(tool_name)
            if registered and tool_name not in registered:
                issues.append(
                    _issue("error", "agent_unknown_tool", f"Agent {agent.id} declares unknown tool {tool_name!r}.", "agent", agent.id)
                )
                continue
            if not capability:
                continue
            missing = _missing_permissions(agent.permissions, capability)
            if missing:
                issues.append(
                    _issue(
                        "error",
                        "agent_tool_permission_denied",
                        f"Agent {agent.id} declares {tool_name!r} but lacks permissions: {', '.join(missing)}.",
                        "agent",
                        agent.id,
                    )
                )
            if capability.get("requires_approval") and not agent.permissions.requires_approval:
                issues.append(
                    _issue(
                        "error",
                        "agent_tool_requires_approval",
                        f"Agent {agent.id} declares approval-gated tool {tool_name!r} without requiring approval.",
                        "agent",
                        agent.id,
                    )
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
            "tools": _tool_summaries(workflow, capabilities),
            "permission_summary": _permission_summary(workflow, capabilities),
            "provider_status": provider_status or {},
        },
    ).model_dump(mode="json")


def _provider_issue_for_agent(agent: Any, provider_status: dict[str, Any]) -> PreflightIssue | None:
    provider = str(getattr(agent, "provider", None) or provider_status.get("default_provider") or "").lower()
    providers = provider_status.get("providers")
    if not provider or not isinstance(providers, list):
        return None
    status = next((item for item in providers if isinstance(item, dict) and item.get("provider") == provider), None)
    if not status or status.get("configured"):
        return None
    return _issue(
        "warning",
        "provider_credentials_missing",
        f"Agent {agent.id} uses provider {provider!r}, but no credentials are configured; runtime will use mock output.",
        "agent",
        agent.id,
    )


def _runtime_tool_name(node_type: str, tool_name: str | None) -> str | None:
    if not tool_name:
        return None
    return "mcp_call" if node_type == "mcp_tool" else tool_name


def _tool_summaries(workflow: WorkflowSpec, capabilities: dict[str, dict[str, Any]]) -> list[dict[str, Any]]:
    summaries: list[dict[str, Any]] = []
    for node in workflow.nodes:
        if node.type not in {"tool", "mcp_tool"}:
            continue
        runtime_tool = _runtime_tool_name(node.type, node.tool)
        capability = capabilities.get(runtime_tool or "")
        summaries.append(
            {
                "node_id": node.id,
                "tool": node.tool,
                "runtime_tool": runtime_tool,
                "display_name": _capability_value(capability, "display_name", node.tool or "unconfigured"),
                "risk_level": _capability_value(capability, "risk_level", "unknown"),
                "permissions": _capability_list(capability, "permissions"),
                "requires_approval": bool(capability.get("requires_approval")) if capability else False,
            }
        )
    return summaries


def _permission_summary(workflow: WorkflowSpec, capabilities: dict[str, dict[str, Any]]) -> dict[str, Any]:
    permission_counts = {"read_files": 0, "edit_files": 0, "run_commands": 0, "use_network": 0}
    risk_counts = {"low": 0, "medium": 0, "high": 0, "unknown": 0}
    approval_required = 0
    for summary in _tool_summaries(workflow, capabilities):
        for permission in summary["permissions"]:
            if permission in permission_counts:
                permission_counts[permission] += 1
        risk = str(summary["risk_level"])
        risk_counts[risk if risk in risk_counts else "unknown"] += 1
        if summary["requires_approval"]:
            approval_required += 1
    return {
        "permissions": permission_counts,
        "risk": risk_counts,
        "approval_required_tools": approval_required,
    }


def _missing_permissions(permission_policy: Any, capability: dict[str, Any]) -> list[str]:
    return [
        permission
        for permission in _capability_list(capability, "permissions")
        if not bool(getattr(permission_policy, permission, False))
    ]


def _capability_value(capability: dict[str, Any] | None, key: str, fallback: Any) -> Any:
    if not capability:
        return fallback
    return capability.get(key, fallback)


def _capability_list(capability: dict[str, Any] | None, key: str) -> list[str]:
    if not capability:
        return []
    value = capability.get(key)
    if not isinstance(value, list):
        return []
    return [str(item) for item in value]


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
