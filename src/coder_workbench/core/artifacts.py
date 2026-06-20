from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field, ValidationError


ArtifactType = Literal[
    "run_contract",
    "planner_order",
    "execution_result",
    "test_result",
    "planner_decision",
    "round_summary",
    "plan_artifact",
    "patch_artifact",
    "review_artifact",
]
ReviewStatus = Literal["pass", "needs_changes", "failed", "blocked"]
RiskLevel = Literal["low", "medium", "high"]
ExecutionStatus = Literal["completed", "blocked", "failed"]
TestStatus = Literal["pass", "fail", "blocked"]
PlannerNextAction = Literal["continue", "ask_human", "finish", "stop"]
ConfidenceLevel = Literal["low", "medium", "high"]


class ArtifactValidationError(ValueError):
    def __init__(self, artifact_type: str, errors: list[dict[str, Any]]) -> None:
        self.artifact_type = artifact_type
        self.errors = errors
        super().__init__(f"{artifact_type} failed schema validation")


class _ArtifactBase(BaseModel):
    model_config = ConfigDict(extra="forbid")

    artifact_id: str | None = None
    artifact_type: ArtifactType


class ScopeContract(BaseModel):
    model_config = ConfigDict(extra="forbid")

    allowed_paths: list[str] = Field(default_factory=list)
    forbidden_paths: list[str] = Field(default_factory=list)


class LoopPolicy(BaseModel):
    model_config = ConfigDict(extra="forbid")

    max_auto_rounds: int = Field(default=3, ge=0, le=20)
    user_can_override: bool = True


class RiskPolicy(BaseModel):
    model_config = ConfigDict(extra="forbid")

    planner_is_risk_judge: bool = True
    high_risk_requires_human: bool = True
    low_risk_auto_continue: bool = True


class ExecutionPolicy(BaseModel):
    model_config = ConfigDict(extra="forbid")

    executor_can_modify_files: bool = True
    executor_cannot_ask_human: bool = True
    executor_must_follow_planner_order: bool = True


class TestPolicy(BaseModel):
    model_config = ConfigDict(extra="forbid")

    default_mode: str = "model_review_and_optional_command"
    tester_cannot_ask_human: bool = True


class RunContractArtifact(_ArtifactBase):
    artifact_type: Literal["run_contract"] = "run_contract"
    user_goal: str
    done_criteria: list[str] = Field(default_factory=list)
    scope: ScopeContract = Field(default_factory=ScopeContract)
    loop_policy: LoopPolicy = Field(default_factory=LoopPolicy)
    risk_policy: RiskPolicy = Field(default_factory=RiskPolicy)
    execution_policy: ExecutionPolicy = Field(default_factory=ExecutionPolicy)
    test_policy: TestPolicy = Field(default_factory=TestPolicy)
    human_agreements: list[str] = Field(default_factory=list)


class PlannerOrderArtifact(_ArtifactBase):
    artifact_type: Literal["planner_order"] = "planner_order"
    round: int = Field(default=1, ge=1)
    round_goal: str
    instructions_for_executor: list[str] = Field(default_factory=list)
    allowed_actions: list[str] = Field(default_factory=list)
    forbidden_actions: list[str] = Field(default_factory=list)
    target_files_or_outputs: list[str] = Field(default_factory=list)
    expected_outputs: list[str] = Field(default_factory=list)
    risk_level: RiskLevel = "low"
    requires_human_confirmation: bool = False
    tester_instructions: list[str] = Field(default_factory=list)
    stop_and_return_to_planner_when: list[str] = Field(default_factory=list)


class ExecutionResultArtifact(_ArtifactBase):
    artifact_type: Literal["execution_result"] = "execution_result"
    round: int = Field(default=1, ge=1)
    status: ExecutionStatus
    summary: str
    changed_files: list[str] = Field(default_factory=list)
    created_files: list[str] = Field(default_factory=list)
    deleted_files: list[str] = Field(default_factory=list)
    patch_refs: list[str] = Field(default_factory=list)
    outputs: list[str] = Field(default_factory=list)
    unexpected_issues: list[str] = Field(default_factory=list)
    out_of_contract: bool = False
    needs_planner_decision: bool = False
    tester_notes: list[str] = Field(default_factory=list)


class TestIssue(BaseModel):
    model_config = ConfigDict(extra="forbid")

    title: str
    severity: RiskLevel = "low"
    evidence_ref: str | None = None


class TestResultArtifact(_ArtifactBase):
    artifact_type: Literal["test_result"] = "test_result"
    round: int = Field(default=1, ge=1)
    status: TestStatus
    summary: str
    evidence: list[str] = Field(default_factory=list)
    issues: list[TestIssue] = Field(default_factory=list)
    remaining_work: list[str] = Field(default_factory=list)
    confidence: ConfidenceLevel = "medium"
    check_commands: list[str] = Field(default_factory=list)
    check_outputs_ref: str | None = None


class PlannerDecisionArtifact(_ArtifactBase):
    artifact_type: Literal["planner_decision"] = "planner_decision"
    round: int = Field(default=1, ge=1)
    task_done: bool
    next_action: PlannerNextAction
    risk_level: RiskLevel = "low"
    requires_human_confirmation: bool = False
    reason: str
    next_round_goal: str = ""
    remaining_auto_rounds: int = Field(default=0, ge=0, le=20)
    human_message: str | None = None


class RoundSummaryArtifact(_ArtifactBase):
    artifact_type: Literal["round_summary"] = "round_summary"
    round: int = Field(default=1, ge=1)
    planner_order_summary: str
    execution_summary: str
    test_summary: str
    planner_decision_summary: str
    important_refs: list[str] = Field(default_factory=list)
    carry_forward_constraints: list[str] = Field(default_factory=list)
    remaining_work: list[str] = Field(default_factory=list)


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
    "run_contract": RunContractArtifact,
    "planner_order": PlannerOrderArtifact,
    "execution_result": ExecutionResultArtifact,
    "test_result": TestResultArtifact,
    "planner_decision": PlannerDecisionArtifact,
    "round_summary": RoundSummaryArtifact,
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
    if artifact_type == "run_contract":
        scope = artifact.get("scope") or {}
        loop_policy = artifact.get("loop_policy") or {}
        summary.update(
            {
                "user_goal": artifact.get("user_goal"),
                "done_criteria": len(artifact.get("done_criteria", [])),
                "allowed_paths": scope.get("allowed_paths", []),
                "forbidden_paths": scope.get("forbidden_paths", []),
                "max_auto_rounds": loop_policy.get("max_auto_rounds"),
            }
        )
    elif artifact_type == "planner_order":
        summary.update(
            {
                "round": artifact.get("round"),
                "round_goal": artifact.get("round_goal"),
                "risk_level": artifact.get("risk_level"),
                "requires_human_confirmation": artifact.get("requires_human_confirmation"),
                "instructions": len(artifact.get("instructions_for_executor", [])),
                "expected_outputs": artifact.get("expected_outputs", []),
            }
        )
    elif artifact_type == "execution_result":
        summary.update(
            {
                "round": artifact.get("round"),
                "status": artifact.get("status"),
                "summary": artifact.get("summary"),
                "changed_files": artifact.get("changed_files", []),
                "unexpected_issues": len(artifact.get("unexpected_issues", [])),
                "needs_planner_decision": artifact.get("needs_planner_decision"),
            }
        )
    elif artifact_type == "test_result":
        summary.update(
            {
                "round": artifact.get("round"),
                "status": artifact.get("status"),
                "summary": artifact.get("summary"),
                "issues": len(artifact.get("issues", [])),
                "remaining_work": artifact.get("remaining_work", []),
                "confidence": artifact.get("confidence"),
            }
        )
    elif artifact_type == "planner_decision":
        summary.update(
            {
                "round": artifact.get("round"),
                "task_done": artifact.get("task_done"),
                "next_action": artifact.get("next_action"),
                "risk_level": artifact.get("risk_level"),
                "remaining_auto_rounds": artifact.get("remaining_auto_rounds"),
                "reason": artifact.get("reason"),
            }
        )
    elif artifact_type == "round_summary":
        summary.update(
            {
                "round": artifact.get("round"),
                "planner_order_summary": artifact.get("planner_order_summary"),
                "execution_summary": artifact.get("execution_summary"),
                "test_summary": artifact.get("test_summary"),
                "decision_summary": artifact.get("planner_decision_summary"),
                "remaining_work": artifact.get("remaining_work", []),
            }
        )
    elif artifact_type == "plan_artifact":
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
