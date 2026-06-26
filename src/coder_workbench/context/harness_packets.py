from __future__ import annotations

from typing import Any


MAX_INLINE_TEXT_CHARS = 1200
MAX_INLINE_LIST_ITEMS = 10


def build_harness_context_packet(
    *,
    mode: str,
    user_goal: str,
    workflow_id: str,
    agent_id: str,
    planner_agent_id: str | None = None,
    round_number: int | None = None,
    work_item: Any | None = None,
    task_envelope: Any | None = None,
    state_view: dict[str, Any] | None = None,
    capability_set: dict[str, Any] | None = None,
    workflow_summary: dict[str, Any] | None = None,
    user_constraints: list[str] | None = None,
    selected_knowledge_pack_ids: list[str] | None = None,
    selected_skill_pack_ids: list[str] | None = None,
    selected_memory_pack_ids: list[str] | None = None,
    selected_knowledge_pack_summaries: list[dict[str, Any]] | None = None,
    selected_skill_pack_summaries: list[dict[str, Any]] | None = None,
    selected_memory_pack_summaries: list[dict[str, Any]] | None = None,
    project_summary: dict[str, Any] | None = None,
    run_contract: dict[str, Any] | None = None,
    planner_task_state: dict[str, Any] | None = None,
    round_summary: dict[str, Any] | None = None,
    execution_results: list[dict[str, Any]] | None = None,
    verification_summaries: list[dict[str, Any]] | None = None,
    blocked_reasons: list[str] | None = None,
    changed_files_summary: dict[str, Any] | None = None,
    current_decision_needed: str | None = None,
    constraints: list[str] | None = None,
    success_criteria: list[str] | None = None,
    sandbox_policy: dict[str, Any] | None = None,
    relevant_file_summaries: list[dict[str, Any]] | None = None,
    relevant_skill_context: list[dict[str, Any]] | None = None,
    previous_execution_summary: dict[str, Any] | None = None,
    evidence_refs: list[str] | None = None,
    native_event_refs: list[str] | None = None,
    diff_refs: list[str] | None = None,
    log_refs: list[str] | None = None,
    upstream_refs: list[str] | None = None,
    planner_order_refs: list[str] | None = None,
    file_refs: list[str] | None = None,
    knowledge_refs: list[str] | None = None,
    memory_refs: list[str] | None = None,
    repo_intelligence_refs: list[str] | None = None,
) -> dict[str, Any]:
    workflow_summary = workflow_summary or {"workflow_id": workflow_id}
    packet = {
        "schema_version": "harness-context-packet/v1",
        "mode": mode,
        "workflow_id": workflow_id,
        "agent_id": agent_id,
        "round": round_number,
        "hot": {
            "user_goal": user_goal,
        },
        "warm": {},
        "cold_refs": [],
    }
    if mode == "planning_chat":
        packet["hot"].update(
            {
                "selected_workflow": _compact_value(workflow_summary),
                "planner_agent_id": planner_agent_id or agent_id,
                "user_constraints": list(user_constraints or []),
                "selected_knowledge_pack_ids": list(selected_knowledge_pack_ids or []),
                "selected_skill_pack_ids": list(selected_skill_pack_ids or []),
                "selected_memory_pack_ids": list(selected_memory_pack_ids or []),
            }
        )
        packet["warm"]["workflow_summary"] = _compact_value(workflow_summary)
        if selected_knowledge_pack_summaries:
            packet["warm"]["selected_knowledge_pack_summaries"] = _compact_value(selected_knowledge_pack_summaries)
        if selected_skill_pack_summaries:
            packet["warm"]["selected_skill_pack_summaries"] = _compact_value(selected_skill_pack_summaries)
        if selected_memory_pack_summaries:
            packet["warm"]["selected_memory_pack_summaries"] = _compact_value(selected_memory_pack_summaries)
        if project_summary:
            packet["warm"]["project_summary"] = _compact_value(project_summary)
        _append_refs(packet, "knowledge", knowledge_refs or [])
        _append_refs(packet, "memory", memory_refs or [])
        _append_refs(packet, "repo_intelligence", repo_intelligence_refs or [])
    elif mode == "workflow_supervisor":
        packet["hot"]["planner_authority"] = "primary_planner"
        packet["hot"]["confirmed_goal"] = user_goal
        packet["hot"]["current_round"] = round_number
        packet["hot"]["current_decision_needed"] = current_decision_needed or "decide_continue_or_finish"
        packet["warm"]["run_state_summary"] = _state_summary(state_view or {})
        packet["warm"]["capability_summary"] = _capability_summary(capability_set or {})
        if planner_task_state:
            packet["warm"]["planner_task_state"] = _compact_value(planner_task_state)
        if run_contract:
            packet["warm"]["run_contract"] = _compact_value(run_contract)
        if round_summary:
            packet["warm"]["round_summary"] = _compact_value(round_summary)
        if execution_results:
            packet["warm"]["execution_result_summaries"] = _compact_value([_execution_result_summary(item) for item in execution_results])
        if verification_summaries:
            packet["warm"]["verification_summaries"] = _compact_value(verification_summaries)
        if blocked_reasons:
            packet["warm"]["blocked_reasons"] = list(blocked_reasons)[:MAX_INLINE_LIST_ITEMS]
        if changed_files_summary:
            packet["warm"]["changed_files_summary"] = _compact_value(changed_files_summary)
        _append_refs(packet, "evidence", evidence_refs or [])
        _append_refs(packet, "native_runtime", native_event_refs or [])
        _append_refs(packet, "diff", diff_refs or [])
        _append_refs(packet, "log", log_refs or [])
    elif mode == "task_execution":
        packet["hot"]["work_item"] = _compact_value(_model_or_dict(work_item))
        envelope = _model_or_dict(task_envelope)
        packet["hot"]["task_envelope"] = _task_envelope_summary(envelope)
        packet["hot"]["constraints"] = list(constraints or envelope.get("constraints") or [])
        packet["hot"]["success_criteria"] = list(success_criteria or envelope.get("success_criteria") or [])
        if sandbox_policy:
            packet["hot"]["sandbox_policy"] = _compact_value(sandbox_policy)
        packet["warm"]["capability_summary"] = _capability_summary(capability_set or {})
        if relevant_file_summaries:
            packet["warm"]["relevant_file_summaries"] = _compact_value(relevant_file_summaries)
        if relevant_skill_context:
            packet["warm"]["relevant_skill_context"] = _compact_value(relevant_skill_context)
        if previous_execution_summary:
            packet["warm"]["previous_execution_summary"] = _compact_value(previous_execution_summary)
        _append_refs(packet, "upstream", list(upstream_refs or envelope.get("upstream_refs") or []))
        _append_refs(
            packet,
            "planner_order",
            list(planner_order_refs or ([str(envelope.get("planner_order_ref"))] if envelope.get("planner_order_ref") else [])),
        )
        _append_refs(packet, "file", file_refs or [])
        _append_refs(packet, "evidence", evidence_refs or [])
    else:
        packet["warm"]["state_summary"] = _state_summary(state_view or {})
    return {key: value for key, value in packet.items() if value not in (None, {}, [])}


def _model_or_dict(value: Any) -> dict[str, Any]:
    if value is None:
        return {}
    if isinstance(value, dict):
        return dict(value)
    model_dump = getattr(value, "model_dump", None)
    if callable(model_dump):
        return model_dump(mode="json")
    return {"value": str(value)}


def _task_envelope_summary(envelope: dict[str, Any]) -> dict[str, Any]:
    keep = {
        "round",
        "work_item_id",
        "merge_index",
        "assigned_agent_id",
        "task_summary",
        "constraints",
        "allowed_skill_ids",
        "loaded_skill_refs",
        "omitted_skill_ids",
        "estimated_skill_tokens",
    }
    return {key: envelope[key] for key in keep if key in envelope}


def _state_summary(state_view: dict[str, Any]) -> dict[str, Any]:
    keep = {
        "status",
        "round",
        "workflow_id",
        "run_id",
        "current_goal",
        "plan_status",
        "completed_count",
        "blocked_count",
    }
    summary = {key: state_view[key] for key in keep if key in state_view}
    work_items = state_view.get("work_items")
    if isinstance(work_items, dict):
        summary["work_items"] = [
            _small_record(record)
            for record in work_items.values()
            if isinstance(record, dict)
        ][:10]
    return summary


def _capability_summary(capability_set: dict[str, Any]) -> dict[str, Any]:
    return {
        "tools": [tool.get("name") for tool in capability_set.get("tools", []) if isinstance(tool, dict)],
        "skills": [skill.get("skill_id") for skill in capability_set.get("skills", []) if isinstance(skill, dict)],
        "denied": [item.get("name") for item in capability_set.get("denied", []) if isinstance(item, dict)],
    }


def _small_record(record: dict[str, Any]) -> dict[str, Any]:
    keep = {"work_item_id", "agent_id", "status", "summary", "execution_result_ref"}
    return {key: record[key] for key in keep if key in record}


def _execution_result_summary(record: dict[str, Any]) -> dict[str, Any]:
    verification = record.get("verification") if isinstance(record.get("verification"), dict) else {}
    keep = {
        "artifact_id",
        "work_item_id",
        "agent_id",
        "status",
        "summary",
        "changed_files",
        "created_files",
        "deleted_files",
        "patch_refs",
        "evidence_refs",
        "blocker_type",
        "blocker_reason",
    }
    summary = {key: record[key] for key in keep if key in record}
    if verification:
        summary["verification"] = {
            key: verification[key]
            for key in {"status", "evidence_refs", "no_check_rationale", "remaining_work"}
            if key in verification
        }
    return summary


def _compact_value(value: Any) -> Any:
    if isinstance(value, str):
        return _compact_text(value)
    if isinstance(value, dict):
        return {
            str(key): _compact_value(item)
            for key, item in value.items()
            if str(key) not in {"raw_events", "raw_runtime_json", "terminal_log", "full_diff", "full_text"}
        }
    if isinstance(value, list):
        return [_compact_value(item) for item in value[:MAX_INLINE_LIST_ITEMS]]
    return value


def _compact_text(value: str) -> str | dict[str, Any]:
    if len(value) <= MAX_INLINE_TEXT_CHARS:
        return value
    return {
        "preview": value[:MAX_INLINE_TEXT_CHARS],
        "truncated": True,
        "size_chars": len(value),
    }


def _append_refs(packet: dict[str, Any], ref_type: str, refs: list[str]) -> None:
    clean_refs = [ref for ref in refs if ref]
    if clean_refs:
        packet.setdefault("cold_refs", []).append({"ref_type": ref_type, "refs": clean_refs})


__all__ = ["build_harness_context_packet"]
