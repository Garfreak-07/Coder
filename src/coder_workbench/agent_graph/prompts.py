from __future__ import annotations

import json
from typing import Any

from coder_workbench.agent_graph.schema import AgentTaskEnvelope, PlannerInputBundle, WorkItem
from coder_workbench.agent_harness.contracts import CODE_WORKER_HARNESS, PLANNER_DECISION_HARNESS, PLANNER_ORDER_HARNESS
from coder_workbench.agent_harness.prompt_layers import (
    PromptLayer,
    harness_contract_layer,
    instruction_layer,
    json_layer,
    output_contract_layer,
    render_prompt_layers,
    text_layer,
)
from coder_workbench.core import AgentWorkflowAgent, AgentWorkflowSpec
from coder_workbench.skills.index import SkillIndex


def build_planner_order_prompt(
    *,
    request: str,
    agent_workflow: AgentWorkflowSpec,
    previous_bundle: PlannerInputBundle | None = None,
    previous_round_summary: dict[str, Any] | None = None,
    skill_index: SkillIndex | None = None,
    repo_intelligence: dict[str, Any] | None = None,
    state_view: dict[str, Any] | None = None,
    capability_set: dict[str, Any] | None = None,
    round_number: int = 1,
) -> str:
    layers: list[PromptLayer | None] = [
        output_contract_layer(artifact_type="planner_order", schema_notes=_planner_order_schema_notes()),
        instruction_layer(
            layer_id="planner_rules",
            title="Planner Order Rules",
            instructions=[
                "You are the primary Planner. Split the user request into a small AgentGraph plan.",
                "Only use Agents that exist in the supplied AgentWorkflow.",
                "depends_on is the only execution dependency. Do not use merge_index to force execution order.",
                "If work items can run independently, leave depends_on empty.",
                "merge_index is only the stable result presentation order returned to Planner.",
                "If previous PlannerInputBundle or RoundSummary exists, plan only remaining or corrective work.",
                "Do not repeat completed work unless necessary. New work_item_id values should be unique for this round.",
                "Use repo intelligence before creating work_items.",
                "Do not create vague work items.",
                "Assign work_items to reachable Agents only.",
                "Use depends_on only for real execution dependency.",
                "Use merge_index only for result presentation.",
                "Do not touch risk_files unless the RunContract allows it.",
            ],
        ),
        harness_contract_layer(PLANNER_ORDER_HARNESS.harness_id),
        text_layer(layer_id="round", title="Round number", content=round_number),
        text_layer(layer_id="user_request", title="User request", content=request),
        json_layer(layer_id="agent_workflow", title="AgentWorkflow JSON", value=_workflow_summary(agent_workflow)),
    ]
    if repo_intelligence:
        layers.extend(
            [
                json_layer(layer_id="repo_index", title="RepoIndex summary JSON", value=repo_intelligence.get("repo_index", {})),
                json_layer(layer_id="command_discovery", title="CommandDiscovery summary JSON", value=repo_intelligence.get("command_discovery", {})),
                json_layer(layer_id="risk_map", title="RiskMap summary JSON", value=repo_intelligence.get("risk_map", {})),
                json_layer(layer_id="symbol_index", title="SymbolIndex summary JSON", value=_symbol_index_summary(repo_intelligence.get("symbol_index", {}))),
            ]
        )
    if state_view:
        layers.append(json_layer(layer_id="state_view", title="PlannerStateView JSON", value=state_view))
    if capability_set:
        layers.append(json_layer(layer_id="capability_set", title="Resolved CapabilitySet JSON", value=capability_set))
    if skill_index and skill_index.skills:
        layers.extend(
            [
                json_layer(layer_id="skill_index", title="Installed SkillIndex JSON", value=skill_index.model_dump(mode="json")),
                text_layer(
                    layer_id="skill_index_rules",
                    title="SkillIndex Rules",
                    content="Use the SkillIndex for capability awareness, but do not paste Skill bodies into PlannerOrder.",
                ),
            ]
        )
    if previous_bundle:
        layers.append(
            json_layer(
                layer_id="previous_planner_input_bundle",
                title="Previous PlannerInputBundle JSON",
                value=previous_bundle.model_dump(mode="json", exclude_none=True),
            )
        )
        debug_findings = _debug_findings_from_effects(previous_bundle.effects)
        if debug_findings:
            layers.append(text_layer(layer_id="debug_findings", title="Debug findings from previous round", content=_debug_findings_text(debug_findings)))
    if previous_round_summary:
        layers.append(json_layer(layer_id="previous_round_summary", title="Previous RoundSummary JSON", value=previous_round_summary))
    return render_prompt_layers(layers)


def build_worker_execution_prompt(
    *,
    agent: AgentWorkflowAgent,
    item: WorkItem,
    envelope: AgentTaskEnvelope,
    capability_set: dict[str, Any] | None = None,
) -> str:
    return render_prompt_layers(
        [
            output_contract_layer(artifact_type="execution_result", schema_notes=_execution_result_schema_notes()),
            instruction_layer(
                layer_id="executor_rules",
                title="Executor Rules",
                instructions=[
                    "You are an Executor Agent. Return execution facts only.",
                    "Use the fixed lifecycle: inspect the WorkItem, execute only inside its scope, verify the result, then report.",
                    "Do not ask the human. Do not make global continue/finish decisions.",
                    "All file edits, commands, connector calls, and external effects must go through the runtime ActionGateway; do not claim a side effect happened unless the runtime evidence exists.",
                    "The Executor may run allowed checks, but check/test evidence must be stored inside execution_result.verification.",
                    "If you cannot safely complete the work item, return status=\"blocked\", "
                    "needs_planner_decision=true, blocker_type, executor_recovery_exhausted=true, "
                    "blocker_reason, planner_recommendation, remaining_work or affected_files, verification, "
                    "and constraint_boundary.",
                    "Do not ask the human directly. Do not decide whether to continue the whole workflow.",
                    "If changes are needed, describe them in proposed_changes; do not claim files were modified directly.",
                ],
            ),
            harness_contract_layer(CODE_WORKER_HARNESS.harness_id),
            json_layer(layer_id="assigned_agent", title="Assigned Agent JSON", value=_agent_summary(agent)),
            json_layer(layer_id="work_item", title="Work item JSON", value=item.model_dump(mode="json")),
            json_layer(layer_id="agent_task_envelope", title="AgentTaskEnvelope JSON", value=envelope.model_dump(mode="json")),
            json_layer(layer_id="coding_context_packet", title="CodingContextPacket JSON", value=envelope.coding_context_packet),
            json_layer(layer_id="capability_set", title="Resolved CapabilitySet JSON", value=capability_set or envelope.capability_set),
            json_layer(layer_id="selected_skill_context", title="Selected Skill context JSON", value=envelope.selected_skill_context),
        ]
    )


def build_worker_tool_loop_prompt(
    *,
    base_prompt: str,
    item: WorkItem,
    envelope: AgentTaskEnvelope,
    prepared_context: dict[str, Any],
    capability_set: dict[str, Any] | None = None,
) -> str:
    return render_prompt_layers(
        [
            instruction_layer(
                layer_id="tool_loop_contract",
                title="CodeWorker Tool Loop Contract",
                instructions=[
                    "Return exactly one JSON object. Do not include markdown, commentary, transcript, or code fences.",
                    "Allowed artifact_type values are harness_action and execution_result.",
                    "Use harness_action when you need runtime evidence before continuing.",
                    "Use execution_result only when the WorkItem is complete or blocked.",
                    "Do not claim file edits, command results, diffs, or evidence unless prior runtime observations support them.",
                    "Do not fabricate observations. Observations are produced only by runtime.",
                    "Do not ask the human. Do not return planner_decision or final_report.",
                    "Non-sandbox run_command is forbidden. Use run_command_sandbox only when command execution is needed.",
                ],
            ),
            text_layer(layer_id="base_worker_prompt", title="Base executor prompt", content=base_prompt),
            json_layer(layer_id="work_item", title="Work item JSON", value=item.model_dump(mode="json")),
            json_layer(layer_id="agent_task_envelope", title="AgentTaskEnvelope JSON", value=envelope.model_dump(mode="json")),
            json_layer(layer_id="capability_set", title="Resolved CapabilitySet JSON", value=capability_set or envelope.capability_set),
            json_layer(layer_id="prepared_context", title="Prepared CodeWorker context JSON", value=prepared_context),
            text_layer(
                layer_id="action_schema",
                title="Harness action schema",
                content=(
                    '{"artifact_type":"harness_action","action_id":"step-1","action_type":"read_file",'
                    '"payload":{"path":"src/example.py"},"reason":"Inspect the file before editing.",'
                    '"risk_level":"low","expected_evidence":["file content preview"]}'
                ),
            ),
            text_layer(
                layer_id="final_schema",
                title="Execution result schema",
                content=(
                    '{"artifact_type":"execution_result","status":"completed","summary":"Done.",'
                    '"verification":{"status":"pass","checks_run":[],"evidence_refs":[],"confidence":"medium",'
                    '"remaining_work":[],"no_check_rationale":null,"repair_attempted":false,"repair_summary":null}}'
                ),
            ),
        ]
    )


def build_planner_decision_prompt(
    *,
    planner: AgentWorkflowAgent,
    bundle: PlannerInputBundle,
    state_view: dict[str, Any] | None = None,
    capability_set: dict[str, Any] | None = None,
) -> str:
    layers: list[PromptLayer | None] = [
        output_contract_layer(artifact_type="planner_decision", schema_notes=_planner_decision_schema_notes()),
        instruction_layer(
            layer_id="planner_rules",
            title="Planner Decision Rules",
            instructions=[
                "You are the primary Planner. You are the only Agent that can decide continue or finish.",
                "Use continue when the problem can be handled within the existing user agreement.",
                "Use finish with final_status=blocked when work cannot proceed safely inside the current agreement.",
                "Use finish with final_status=failed when verification proves the task failed and recovery is exhausted.",
                "Use finish with no final_status when the task is complete. Do not ignore interrupts.",
            ],
        ),
        harness_contract_layer(PLANNER_DECISION_HARNESS.harness_id),
        json_layer(layer_id="planner_agent", title="Planner Agent JSON", value=_agent_summary(planner)),
        json_layer(layer_id="planner_input_bundle", title="PlannerInputBundle JSON", value=bundle.model_dump(mode="json", exclude_none=True)),
    ]
    if state_view:
        layers.append(json_layer(layer_id="state_view", title="PlannerStateView JSON", value=state_view))
    if capability_set:
        layers.append(json_layer(layer_id="capability_set", title="Resolved CapabilitySet JSON", value=capability_set))
    debug_findings = _debug_findings_from_effects(bundle.effects)
    if debug_findings:
        layers.extend(
            [
                text_layer(layer_id="debug_findings", title="Debug findings from previous round", content=_debug_findings_text(debug_findings)),
                instruction_layer(
                    layer_id="debug_finding_rules",
                    title="Debug Finding Rules",
                    instructions=[
                        "If debug finding is within RunContract, prefer continue/replan.",
                        "Finish with final_status=blocked if the fix exceeds boundary or the same error repeated.",
                    ],
                ),
            ]
        )
    return render_prompt_layers(layers)


def schema_notes_for_artifact(artifact_type: str) -> str:
    if artifact_type == "planner_order":
        return _planner_order_schema_notes()
    if artifact_type == "execution_result":
        return _execution_result_schema_notes()
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
        "task_summary, depends_on.\n"
        "Do not include legacy review-agent workflow fields."
    )


def _execution_result_schema_notes() -> str:
    return (
        "Required execution_result fields: artifact_type, status, summary, verification.\n"
        "Allowed status values: completed, blocked. Never return failed for execution_result.\n"
        "verification.status must be one of pass, fail, blocked, skipped.\n"
        "Store all check/test evidence inside execution_result.verification.\n"
        "If verification.status is fail or blocked, execution_result.status must be blocked.\n"
        "If verification.status is skipped, include no_check_rationale or evidence_refs.\n"
        "Optional fields include proposed_changes, changed_files, created_files, deleted_files, patch_refs, "
        "outputs, unexpected_issues, out_of_contract, needs_planner_decision, blocker_type, "
        "executor_recovery_exhausted, blocker_reason, blocker_fingerprint, recovery_attempts, "
        "planner_recommendation, replan_goal, affected_files, constraint_boundary, planner_question, "
        "candidate_options, planner_options, continue_without_human_possible, attempted_actions, "
        "evidence_refs, remaining_work, no_op_rationale.\n"
        "Blocked execution_result requires executor_recovery_exhausted=true, blocker_reason, "
        "planner_recommendation, verification, and remaining_work or affected_files or evidence_refs.\n"
        "Allowed blocker_type values: test_failed, command_failed, command_unavailable, missing_dependency, "
        "missing_file, scope_violation, risk_path_blocked, permission_boundary, missing_secret, "
        "network_required, external_account_required, timeout, schema_validation_failed, context_missing, "
        "tool_unavailable, sandbox_unavailable, unknown_error.\n"
        "candidate_options is a list of objects with option_id, summary, risk_level, and requires_human."
    )


def _planner_decision_schema_notes() -> str:
    return (
        "Required planner_decision fields: artifact_type, task_done, next_action, reason.\n"
        "Allowed next_action values: continue, finish.\n"
        "When finish does not mean completed, include final_status as blocked, failed, or cancelled."
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


def _debug_findings_from_effects(effects: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [
        effect
        for effect in effects
        if isinstance(effect, dict) and effect.get("effect_type") == "debug_finding"
    ]


def _debug_findings_text(findings: list[dict[str, Any]]) -> str:
    lines: list[str] = []
    for finding in findings[:8]:
        likely_files = finding.get("likely_files")
        lines.append(
            "- work_item_id={work_item_id}; failure_summary={failure_summary}; likely_files={likely_files}; raw_output_ref={raw_output_ref}".format(
                work_item_id=str(finding.get("work_item_id") or ""),
                failure_summary=str(finding.get("failure_summary") or ""),
                likely_files=", ".join(str(item) for item in likely_files) if isinstance(likely_files, list) else "",
                raw_output_ref=str(finding.get("raw_output_ref") or finding.get("output_ref") or ""),
            )
        )
    return "\n".join(lines)


def _symbol_index_summary(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        return {}
    files = value.get("files") if isinstance(value.get("files"), list) else []
    return {
        "artifact_type": value.get("artifact_type", "symbol_index"),
        "parser": value.get("parser"),
        "languages": value.get("languages", []),
        "file_count": len(files),
        "files": [
            {
                "path": item.get("path"),
                "symbols": item.get("symbols", [])[:12],
            }
            for item in files[:40]
            if isinstance(item, dict)
        ],
    }
