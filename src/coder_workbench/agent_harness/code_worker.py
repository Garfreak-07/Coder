from __future__ import annotations

from typing import Any

from pydantic import ValidationError

from coder_workbench.agent_graph.artifacts import graph_artifact_id
from coder_workbench.agent_graph.repair import build_repair_prompt, parse_json_object
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, ExecutionRecord, WorkItem
from coder_workbench.core.artifacts import ArtifactValidationError, validate_artifact

from .base import AgentHarness
from .policies import code_worker_policy


class CodeWorkerHarness(AgentHarness):
    def __init__(self, *, model: Any | None = None) -> None:
        super().__init__(policy=code_worker_policy())
        self.model = model

    def create_execution_result(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        coding_context_packet: dict[str, Any] | None = None,
    ) -> ExecutionRecord:
        payload = self._payload_from_model_or_mock(item=item, envelope=envelope, coding_context_packet=coding_context_packet)
        payload = _with_forced_fields(
            payload,
            {
                "artifact_type": "execution_result",
                "round": envelope.round,
                "work_item_id": item.work_item_id,
                "merge_index": item.merge_index,
                "agent_id": item.assignee_agent_id,
            },
        )
        try:
            artifact = validate_artifact(payload, expected_type="execution_result")
        except ArtifactValidationError:
            artifact = _blocked_payload(item, envelope.round, "Worker output failed schema validation after one repair.")
        return ExecutionRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status=artifact["status"],
            execution_summary=artifact["summary"],
            execution_result_ref=graph_artifact_id("execution_result", item.work_item_id),
            artifact_payload=artifact,
        )

    def _payload_from_model_or_mock(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        coding_context_packet: dict[str, Any] | None,
    ) -> dict[str, Any]:
        if not self.model:
            if coding_context_packet is not None and not coding_context_packet.get("included_files") and item.task_summary:
                return _blocked_payload(item, envelope.round, "Coding context was insufficient for this work item.")
            return {
                "artifact_type": "execution_result",
                "round": envelope.round,
                "status": "completed",
                "summary": "CodeWorkerHarness mock completed a dry-run execution.",
                "proposed_changes": [],
                "changed_files": [],
                "created_files": [],
                "deleted_files": [],
                "patch_refs": [],
                "outputs": envelope.upstream_refs,
                "unexpected_issues": [],
                "out_of_contract": False,
                "needs_planner_decision": False,
                "tester_notes": ["No real file mutation was performed in mock mode."],
            }
        response = self.model.invoke(_worker_prompt(item, envelope, coding_context_packet))
        content = str(getattr(response, "content", response))
        payload = parse_json_object(content)
        if payload is not None:
            try:
                validate_artifact(payload, expected_type="execution_result")
                return payload
            except ArtifactValidationError:
                pass
        repaired = self._repair_once(content)
        if repaired is not None:
            return repaired
        return _blocked_payload(item, envelope.round, "Worker output failed schema validation after one repair.")

    def _repair_once(self, invalid_output: str) -> dict[str, Any] | None:
        if not self.model:
            return None
        prompt = build_repair_prompt(
            expected_type="execution_result",
            invalid_output=invalid_output,
            errors=[{"loc": ["response"], "msg": "schema validation failed"}],
            schema_notes="Return a valid execution_result JSON object.",
        )
        response = self.model.invoke(prompt)
        payload = parse_json_object(str(getattr(response, "content", response)))
        if payload is None:
            return None
        try:
            validate_artifact(payload, expected_type="execution_result")
        except (ArtifactValidationError, ValidationError):
            return None
        return payload


def _worker_prompt(item: WorkItem, envelope: AgentTaskEnvelope, coding_context_packet: dict[str, Any] | None) -> str:
    return "\n\n".join(
        [
            "Return JSON only with artifact_type='execution_result'.",
            "Do not ask the human. Use proposed_changes for file edits.",
            f"Work item: {item.model_dump(mode='json')}",
            f"Agent task envelope: {envelope.model_dump(mode='json')}",
            f"Coding context packet: {coding_context_packet or {}}",
        ]
    )


def _blocked_payload(item: WorkItem, round_number: int, summary: str) -> dict[str, Any]:
    return {
        "artifact_type": "execution_result",
        "round": round_number,
        "work_item_id": item.work_item_id,
        "merge_index": item.merge_index,
        "agent_id": item.assignee_agent_id,
        "status": "blocked",
        "summary": summary,
        "unexpected_issues": ["context_or_schema_blocker"],
        "needs_planner_decision": True,
        "blocker_type": "context_missing" if "context" in summary.lower() else "schema_validation_failed",
        "planner_question": "Should Planner provide more context, retry, or replan this work item?",
        "candidate_options": [],
        "continue_without_human_possible": True,
    }


def _with_forced_fields(payload: dict[str, Any], forced: dict[str, Any]) -> dict[str, Any]:
    merged = dict(payload)
    merged.update(forced)
    return merged
