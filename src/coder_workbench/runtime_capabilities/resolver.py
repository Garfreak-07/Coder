from __future__ import annotations

from typing import Any

from coder_workbench.harness_runtime.contracts import (
    CONVERSATION_HARNESS_ID,
    TASK_EXECUTION_HARNESS_ID,
    resolve_harness_id,
)

from .registries import (
    code_worker_denied_capabilities,
    code_worker_memory_scopes,
    code_worker_tool_capabilities,
    planner_denied_capabilities,
    planner_memory_scopes,
    planner_tool_capabilities,
)
from .schema import CapabilitySet, DeniedCapability, SkillCapability, ToolCapability


CODE_WORKER_HARNESS_ID = "code-worker-harness"
FINAL_REPORT_HARNESS_ID = "final-report-harness"
PLANNER_DECISION_HARNESS_ID = "planner-decision-harness"
PLANNER_ORDER_HARNESS_ID = "planner-order-harness"
HARNESS_ROLES = {
    CONVERSATION_HARNESS_ID: "planner",
    TASK_EXECUTION_HARNESS_ID: "executor",
    CODE_WORKER_HARNESS_ID: "executor",
    FINAL_REPORT_HARNESS_ID: "planner",
    PLANNER_DECISION_HARNESS_ID: "planner",
    PLANNER_ORDER_HARNESS_ID: "planner",
}

PLANNING_CHAT_TOOLS = {
    "inspect_workflow",
    "inspect_skill_index",
    "inspect_memory",
    "inspect_project_summary",
    "validate_run_contract_draft",
    "estimate_risk",
    "estimate_budget",
}
WORKFLOW_SUPERVISOR_TOOLS = {
    "inspect_artifact",
    "inspect_run_state",
    "inspect_evidence",
    "inspect_round_summary",
    "validate_planner_order",
    "validate_planner_decision",
    "build_final_report",
    "estimate_risk",
    "estimate_budget",
}


def resolve_capabilities(
    *,
    agent: Any,
    runtime_profile: Any,
    harness_id: str,
    work_item: Any = None,
    state_view: dict[str, Any] | None = None,
    installed_capabilities: Any = None,
) -> CapabilitySet:
    canonical_harness_id, alias_mode = resolve_harness_id(harness_id)
    contract_role = HARNESS_ROLES.get(canonical_harness_id)
    if contract_role is None:
        raise ValueError(f"unknown harness_id {harness_id!r}")
    role = str(getattr(agent, "role", "") or getattr(runtime_profile, "role", "") or contract_role)
    mode = str(getattr(runtime_profile, "mode", "") or alias_mode or "")
    if contract_role == "planner" or role == "planner":
        return _planner_capability_set(
            harness_id=harness_id,
            canonical_harness_id=canonical_harness_id,
            mode=mode,
            installed_capabilities=installed_capabilities,
        )
    return _code_worker_capability_set(
        runtime_profile=runtime_profile,
        work_item=work_item,
        state_view=state_view or {},
        installed_capabilities=installed_capabilities,
    )


def _planner_capability_set(
    *,
    harness_id: str,
    canonical_harness_id: str,
    mode: str,
    installed_capabilities: Any,
) -> CapabilitySet:
    tools = planner_tool_capabilities()
    if canonical_harness_id == CONVERSATION_HARNESS_ID and mode == "planning_chat":
        tools = [tool for tool in tools if tool.name in PLANNING_CHAT_TOOLS]
    elif canonical_harness_id == CONVERSATION_HARNESS_ID and mode == "workflow_supervisor":
        tools = [tool for tool in tools if tool.name in WORKFLOW_SUPERVISOR_TOOLS]
    if harness_id == FINAL_REPORT_HARNESS_ID:
        tools = [tool for tool in tools if tool.name in {"inspect_artifact", "inspect_run_state", "inspect_evidence", "build_final_report"}]
    elif harness_id == PLANNER_ORDER_HARNESS_ID:
        tools = [tool for tool in tools if tool.name != "validate_planner_decision"]
    elif harness_id == PLANNER_DECISION_HARNESS_ID:
        tools = [tool for tool in tools if tool.name != "validate_planner_order"]
    return CapabilitySet(
        skills=_skill_capabilities(installed_capabilities, level="index"),
        tools=tools,
        memory_scopes=planner_memory_scopes(),
        denied=planner_denied_capabilities(),
    )


def _code_worker_capability_set(
    *,
    runtime_profile: Any,
    work_item: Any,
    state_view: dict[str, Any],
    installed_capabilities: Any,
) -> CapabilitySet:
    tools = code_worker_tool_capabilities()
    denied = code_worker_denied_capabilities()
    tool_policy = getattr(runtime_profile, "tool_policy", None)
    tool_policy = tool_policy if isinstance(tool_policy, dict) else {}
    allowed_tools: list[ToolCapability] = []
    for tool in tools:
        denied_reason = _denied_tool_reason(tool, tool_policy)
        if denied_reason:
            denied.append(DeniedCapability(name=tool.name, reason=denied_reason))
        else:
            allowed_tools.append(tool)
    if not state_view.get("assigned_work_item") and work_item is None:
        denied.append(DeniedCapability(name=CODE_WORKER_HARNESS_ID, reason="No assigned work item is available."))
    return CapabilitySet(
        skills=_skill_capabilities(installed_capabilities, level="summary"),
        tools=allowed_tools,
        memory_scopes=code_worker_memory_scopes(),
        denied=denied,
    )


def _denied_tool_reason(tool: ToolCapability, tool_policy: dict[str, Any]) -> str | None:
    if tool.name in {"propose_patch", "apply_patch_sandbox"} and not bool(tool_policy.get("write_files")):
        return "Runtime profile does not allow file writes."
    if tool.name == "run_command_sandbox" and not bool(tool_policy.get("run_commands")):
        return "Runtime profile does not allow command execution."
    if tool.name in {"read_file", "search_files"} and not bool(tool_policy.get("read_files", True)):
        return "Runtime profile does not allow file reads."
    return None


def _skill_capabilities(installed_capabilities: Any, *, level: str) -> list[SkillCapability]:
    skill_ids = _skill_ids(installed_capabilities)
    return [
        SkillCapability(skill_id=skill_id, level=level)  # type: ignore[arg-type]
        for skill_id in skill_ids
    ]


def _skill_ids(installed_capabilities: Any) -> list[str]:
    if installed_capabilities is None:
        return []
    if isinstance(installed_capabilities, dict):
        explicit = installed_capabilities.get("allowed_skill_ids")
        if isinstance(explicit, list):
            return _unique_strings(explicit)
        skill_index = installed_capabilities.get("skill_index")
        if isinstance(skill_index, dict):
            return _skill_ids_from_index_dict(skill_index)
    enabled = getattr(installed_capabilities, "enabled", None)
    if callable(enabled):
        try:
            return _unique_strings(getattr(skill, "id", "") for skill in enabled())
        except Exception:
            return []
    skills = getattr(installed_capabilities, "skills", None)
    if isinstance(skills, list):
        return _unique_strings(getattr(skill, "id", "") for skill in skills)
    if isinstance(installed_capabilities, list):
        return _unique_strings(
            item.get("id") if isinstance(item, dict) else str(item)
            for item in installed_capabilities
        )
    return []


def _skill_ids_from_index_dict(skill_index: dict[str, Any]) -> list[str]:
    skills = skill_index.get("skills")
    if not isinstance(skills, list):
        return []
    return _unique_strings(
        skill.get("id")
        for skill in skills
        if isinstance(skill, dict) and skill.get("enabled", True)
    )


def _unique_strings(values: Any) -> list[str]:
    seen: set[str] = set()
    output: list[str] = []
    for value in values:
        text = str(value or "").strip()
        if not text or text in seen:
            continue
        seen.add(text)
        output.append(text)
    return output


__all__ = ["resolve_capabilities"]
