from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel
from pydantic import ValidationError

from coder_workbench.core.planner_chat_artifacts import (
    PLANNER_CHAT_ARTIFACT_MODELS,
    planner_chat_artifact_summary,
)
from coder_workbench.core.planner_artifacts import (
    PLANNER_ARTIFACT_MODELS,
    PlannerArtifactType,
    planner_artifact_summary,
)
from coder_workbench.memory.planner_file_memory import PlannerMemoryWriteProposal


ArtifactType = Literal[
    "project_plan_draft",
    "run_contract_draft",
    "run_contract",
    "planner_order",
    "execution_result",
    "planner_decision",
    "planner_chat_turn",
    "workflow_activity_update",
    "round_summary",
    "final_report",
    "planner_memory_write_proposal",
]

ARTIFACT_MODELS: dict[str, type[BaseModel]] = {
    **PLANNER_ARTIFACT_MODELS,
    **PLANNER_CHAT_ARTIFACT_MODELS,
    "planner_memory_write_proposal": PlannerMemoryWriteProposal,
}


class ArtifactValidationError(ValueError):
    def __init__(self, artifact_type: str, errors: list[dict[str, Any]]) -> None:
        self.artifact_type = artifact_type
        self.errors = errors
        super().__init__(f"{artifact_type} failed schema validation")


def supported_artifact_types() -> list[str]:
    return sorted(ARTIFACT_MODELS)


def validate_artifact(
    value: dict[str, Any],
    *,
    expected_type: str | None = None,
    artifact_id: str | None = None,
) -> dict[str, Any]:
    """Validate and normalize a supported workflow artifact."""

    artifact_type = expected_type or str(value.get("artifact_type") or "")
    model = ARTIFACT_MODELS.get(artifact_type)
    if model is None:
        raise ArtifactValidationError(
            artifact_type or "unknown_artifact",
            [{"loc": ["artifact_type"], "msg": f"unsupported artifact type: {artifact_type or 'missing'}"}],
        )

    payload = dict(value)
    payload["artifact_type"] = artifact_type
    if artifact_id is not None:
        payload["artifact_id"] = artifact_id
    try:
        return model.model_validate(payload).model_dump(mode="json")
    except ValidationError as exc:
        raise ArtifactValidationError(artifact_type, exc.errors()) from exc


def artifact_summary(artifact: dict[str, Any]) -> dict[str, Any]:
    artifact_type = str(artifact.get("artifact_type") or "")
    summary: dict[str, Any] = {
        "artifact_id": artifact.get("artifact_id"),
        "artifact_type": artifact_type,
    }
    if artifact_type in PLANNER_ARTIFACT_MODELS:
        summary.update(planner_artifact_summary(artifact))
    if artifact_type in PLANNER_CHAT_ARTIFACT_MODELS:
        summary.update(planner_chat_artifact_summary(artifact))
    return {key: value for key, value in summary.items() if value not in (None, "", [])}


__all__ = [
    "ArtifactType",
    "ArtifactValidationError",
    "PlannerArtifactType",
    "artifact_summary",
    "supported_artifact_types",
    "validate_artifact",
]
