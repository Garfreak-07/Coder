from __future__ import annotations

import json
from typing import Any, Protocol

from coder_workbench.core import AgentSpec
from coder_workbench.config import load_runtime_config
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

    def run(self, agent: AgentSpec, context: dict[str, Any]) -> dict[str, Any]:
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
