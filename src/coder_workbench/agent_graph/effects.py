from __future__ import annotations

from typing import Any

from coder_workbench.actions import ActionGateway, ActionSpec, RunContext, RuntimeActionRecord, action_completed_payload
from coder_workbench.agent_graph.artifacts import graph_artifact_id
from coder_workbench.agent_graph.cache import GraphRunCache
from coder_workbench.core import AgentWorkflowSpec


def apply_hidden_effects(
    *,
    agent_workflow: AgentWorkflowSpec,
    cache: GraphRunCache,
    repo_root: str,
    scopes: list[str],
    data: dict[str, Any],
    action_gateway: ActionGateway | None = None,
) -> list[dict[str, Any]]:
    gateway = action_gateway or ActionGateway()
    records: list[dict[str, Any]] = []
    records.extend(_handle_patch_previews(agent_workflow, cache, repo_root, scopes, data, gateway))
    records.extend(_handle_optional_check_commands(agent_workflow, cache, repo_root, scopes, data, gateway))
    records.extend(_handle_requested_runtime_actions(agent_workflow, cache, repo_root, scopes, data, gateway))
    return records


def replay_approved_runtime_actions(
    *,
    cache: GraphRunCache,
    repo_root: str,
    scopes: list[str],
    data: dict[str, Any],
    action_gateway: ActionGateway | None = None,
) -> list[dict[str, Any]]:
    gateway = action_gateway or ActionGateway()
    entries = _approved_runtime_actions_from_data(data)
    records: list[dict[str, Any]] = []
    for index, entry in enumerate(entries, start=1):
        action_spec = _action_spec_from_approved_runtime_action(entry)
        if action_spec is None:
            continue
        replay_input = dict(action_spec.input)
        replay_input["approved"] = True
        approval_key = str(entry.get("approval_key") or replay_input.get("approval_key") or "")
        if approval_key:
            replay_input["approval_key"] = approval_key
        replay_spec = action_spec.model_copy(
            update={
                "action_id": f"replay:{action_spec.action_id}:{cache.round}:{index}",
                "input": replay_input,
            }
        )
        action_run_context = RunContext(
            run_id=str(data.get("run_id") or "agent-graph"),
            repo_root=repo_root,
            scopes=scopes,
            data=data,
        )
        action = gateway.run(replay_spec, run_context=action_run_context)
        output_ref = graph_artifact_id("tool_result", "replay", "round", cache.round, index)
        record = _runtime_action_record(
            action_spec=action_spec,
            completed_spec=replay_spec,
            action=action,
            output_ref=output_ref,
            work_item_id=_work_item_id_from_entry(entry, action_spec),
            requested_action=dict(entry.get("requested_action") or {}),
            replay_of=str(entry.get("replay_of") or entry.get("artifact_ref") or "") or None,
        )
        cache.record_hidden_effect(record, output=_runtime_action_output(action))
        records.append(record)
    return records


def _handle_requested_runtime_actions(
    agent_workflow: AgentWorkflowSpec,
    cache: GraphRunCache,
    repo_root: str,
    scopes: list[str],
    data: dict[str, Any],
    action_gateway: ActionGateway,
) -> list[dict[str, Any]]:
    requests = _requested_runtime_actions_from_artifacts(cache)
    if not requests:
        return []

    records: list[dict[str, Any]] = []
    for index, request in enumerate(requests, start=1):
        action_type = str(request.get("action_type") or "call_plugin")
        action_spec = ActionSpec(
            action_id=f"{action_type}:{cache.round}:{index}",
            action_type=action_type,
            input=dict(request),
            risk_level=_risk_level(request.get("risk_level")),
            requires_permission=bool(request.get("requires_permission")),
        )
        action_run_context = RunContext(
            run_id=str(data.get("run_id") or "agent-graph"),
            repo_root=repo_root,
            scopes=scopes,
            data=data,
        )
        action = action_gateway.run(
            action_spec,
            run_context=action_run_context,
        )

        output_ref = graph_artifact_id("tool_result", "round", cache.round, index)
        record = _runtime_action_record(
            action_spec=action_spec,
            completed_spec=action_spec,
            action=action,
            output_ref=output_ref,
            work_item_id=str(request.get("work_item_id") or "") or None,
            requested_action=request,
        )
        cache.record_hidden_effect(record, output=_runtime_action_output(action))
        records.append(record)
    return records


def _handle_optional_check_commands(
    agent_workflow: AgentWorkflowSpec,
    cache: GraphRunCache,
    repo_root: str,
    scopes: list[str],
    data: dict[str, Any],
    action_gateway: ActionGateway,
) -> list[dict[str, Any]]:
    if not any("optional_check_command" in agent.capabilities for agent in agent_workflow.agents):
        return []
    commands = _requested_check_commands_from_artifacts(cache)
    if not commands:
        return []

    records: list[dict[str, Any]] = []
    for index, command_request in enumerate(commands, start=1):
        action_spec = ActionSpec(
            action_id=f"run_command:{cache.round}:{index}",
            action_type="run_command_sandbox",
            input={
                "command": str(command_request["command"]),
                "cwd": str(command_request.get("cwd") or "."),
                "timeout_seconds": int(command_request.get("timeout_seconds") or 120),
            },
        )
        action_run_context = RunContext(
            run_id=str(data.get("run_id") or "agent-graph"),
            repo_root=repo_root,
            sandbox_root=_sandbox_root_from_data(data),
            scopes=scopes,
            data=data,
        )
        action = action_gateway.run(
            action_spec,
            run_context=action_run_context,
        )
        result = dict(action.payload.get("result") or {})
        check_artifact_ref = graph_artifact_id("check_result", "round", cache.round, index)
        output_ref = graph_artifact_id("check_output", "round", cache.round, index)
        if action.status == "blocked" or result.get("blocked"):
            record = {
                "effect_type": "optional_check_command",
                "action_type": "run_command_sandbox",
                "status": "check_requires_planner_confirmation",
                "work_item_id": command_request.get("work_item_id"),
                "artifact_ref": check_artifact_ref,
                "output_ref": output_ref,
                "requires_planner_replan": True,
                "command": command_request["command"],
                "approval_key": result.get("approval_key"),
                "reason": result.get("message") or result.get("output"),
                "action": action_completed_payload(action_spec, action),
            }
            cache.record_hidden_effect(record, output=result)
            records.append(record)
            continue

        record = {
            "effect_type": "optional_check_command",
            "action_type": "run_command_sandbox",
            "status": "completed" if result.get("passed") else "failed",
            "work_item_id": command_request.get("work_item_id"),
            "artifact_ref": check_artifact_ref,
            "command": command_request["command"],
            "output_ref": output_ref,
            "requires_planner_replan": not bool(result.get("passed")),
            "reason": action.summary,
            "passed": bool(result.get("passed")),
            "returncode": result.get("returncode"),
            "action": action_completed_payload(action_spec, action),
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
    action_gateway: ActionGateway,
) -> list[dict[str, Any]]:
    if not any("modify_files" in agent.capabilities for agent in agent_workflow.agents):
        return []
    changes = _requested_patch_changes_from_artifacts(cache)
    if not changes:
        return []

    action_spec = ActionSpec(
        action_id=f"propose_patch:{cache.round}",
        action_type="propose_patch",
        input={"changes": changes},
    )
    action_run_context = RunContext(
        run_id=str(data.get("run_id") or "agent-graph"),
        repo_root=repo_root,
        scopes=scopes,
        data=data,
    )
    action = action_gateway.run(
        action_spec,
        run_context=action_run_context,
    )
    preview = dict(action.payload.get("preview") or {})
    if action.status == "failed":
        artifact_ref = graph_artifact_id("patch_preview", "failed", cache.round)
        record = {
            "effect_type": "modify_files",
            "action_type": "propose_patch",
            "status": "patch_preview_failed",
            "work_item_id": None,
            "artifact_ref": artifact_ref,
            "output_ref": artifact_ref,
            "requires_planner_replan": True,
            "reason": action.summary,
            "error_code": action.error_code,
            "action": action_completed_payload(action_spec, action),
        }
        cache.record_hidden_effect(record, output={"status": "failed", "message": action.summary, "error_code": action.error_code})
        return [record]
    if preview.get("status") == "blocked":
        risky_changes = list(preview.get("risky_changes") or [])
        work_item_id = str(risky_changes[0].get("work_item_id") or "") if risky_changes else ""
        execution = cache.execution_cache.get(work_item_id) if work_item_id else None
        artifact_ref = graph_artifact_id("patch_preview", "blocked", work_item_id or "runtime")
        record = {
            "effect_type": "modify_files",
            "action_type": "propose_patch",
            "status": "patch_preview_blocked",
            "work_item_id": work_item_id or None,
            "artifact_ref": artifact_ref,
            "output_ref": artifact_ref,
            "requires_planner_replan": True,
            "reason": str(preview.get("message") or "Proposed change targets a risk path."),
            "errors": list(preview.get("risk_errors") or []),
            "action": action_completed_payload(action_spec, action),
        }
        cache.record_hidden_effect(record, output=preview)
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
                "artifact_ref": artifact_ref,
            }
        )
        return [record]

    patch_ref = graph_artifact_id("patch_preview", preview["patch_id"])
    record = {
        "effect_type": "modify_files",
        "action_type": "propose_patch",
        "status": "patch_preview_created",
        "work_item_id": _single_work_item_id(changes),
        "artifact_ref": patch_ref,
        "patch_ref": patch_ref,
        "output_ref": patch_ref,
        "change_count": preview.get("change_count", 0),
        "requires_approval": True,
        "requires_planner_replan": False,
        "reason": action.summary,
        "action": action_completed_payload(action_spec, action),
    }
    cache.record_hidden_effect(record, output=preview)
    records = [record]
    sandbox_root = _sandbox_root_from_data(data)
    if sandbox_root is not None:
        apply_spec = ActionSpec(
            action_id=f"apply_patch_sandbox:{cache.round}",
            action_type="apply_patch_sandbox",
            input={"patch": preview},
        )
        apply_run_context = RunContext(
            run_id=str(data.get("run_id") or "agent-graph"),
            repo_root=repo_root,
            sandbox_root=sandbox_root,
            scopes=scopes,
            data=data,
        )
        action = action_gateway.run(
            apply_spec,
            run_context=apply_run_context,
        )
        result = dict(action.payload.get("result") or {})
        apply_ref = graph_artifact_id("sandbox_apply", cache.round)
        apply_record = {
            "effect_type": "sandbox_apply",
            "action_type": "apply_patch_sandbox",
            "status": "applied" if result.get("status") == "applied" else action.status,
            "work_item_id": record.get("work_item_id"),
            "artifact_ref": apply_ref,
            "patch_ref": patch_ref,
            "output_ref": apply_ref,
            "sandbox_root": action.payload.get("sandbox_root"),
            "sandbox_unavailable": action.payload.get("sandbox_unavailable", False),
            "requires_planner_replan": result.get("status") != "applied",
            "reason": action.summary,
            "action": action_completed_payload(apply_spec, action),
        }
        cache.record_hidden_effect(apply_record, output=result)
        records.append(apply_record)
    return records


def _requested_check_commands_from_artifacts(cache: GraphRunCache) -> list[dict[str, Any]]:
    commands: list[dict[str, Any]] = []
    for record in cache.execution_cache.values():
        artifact = record.artifact_payload or {}
        for command in _requested_check_commands(artifact.get("requested_actions")):
            command.setdefault("work_item_id", artifact.get("work_item_id") or record.work_item_id)
            commands.append(command)
        for command in _requested_check_commands(artifact.get("check_commands")):
            command.setdefault("work_item_id", artifact.get("work_item_id") or record.work_item_id)
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


def _requested_runtime_actions_from_artifacts(cache: GraphRunCache) -> list[dict[str, Any]]:
    actions: list[dict[str, Any]] = []
    for record in cache.execution_cache.values():
        artifact = record.artifact_payload or {}
        raw = artifact.get("requested_actions")
        if not isinstance(raw, list):
            continue
        for item in raw:
            if not isinstance(item, dict):
                continue
            payload = dict(item)
            payload.setdefault("work_item_id", artifact.get("work_item_id") or record.work_item_id)
            actions.append(payload)
    return actions


def _approved_runtime_actions_from_data(data: dict[str, Any]) -> list[dict[str, Any]]:
    values: list[Any] = []
    raw = data.get("approved_runtime_actions")
    if isinstance(raw, list):
        values.extend(raw)
    actions: list[dict[str, Any]] = []
    seen: set[str] = set()
    for item in values:
        if isinstance(item, dict):
            action = dict(item)
            key = repr(
                (
                    action.get("approval_key"),
                    action.get("replay_of") or action.get("artifact_ref"),
                    action.get("action_spec"),
                )
            )
            if key in seen:
                continue
            seen.add(key)
            actions.append(action)
    return actions


def _action_spec_from_approved_runtime_action(entry: dict[str, Any]) -> ActionSpec | None:
    raw = entry.get("action_spec") if isinstance(entry.get("action_spec"), dict) else entry
    if not isinstance(raw, dict):
        return None
    try:
        return ActionSpec.model_validate(raw)
    except Exception:
        return None


def _work_item_id_from_entry(entry: dict[str, Any], action_spec: ActionSpec) -> str | None:
    value = entry.get("work_item_id") or action_spec.input.get("work_item_id")
    return str(value or "") or None


def _runtime_action_record(
    *,
    action_spec: ActionSpec,
    completed_spec: ActionSpec,
    action: Any,
    output_ref: str,
    work_item_id: str | None,
    requested_action: dict[str, Any],
    replay_of: str | None = None,
) -> dict[str, Any]:
    payload = action.payload if isinstance(action.payload, dict) else {}
    record = RuntimeActionRecord(
        action_type=action_spec.action_type,
        status=action.status,
        work_item_id=work_item_id,
        artifact_ref=output_ref,
        output_ref=output_ref,
        tool_result_ref=output_ref,
        requires_planner_replan=action.status != "ok",
        reason=action.summary,
        error_code=action.error_code,
        operation_id=_operation_id(action_spec),
        approval_key=str(payload.get("approval_key") or "") or None,
        policy=dict(payload.get("policy") or {}),
        action_spec=action_spec.model_dump(mode="json", exclude_none=True),
        requested_action=dict(requested_action),
        replay_of=replay_of,
        action=action_completed_payload(completed_spec, action),
    )
    return record.model_dump(mode="json", exclude_none=True)


def _runtime_action_output(action: Any) -> dict[str, Any]:
    output = dict(action.payload if isinstance(action.payload, dict) else {})
    output.setdefault("status", action.status)
    output.setdefault("summary", action.summary)
    if action.error_code:
        output.setdefault("error_code", action.error_code)
    return output


def _operation_id(action_spec: ActionSpec) -> str | None:
    value = action_spec.input.get("operation_id") or action_spec.input.get("mcp_operation_id")
    if value is None and action_spec.action_type == "call_mcp":
        value = "mcp_call"
    return str(value or "") or None


def _risk_level(value: Any) -> str:
    risk = str(value or "low").strip().lower()
    return risk if risk in {"low", "medium", "high"} else "low"


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


def _single_work_item_id(changes: list[dict[str, Any]]) -> str | None:
    work_item_ids = {
        str(change.get("work_item_id") or "")
        for change in changes
        if str(change.get("work_item_id") or "").strip()
    }
    if len(work_item_ids) == 1:
        return next(iter(work_item_ids))
    return None


def _sandbox_root_from_data(data: dict[str, Any]) -> str | None:
    value = data.get("sandbox_root")
    if isinstance(value, str) and value.strip():
        return value
    return None
