from __future__ import annotations

import json
from typing import Any, Protocol

from coder_workbench.core import AgentSpec
from coder_workbench.config import RuntimeConfig, load_runtime_config
from coder_workbench.llm import create_chat_model


class AgentExecutor(Protocol):
    def run(self, agent: AgentSpec, context: dict[str, Any]) -> dict[str, Any]:
        ...


class DefaultAgentExecutor:
    """Token-conscious agent adapter.

    If credentials are available, this uses the existing OpenAI-compatible chat
    adapter. Otherwise it returns deterministic structured output so workflow
    routing can be developed and tested without spending tokens.
    """

    def __init__(self, runtime_settings: Any | None = None) -> None:
        self.runtime_settings = runtime_settings

    def run(self, agent: AgentSpec, context: dict[str, Any]) -> dict[str, Any]:
        if self.runtime_settings is not None:
            from coder_workbench.server.settings import resolve_settings_config

            values = resolve_settings_config(self.runtime_settings, agent.provider, agent.model)
            config = RuntimeConfig(
                provider=str(values["provider"]),
                model=str(values["model"]),
                api_key=values["api_key"],
                base_url=values["base_url"],
            )
        else:
            config = load_runtime_config(agent.provider, agent.model)
        if not config.has_llm_credentials:
            return self._mock(agent, context)

        model = create_chat_model(config)
        prompt = self._build_prompt(agent, context)
        response = model.invoke(prompt)
        content = getattr(response, "content", str(response))
        parsed = _try_parse_json(content)
        if isinstance(parsed, dict):
            return parsed
        return {
            "summary": content[:1200],
            "raw": content,
            "status": "completed",
        }

    def _build_prompt(self, agent: AgentSpec, context: dict[str, Any]) -> str:
        return "\n\n".join(
            [
                f"Role: {agent.role}",
                f"Goal: {agent.goal}",
                "Instructions:",
                agent.instructions or "Return concise structured JSON.",
                "Token policy: use the supplied summaries first. Ask for only the minimum extra context needed.",
                "If asked to modify files, return JSON containing a `changes` array with objects shaped as "
                "{path, action, content}. Never claim that files were modified directly.",
                f"Required artifact type: {agent.artifact_type or 'none'}.",
                "Context JSON:",
                json.dumps(context, ensure_ascii=False, indent=2),
                "Return JSON only.",
            ]
        )

    def _mock(self, agent: AgentSpec, context: dict[str, Any]) -> dict[str, Any]:
        request = context.get("request", "")
        summaries = context.get("state_summaries", {})
        if agent.artifact_type == "run_contract":
            return {
                "artifact_type": "run_contract",
                "user_goal": str(request),
                "done_criteria": [
                    "Planner has produced a decision for the current request.",
                    "Executor and Tester returned structured facts for the round.",
                ],
                "scope": {
                    "allowed_paths": [],
                    "forbidden_paths": [".git", ".env", ".coder_history", ".coder"],
                },
                "loop_policy": {
                    "max_auto_rounds": 3,
                    "user_can_override": True,
                },
                "risk_policy": {
                    "planner_is_risk_judge": True,
                    "high_risk_requires_human": True,
                    "low_risk_auto_continue": True,
                },
                "execution_policy": {
                    "executor_can_modify_files": True,
                    "executor_cannot_ask_human": True,
                    "executor_must_follow_planner_order": True,
                },
                "test_policy": {
                    "default_mode": "model_review_and_optional_command",
                    "tester_cannot_ask_human": True,
                },
                "human_agreements": [],
            }
        if agent.artifact_type == "planner_order":
            return {
                "artifact_type": "planner_order",
                "round": _round_from_context(context),
                "round_goal": f"Handle the smallest useful step for: {request}",
                "instructions_for_executor": [
                    "Use the current RunContract and stay inside scope.",
                    "Return an ExecutionResult with facts only.",
                ],
                "allowed_actions": ["inspect_context", "prepare_changes_when_authorized"],
                "forbidden_actions": ["ask_human_directly", "change_global_goal"],
                "target_files_or_outputs": [],
                "expected_outputs": ["execution_result"],
                "risk_level": "low",
                "requires_human_confirmation": False,
                "tester_instructions": [
                    "Review the ExecutionResult and return evidence without deciding the next step."
                ],
                "stop_and_return_to_planner_when": [
                    "The requested work exceeds the RunContract.",
                    "A human decision is required before continuing.",
                ],
            }
        if agent.artifact_type == "execution_result":
            return {
                "artifact_type": "execution_result",
                "round": _round_from_context(context),
                "status": "completed",
                "summary": f"Mock executor completed a dry run for: {request}",
                "changed_files": [],
                "created_files": [],
                "deleted_files": [],
                "patch_refs": [],
                "outputs": sorted(summaries.keys()),
                "unexpected_issues": [],
                "out_of_contract": False,
                "needs_planner_decision": False,
                "tester_notes": ["No real file mutation was performed in mock mode."],
            }
        if agent.artifact_type == "test_result":
            return {
                "artifact_type": "test_result",
                "round": _round_from_context(context),
                "status": "pass",
                "summary": "Mock tester found no blocking issue.",
                "evidence": sorted(summaries.keys()),
                "issues": [],
                "remaining_work": [],
                "confidence": "medium",
                "check_commands": [],
                "check_outputs_ref": None,
            }
        if agent.artifact_type == "planner_decision":
            return {
                "artifact_type": "planner_decision",
                "round": _round_from_context(context),
                "task_done": True,
                "next_action": "finish",
                "risk_level": "low",
                "requires_human_confirmation": False,
                "reason": "Mock execution and test artifacts are complete.",
                "next_round_goal": "",
                "remaining_auto_rounds": 2,
                "human_message": None,
            }
        if agent.artifact_type == "round_summary":
            return {
                "artifact_type": "round_summary",
                "round": _round_from_context(context),
                "planner_order_summary": "Planner issued a scoped low-risk order.",
                "execution_summary": "Executor returned structured execution facts.",
                "test_summary": "Tester returned structured evidence.",
                "planner_decision_summary": "Planner decided to finish.",
                "important_refs": sorted(summaries.keys()),
                "carry_forward_constraints": [
                    "Only Planner can talk to the human.",
                    "Executor and Tester return facts, not decisions.",
                ],
                "remaining_work": [],
            }
        if agent.artifact_type == "plan_artifact":
            return {
                "artifact_type": "plan_artifact",
                "summary": f"Plan a safe local coding task for: {request}",
                "target_files": [],
                "required_context": sorted(summaries.keys()),
                "implementation_steps": [
                    "Inspect the selected project summary.",
                    "Keep the implementation scope narrow.",
                    "Generate a patch artifact for runtime review.",
                ],
                "risks": [],
                "recommended_checks": [],
                "executor_instructions": "Prepare a patch artifact only; do not write files directly.",
            }
        if agent.artifact_type == "patch_artifact":
            return {
                "artifact_type": "patch_artifact",
                "implementation_summary": f"Mock executor found no file changes required for: {request}",
                "changed_files": [],
                "patches": [],
                "risks": [],
                "suggested_check_command": "",
            }
        if agent.artifact_type == "review_artifact":
            return {
                "artifact_type": "review_artifact",
                "status": "pass",
                "evidence": sorted(summaries.keys()),
                "issues": [],
                "risk_level": "low",
                "recommended_action": "Finish the workflow.",
            }
        return {
            "status": "completed",
            "agent_id": agent.id,
            "summary": f"{agent.role} completed a dry-run response for: {request}",
            "used_summaries": sorted(summaries.keys()),
            "needs_human": agent.permissions.requires_approval and agent.permissions.edit_files,
            "recommendation": "Continue to the next workflow node if routing conditions allow.",
        }


def _try_parse_json(value: str) -> Any:
    cleaned = value.strip()
    if cleaned.startswith("```"):
        cleaned = cleaned.strip("`")
        if cleaned.startswith("json"):
            cleaned = cleaned[4:].strip()
    try:
        return json.loads(cleaned)
    except json.JSONDecodeError:
        return None


def _round_from_context(context: dict[str, Any]) -> int:
    loop = context.get("loop")
    if isinstance(loop, dict):
        try:
            return max(1, int(loop.get("iteration", 1)))
        except (TypeError, ValueError):
            return 1
    return 1
