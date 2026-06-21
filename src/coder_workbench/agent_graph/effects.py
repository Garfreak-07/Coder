from __future__ import annotations

from typing import Any

from coder_workbench.agent_graph.artifacts import graph_artifact_id
from coder_workbench.agent_graph.cache import GraphRunCache
from coder_workbench.core import AgentWorkflowSpec
from coder_workbench.coding.risk_map import build_risk_map, is_risk_path
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
    commands = _requested_check_commands_from_artifacts(cache)
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

        output_ref = graph_artifact_id("check_output", index)
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
    changes = _requested_patch_changes_from_artifacts(cache)
    if not changes:
        return []

    risk_map = build_risk_map(repo_root).model_dump(mode="json")
    risky_changes = [
        change
        for change in changes
        if is_risk_path(str(change.get("path") or change.get("file") or ""), risk_map)
    ]
    if risky_changes:
        work_item_id = str(risky_changes[0].get("work_item_id") or "")
        execution = cache.execution_cache.get(work_item_id) if work_item_id else None
        record = {
            "effect_type": "modify_files",
            "status": "patch_preview_blocked",
            "work_item_id": work_item_id or None,
            "reason": "Proposed change targets a risk path.",
            "errors": [
                {
                    "path": str(change.get("path") or change.get("file") or ""),
                    "code": "risk_path",
                    "message": "Risk paths require Planner intervention before preview or apply.",
                }
                for change in risky_changes
            ],
        }
        cache.record_hidden_effect(record)
        cache.record_interrupt(
            {
                "round": cache.round,
                "work_item_id": work_item_id or "patch-preview",
                "merge_index": execution.merge_index if execution else 1,
                "agent_id": execution.agent_id if execution else "runtime",
                "blocker_type": "risk_boundary",
                "reason": "Patch preview blocked because proposed changes target risk paths.",
                "planner_question": "Should Planner reject this change, narrow scope, or ask the user for explicit permission?",
                "continue_without_human_possible": False,
                "candidate_options": [],
                "artifact_ref": graph_artifact_id("patch_preview", "blocked", work_item_id or "runtime"),
            }
        )
        return [record]

    preview = propose_patch({"changes": changes}, {"repo_root": repo_root, "scopes": scopes, "data": data})
    patch_ref = graph_artifact_id("patch_preview", preview["patch_id"])
    record = {
        "effect_type": "modify_files",
        "status": "patch_preview_created",
        "patch_ref": patch_ref,
        "change_count": preview.get("change_count", 0),
        "requires_approval": True,
    }
    cache.record_hidden_effect(record, output=preview)
    return [record]


def _requested_check_commands_from_artifacts(cache: GraphRunCache) -> list[dict[str, Any]]:
    commands: list[dict[str, Any]] = []
    for records in cache.test_cache.values():
        for record in records:
            artifact = record.artifact_payload or {}
            for command in _requested_check_commands(artifact.get("check_commands")):
                command.setdefault("work_item_id", artifact.get("work_item_id") or record.work_item_id)
                command.setdefault("tester_agent_id", artifact.get("tester_agent_id") or record.tester_agent_id)
                commands.append(command)
    return commands


def _requested_patch_changes_from_artifacts(cache: GraphRunCache) -> list[dict[str, Any]]:
    changes: list[dict[str, Any]] = []
    for record in cache.execution_cache.values():
        artifact = record.artifact_payload or {}
        for change in _requested_patch_changes(artifact.get("proposed_changes")):
            change.setdefault("work_item_id", artifact.get("work_item_id") or record.work_item_id)
            changes.append(change)
    return changes


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
