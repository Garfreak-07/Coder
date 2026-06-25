from __future__ import annotations

import json
from typing import Any

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.agent_graph.schema import AgentTaskEnvelope, WorkItem
from coder_workbench.agent_harness.session import CodeWorkerLoopState


class CodeWorkerContextBudget(BaseModel):
    model_config = ConfigDict(extra="forbid")

    max_recent_observations: int = 8
    max_observation_chars: int = 1200
    max_total_context_chars: int = 24000
    max_tool_preview_chars: int = 4000


class PreparedCodeWorkerContext(BaseModel):
    model_config = ConfigDict(extra="forbid")

    prompt_payload: dict[str, Any]
    compacted: bool = False
    externalized_refs: list[str] = Field(default_factory=list)
    omitted_counts: dict[str, int] = Field(default_factory=dict)


class CodeWorkerContextPreprocessor:
    def __init__(self, budget: CodeWorkerContextBudget | None = None) -> None:
        self.budget = budget or CodeWorkerContextBudget()

    def prepare(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        state: CodeWorkerLoopState,
    ) -> PreparedCodeWorkerContext:
        session = state.session.model_copy(deep=True)
        observations = [observation.model_dump(mode="json", exclude_none=True) for observation in session.observations]
        recent_raw = observations[-self.budget.max_recent_observations :]
        older = observations[: max(0, len(observations) - len(recent_raw))]
        compacted = False
        externalized_refs = _unique(session.evidence_refs + _observation_refs(observations))
        recent_observations: list[dict[str, Any]] = []
        for observation in recent_raw:
            bounded, was_compacted = _bounded_observation(observation, self.budget.max_observation_chars)
            recent_observations.append(bounded)
            compacted = compacted or was_compacted

        prompt_payload = {
            "hot_context": {
                "work_item": item.model_dump(mode="json"),
                "constraints": list(envelope.constraints),
                "turn_count": state.turn_count,
                "max_turns": state.max_turns,
                "transition": dict(state.transition or {}),
                "latest_failure": _bounded_text(session.blocked_reasons[-1] if session.blocked_reasons else "", 600),
                "recent_observations": recent_observations,
                "changed_files": list(session.changed_files),
                "created_files": list(session.created_files),
                "deleted_files": list(session.deleted_files),
                "patch_refs": list(session.patch_refs),
                "latest_command_check": _compact_mapping(session.command_checks[-1], max_chars=1200) if session.command_checks else None,
            },
            "warm_context": {
                "opened_files": list(session.opened_files),
                "searched_patterns": list(session.searched_patterns),
                "older_observation_summary": _summarize_observations(older),
                "recovery_attempts": _compact_records(session.recovery_attempts[-4:], max_chars=1200),
                "stop_gate_failures": _compact_records(session.stop_gate_failures[-4:], max_chars=1200),
                "coding_context_packet": _compact_mapping(session.coding_context_packet, max_chars=3000),
                "selected_skill_context": list(envelope.selected_skill_context[:4]),
            },
            "cold_context": {
                "evidence_refs": list(session.evidence_refs),
                "output_refs": externalized_refs,
                "omitted_observation_count": len(older),
            },
        }
        omitted_counts = {"observations": len(older)}
        prepared = PreparedCodeWorkerContext(
            prompt_payload=prompt_payload,
            compacted=compacted or bool(older),
            externalized_refs=externalized_refs,
            omitted_counts=omitted_counts,
        )
        return _fit_total_budget(prepared, self.budget)


def _bounded_observation(observation: dict[str, Any], max_chars: int) -> tuple[dict[str, Any], bool]:
    bounded = dict(observation)
    compacted = False
    summary = str(bounded.get("summary") or "")
    bounded_summary = _bounded_text(summary, min(max_chars, 600))
    if bounded_summary != summary:
        bounded["summary"] = bounded_summary
        compacted = True
    payload = bounded.get("payload_preview")
    if payload is None:
        return bounded, compacted
    text = json.dumps(payload, ensure_ascii=False, default=str)
    if len(text) <= max_chars:
        return bounded, compacted
    bounded["payload_preview"] = {
        "preview": text[:max_chars],
        "truncated": True,
        "original_chars": len(text),
    }
    return bounded, True


def _fit_total_budget(prepared: PreparedCodeWorkerContext, budget: CodeWorkerContextBudget) -> PreparedCodeWorkerContext:
    payload = prepared.prompt_payload
    if _json_chars(payload) <= budget.max_total_context_chars:
        return prepared
    compacted_payload = json.loads(json.dumps(payload, ensure_ascii=False, default=str))
    hot = compacted_payload.get("hot_context") if isinstance(compacted_payload.get("hot_context"), dict) else {}
    observations = hot.get("recent_observations") if isinstance(hot.get("recent_observations"), list) else []
    while observations and _json_chars(compacted_payload) > budget.max_total_context_chars:
        observations.pop(0)
    omitted = dict(prepared.omitted_counts)
    omitted["recent_observations_trimmed"] = budget.max_recent_observations - len(observations)
    return prepared.model_copy(
        update={
            "prompt_payload": compacted_payload,
            "compacted": True,
            "omitted_counts": omitted,
        }
    )


def _summarize_observations(observations: list[dict[str, Any]]) -> list[dict[str, Any]]:
    summary: list[dict[str, Any]] = []
    for observation in observations[-16:]:
        summary.append(
            {
                "action_id": observation.get("action_id"),
                "action_type": observation.get("action_type"),
                "status": observation.get("status"),
                "summary": str(observation.get("summary") or "")[:240],
                "output_ref": observation.get("output_ref"),
                "error_code": observation.get("error_code"),
            }
        )
    return summary


def _compact_mapping(value: dict[str, Any], *, max_chars: int) -> dict[str, Any]:
    text = json.dumps(value, ensure_ascii=False, default=str)
    if len(text) <= max_chars:
        return json.loads(text) if text else {}
    return {"preview": text[:max_chars], "truncated": True, "original_chars": len(text)}


def _compact_records(records: list[dict[str, Any]], *, max_chars: int) -> list[dict[str, Any]]:
    return [_compact_mapping(record, max_chars=max_chars) for record in records if isinstance(record, dict)]


def _bounded_text(value: Any, max_chars: int) -> str:
    text = str(value or "")
    if len(text) <= max_chars:
        return text
    return f"{text[:max_chars]}... [truncated, original_chars={len(text)}]"


def _observation_refs(observations: list[dict[str, Any]]) -> list[str]:
    refs: list[str] = []
    for observation in observations:
        output_ref = observation.get("output_ref")
        if output_ref:
            refs.append(str(output_ref))
        evidence = observation.get("evidence_refs")
        if isinstance(evidence, list):
            refs.extend(str(item) for item in evidence if str(item).strip())
    return _unique(refs)


def _json_chars(value: Any) -> int:
    return len(json.dumps(value, ensure_ascii=False, default=str))


def _unique(values: list[str]) -> list[str]:
    seen: set[str] = set()
    output: list[str] = []
    for value in values:
        text = str(value or "").strip()
        if not text or text in seen:
            continue
        seen.add(text)
        output.append(text)
    return output
