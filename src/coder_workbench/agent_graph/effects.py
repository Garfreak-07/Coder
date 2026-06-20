from __future__ import annotations

from typing import Any

from coder_workbench.agent_graph.cache import GraphRunCache
from coder_workbench.core import AgentWorkflowSpec
from coder_workbench.tools import default_tool_registry
from coder_workbench.tools.patching import propose_patch


def apply_hidden_effects(
    *,
    agent_workflow: AgentWorkflowSpec,
    cache: GraphRunCache,
    repo_root: str,
    scopes: list[str],
    data: dict[str, Any],
) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    records.extend(_handle_optional_check_commands(agent_workflow, cache, repo_root, scopes, data))
    records.extend(_handle_patch_previews(agent_workflow, cache, repo_root, scopes, data))
    return records


def _handle_optional_check_commands(
    agent_workflow: AgentWorkflowSpec,
    cache: GraphRunCache,
    repo_root: str,
    scopes: list[str],
    data: dict[str, Any],
) -> list[dict[str, Any]]:
    if not any("optional_check_command" in agent.capabilities for agent in agent_workflow.agents):
        return []
    commands = _requested_check_commands(data.get("requested_check_commands"))
    if not commands:
        return []

    registry = default_tool_registry()
    records: list[dict[str, Any]] = []
    runtime_context = {"repo_root": repo_root, "scopes": scopes, "data": data}
    for index, command_request in enumerate(commands, start=1):
        result = registry.run(
            "run_check",
            {
                "command": command_request["command"],
                "cwd": command_request.get("cwd") or ".",
                "timeout_seconds": command_request.get("timeout_seconds", 120),
            },
            runtime_context,
        )
        if result.get("blocked"):
            record = {
                "effect_type": "optional_check_command",
                "status": "check_requires_planner_confirmation",
                "work_item_id": command_request.get("work_item_id"),
                "command": command_request["command"],
                "approval_key": result.get("approval_key"),
                "reason": result.get("message") or result.get("output"),
            }
            cache.record_hidden_effect(record)
            records.append(record)
            continue

        output_ref = f"memory:check_output:{index}"
        record = {
            "effect_type": "optional_check_command",
            "status": "completed" if result.get("passed") else "failed",
            "work_item_id": command_request.get("work_item_id"),
            "command": command_request["command"],
            "output_ref": output_ref,
            "passed": bool(result.get("passed")),
            "returncode": result.get("returncode"),
        }
        cache.record_hidden_effect(record, output=result)
        records.append(record)
    return records


def _handle_patch_previews(
    agent_workflow: AgentWorkflowSpec,
    cache: GraphRunCache,
    repo_root: str,
    scopes: list[str],
    data: dict[str, Any],
) -> list[dict[str, Any]]:
    if not any("modify_files" in agent.capabilities for agent in agent_workflow.agents):
        return []
    changes = _requested_patch_changes(data.get("proposed_changes"))
    if not changes:
        return []

    preview = propose_patch({"changes": changes}, {"repo_root": repo_root, "scopes": scopes, "data": data})
    patch_ref = f"memory:patch_preview:{preview['patch_id']}"
    record = {
        "effect_type": "modify_files",
        "status": "patch_preview_created",
        "patch_ref": patch_ref,
        "change_count": preview.get("change_count", 0),
        "requires_approval": True,
    }
    cache.record_hidden_effect(record, output=preview)
    return [record]


def _requested_check_commands(value: Any) -> list[dict[str, Any]]:
    if isinstance(value, str) and value.strip():
        return [{"command": value.strip()}]
    if isinstance(value, dict):
        if isinstance(value.get("commands"), list):
            return _requested_check_commands(value["commands"])
        if value.get("command"):
            return [dict(value)]
    if isinstance(value, list):
        commands: list[dict[str, Any]] = []
        for item in value:
            if isinstance(item, str) and item.strip():
                commands.append({"command": item.strip()})
            elif isinstance(item, dict) and item.get("command"):
                commands.append(dict(item))
        return commands
    return []


def _requested_patch_changes(value: Any) -> list[dict[str, Any]]:
    if isinstance(value, dict):
        if isinstance(value.get("changes"), list):
            return [dict(item) for item in value["changes"] if isinstance(item, dict)]
        if value.get("path"):
            return [dict(value)]
    if isinstance(value, list):
        return [dict(item) for item in value if isinstance(item, dict)]
    return []
