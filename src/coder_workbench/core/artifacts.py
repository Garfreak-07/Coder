from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field, ValidationError


ArtifactType = Literal["plan_artifact", "patch_artifact", "review_artifact"]
ReviewStatus = Literal["pass", "needs_changes", "failed", "blocked"]
RiskLevel = Literal["low", "medium", "high"]


class ArtifactValidationError(ValueError):
    def __init__(self, artifact_type: str, errors: list[dict[str, Any]]) -> None:
        self.artifact_type = artifact_type
        self.errors = errors
        super().__init__(f"{artifact_type} failed schema validation")


class _ArtifactBase(BaseModel):
    model_config = ConfigDict(extra="forbid")

    artifact_id: str | None = None
    artifact_type: ArtifactType


class PlanArtifact(_ArtifactBase):
    artifact_type: Literal["plan_artifact"] = "plan_artifact"
    summary: str
    target_files: list[str] = Field(default_factory=list)
    required_context: list[str] = Field(default_factory=list)
    implementation_steps: list[str] = Field(default_factory=list)
    risks: list[str] = Field(default_factory=list)
    recommended_checks: list[str] = Field(default_factory=list)
    executor_instructions: str = ""


class PatchArtifact(_ArtifactBase):
    artifact_type: Literal["patch_artifact"] = "patch_artifact"
    implementation_summary: str
    changed_files: list[str] = Field(default_factory=list)
    patches: list[dict[str, Any]] = Field(default_factory=list)
    risks: list[str] = Field(default_factory=list)
    suggested_check_command: str = ""


class ReviewArtifact(_ArtifactBase):
    artifact_type: Literal["review_artifact"] = "review_artifact"
    status: ReviewStatus
    evidence: list[str] = Field(default_factory=list)
    issues: list[str] = Field(default_factory=list)
    risk_level: RiskLevel = "low"
    recommended_action: str = ""


ARTIFACT_MODELS: dict[str, type[_ArtifactBase]] = {
    "plan_artifact": PlanArtifact,
    "patch_artifact": PatchArtifact,
    "review_artifact": ReviewArtifact,
}


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
    if artifact_type == "plan_artifact":
        summary.update(
            {
                "summary": artifact.get("summary"),
                "target_files": artifact.get("target_files", []),
                "steps": len(artifact.get("implementation_steps", [])),
                "risks": len(artifact.get("risks", [])),
                "checks": artifact.get("recommended_checks", []),
            }
        )
    elif artifact_type == "patch_artifact":
        summary.update(
            {
                "summary": artifact.get("implementation_summary"),
                "changed_files": artifact.get("changed_files", []),
                "patches": len(artifact.get("patches", [])),
                "risks": len(artifact.get("risks", [])),
                "suggested_check_command": artifact.get("suggested_check_command"),
            }
        )
    elif artifact_type == "review_artifact":
        summary.update(
            {
                "status": artifact.get("status"),
                "risk_level": artifact.get("risk_level"),
                "issues": len(artifact.get("issues", [])),
                "recommended_action": artifact.get("recommended_action"),
            }
        )
    return {key: value for key, value in summary.items() if value not in (None, "", [])}
