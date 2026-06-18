from __future__ import annotations

from typing import TypedDict

from langchain_core.language_models.chat_models import BaseChatModel

from .json_utils import coerce_list, parse_json_object


class PlannerResult(TypedDict):
    summary: str
    target_files: list[str]
    steps: list[str]
    risks: list[str]
    checks: list[str]
    needs_human: bool


def run_planner_agent(prompt: str, llm: BaseChatModel) -> PlannerResult:
    response = llm.invoke(prompt)
    data = parse_json_object(str(response.content))

    return {
        "summary": str(data.get("summary", ""))[:500],
        "target_files": coerce_list(data.get("target_files"), limit=20),
        "steps": coerce_list(data.get("steps"), limit=5),
        "risks": coerce_list(data.get("risks"), limit=8),
        "checks": coerce_list(data.get("checks"), limit=5),
        "needs_human": bool(data.get("needs_human", False)),
    }


def planner_prompt(context: str) -> str:
    return f"""You are Planner Agent.
Return one compact JSON object. No markdown. No extra text.

Schema:
{{
  "summary": "one sentence",
  "target_files": ["path"],
  "steps": ["step"],
  "risks": ["risk"],
  "checks": ["command"],
  "needs_human": false
}}

Rules:
- Prefer selected scope over broad edits.
- Max 5 steps.
- Mention only real risks.
- Do not write code.

Context:
{context}
"""

