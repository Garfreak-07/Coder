from __future__ import annotations

import json
from typing import Any

from coder_workbench.agent_graph.schema import AgentTaskEnvelope, PlannerInputBundle, WorkItem
from coder_workbench.core import AgentWorkflowAgent, AgentWorkflowSpec


def build_planner_order_prompt(
    *,
    request: str,
    agent_workflow: AgentWorkflowSpec,
) -> str:
    return "\n\n".join(
        [
            _json_only_header("planner_order"),
            "You are the primary Planner. Split the user request into a small AgentGraph plan.",
            "Only use Agents that exist in the supplied AgentWorkflow.",
            "depends_on is the only execution dependency. Do not use merge_index to force execution order.",
            "If work items can run independently, leave depends_on empty.",
            "merge_index is only the stable result presentation order returned to Planner.",
            _planner_order_schema_notes(),
            "User request:",
            request,
            "AgentWorkflow JSON:",
            _compact_json(_workflow_summary(agent_workflow)),
        ]
    )


def build_worker_execution_prompt(
    *,
    agent: AgentWorkflowAgent,
    item: WorkItem,
    envelope: AgentTaskEnvelope,
) -> str:
    return "\n\n".join(
        [
            _json_only_header("execution_result"),
            "You are a Worker Agent. Return execution facts only.",
            "Do not ask the human. Do not make global continue/finish decisions.",
            "If changes are needed, describe them in proposed_changes; do not claim files were modified directly.",
            _execution_result_schema_notes(),
            "Assigned Agent JSON:",
            _compact_json(_agent_summary(agent)),
            "Work item JSON:",
            _compact_json(item.model_dump(mode="json")),
            "AgentTaskEnvelope JSON:",
            _compact_json(envelope.model_dump(mode="json")),
        ]
    )


def build_tester_prompt(
    *,
    tester: AgentWorkflowAgent,
    item: WorkItem,
    execution_result: dict[str, Any],
) -> str:
    return "\n\n".join(
        [
            _json_only_header("test_result"),
            "You are a Tester Agent. Return evidence only.",
            "Do not ask the human. Do not make global continue/finish decisions.",
            "Use check_commands only for optional commands that would materially improve evidence.",
            _test_result_schema_notes(),
            "Tester Agent JSON:",
            _compact_json(_agent_summary(tester)),
            "Work item JSON:",
            _compact_json(item.model_dump(mode="json")),
            "ExecutionResult JSON:",
            _compact_json(execution_result),
        ]
    )


def build_final_tester_prompt(
    *,
    final_tester: AgentWorkflowAgent,
    bundle: PlannerInputBundle,
) -> str:
    return "\n\n".join(
        [
            _json_only_header("test_result"),
            "You are the Final Tester Agent. Aggregate local tester evidence across the full PlannerInputBundle.",
            "Return one test_result for the whole round, not per-work-item output.",
            "Do not ask the human. Do not make global continue/finish decisions.",
            _test_result_schema_notes(),
            "Final Tester Agent JSON:",
            _compact_json(_agent_summary(final_tester)),
            "PlannerInputBundle JSON:",
            _compact_json(bundle.model_dump(mode="json", exclude_none=True)),
        ]
    )


def build_planner_decision_prompt(
    *,
    planner: AgentWorkflowAgent,
    bundle: PlannerInputBundle,
    planner_human_response: dict[str, Any] | None = None,
) -> str:
    parts = [
        _json_only_header("planner_decision"),
        "You are the primary Planner. You are the only Agent that can decide continue, ask_human, finish, or stop.",
        "Use ask_human only when the next step requires user judgment or approval.",
        _planner_decision_schema_notes(),
        "Planner Agent JSON:",
        _compact_json(_agent_summary(planner)),
        "PlannerInputBundle JSON:",
        _compact_json(bundle.model_dump(mode="json", exclude_none=True)),
    ]
    if planner_human_response:
        parts.extend(["Planner human response JSON:", _compact_json(planner_human_response)])
    return "\n\n".join(parts)


def schema_notes_for_artifact(artifact_type: str) -> str:
    if artifact_type == "planner_order":
        return _planner_order_schema_notes()
    if artifact_type == "execution_result":
        return _execution_result_schema_notes()
    if artifact_type == "test_result":
        return _test_result_schema_notes()
    if artifact_type == "planner_decision":
        return _planner_decision_schema_notes()
    return "Return a JSON object with artifact_type set to the expected type."


def _json_only_header(artifact_type: str) -> str:
    return (
        "Return JSON only. Do not include markdown, commentary, transcript, or code fences.\n"
        f"The JSON object must have artifact_type={artifact_type!r}."
    )


def _planner_order_schema_notes() -> str:
    return (
        "Required planner_order fields: artifact_type, round, round_goal, plan_graph.\n"
        "plan_graph.work_items is a list of objects with work_item_id, merge_index, assignee_agent_id, "
        "task_summary, depends_on, tester_agent_ids.\n"
        "plan_graph.final_tester_agent_id is optional."
    )


def _execution_result_schema_notes() -> str:
    return (
        "Required execution_result fields: artifact_type, status, summary.\n"
        "Allowed status values: completed, blocked, failed.\n"
        "Optional fields include proposed_changes, changed_files, created_files, deleted_files, patch_refs, "
        "outputs, unexpected_issues, out_of_contract, needs_planner_decision, tester_notes."
    )


def _test_result_schema_notes() -> str:
    return (
        "Required test_result fields: artifact_type, status, summary.\n"
        "Allowed status values: pass, fail, blocked.\n"
        "Optional fields include evidence, issues, remaining_work, confidence, check_commands, check_outputs_ref."
    )


def _planner_decision_schema_notes() -> str:
    return (
        "Required planner_decision fields: artifact_type, task_done, next_action, reason.\n"
        "Allowed next_action values: continue, ask_human, finish, stop.\n"
        "If next_action is ask_human, include human_message."
    )


def _workflow_summary(agent_workflow: AgentWorkflowSpec) -> dict[str, Any]:
    return {
        "id": agent_workflow.id,
        "primary_planner_id": agent_workflow.primary_planner_id,
        "agents": [_agent_summary(agent) for agent in agent_workflow.agents],
        "edges": [
            edge.model_dump(mode="json", by_alias=True, exclude_none=True)
            for edge in agent_workflow.edges
        ],
    }


def _agent_summary(agent: AgentWorkflowAgent) -> dict[str, Any]:
    return {
        "id": agent.id,
        "name": agent.name,
        "role": agent.role,
        "model_tier": agent.model_tier,
        "capabilities": agent.capabilities,
        "can_talk_to_human": agent.can_talk_to_human,
    }


def _compact_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, indent=2)[:8000]
