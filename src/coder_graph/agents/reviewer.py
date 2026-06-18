from __future__ import annotations

from typing import Literal, TypedDict

from langchain_core.language_models.chat_models import BaseChatModel

from .json_utils import coerce_list, parse_json_object


class ReviewerResult(TypedDict):
    approved: bool
    risk_level: Literal["low", "medium", "high"]
    scope_escape: bool
    stop_reasons: list[str]
    notes: str


def run_reviewer_agent(prompt: str, llm: BaseChatModel) -> ReviewerResult:
    response = llm.invoke(prompt)
    data = parse_json_object(str(response.content))
    risk_level = str(data.get("risk_level", "medium")).lower()
    if risk_level not in {"low", "medium", "high"}:
        risk_level = "medium"

    stop_reasons = coerce_list(data.get("stop_reasons"), limit=8)
    scope_escape = bool(data.get("scope_escape", False))
    approved = bool(data.get("approved", False)) and not scope_escape and risk_level != "high"

    return {
        "approved": approved,
        "risk_level": risk_level,  # type: ignore[typeddict-item]
        "scope_escape": scope_escape,
        "stop_reasons": stop_reasons,
        "notes": str(data.get("notes", ""))[:500],
    }


def reviewer_prompt(context: str) -> str:
    return f"""You are Reviewer Agent.
Return one compact JSON object. No markdown. No extra text.

Schema:
{{
  "approved": true,
  "risk_level": "low",
  "scope_escape": false,
  "stop_reasons": [],
  "notes": "one sentence"
}}

Rules:
- Reject scope escape.
- Reject high-risk changes unless human approval is required first.
- Prefer deterministic evidence from paths, selected modules, checks, and plan.
- Be terse.

Context:
{context}
"""
