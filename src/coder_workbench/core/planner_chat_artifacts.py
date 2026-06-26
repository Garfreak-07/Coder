from __future__ import annotations

from typing import Literal

import json
from typing import Any

from pydantic import BaseModel, ConfigDict, Field, model_validator


PlannerInteractionMode = Literal["discuss", "work"]
PlannerReadiness = Literal["not_ready", "needs_clarification", "ready_to_plan", "ready_to_execute"]
PlannerChatDecision = Literal[
    "continue_chat",
    "produce_plan",
    "answer_without_workflow",
    "start_workflow",
    "blocked_needs_clarification",
]


class PlannerPlanStep(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str = Field(min_length=1)
    summary: str = Field(min_length=1)
    depends_on: list[str] = Field(default_factory=list)
    status: Literal["draft", "ready", "executing", "done", "blocked"] = "draft"


class PlannerTaskState(BaseModel):
    model_config = ConfigDict(extra="forbid")

    goal: str | None = None
    user_intent: str | None = None
    scope: list[str] = Field(default_factory=list)
    constraints: list[str] = Field(default_factory=list)
    success_criteria: list[str] = Field(default_factory=list)
    known_context: list[str] = Field(default_factory=list)
    missing_context: list[str] = Field(default_factory=list)
    open_questions: list[str] = Field(default_factory=list)
    assumptions: list[str] = Field(default_factory=list)
    risks: list[str] = Field(default_factory=list)
    plan_steps: list[PlannerPlanStep] = Field(default_factory=list)
    readiness: PlannerReadiness = "not_ready"


class PlannerVisibleThinking(BaseModel):
    model_config = ConfigDict(extra="forbid")

    phase: Literal[
        "understanding",
        "gathering_context",
        "clarifying",
        "planning",
        "checking_readiness",
        "ready_to_start",
        "reporting",
    ]
    summary: str = Field(min_length=1, max_length=400)


class PlannerWorkflowHandoff(BaseModel):
    model_config = ConfigDict(extra="forbid")

    workflow_request: str = Field(min_length=1)
    scope: list[str] = Field(default_factory=list)
    success_criteria: list[str] = Field(default_factory=list)
    risks: list[str] = Field(default_factory=list)


class PlannerChatTurn(BaseModel):
    model_config = ConfigDict(extra="forbid")

    artifact_id: str | None = None
    artifact_type: Literal["planner_chat_turn"] = "planner_chat_turn"
    assistant_message: str = Field(min_length=1)
    interaction_mode: PlannerInteractionMode
    decision: PlannerChatDecision
    visible_thinking: PlannerVisibleThinking
    task_state: PlannerTaskState
    handoff: PlannerWorkflowHandoff | None = None

    @model_validator(mode="after")
    def validate_mode_semantics(self) -> "PlannerChatTurn":
        return validate_planner_chat_turn_for_mode(self)


class WorkflowActivityStep(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str = Field(min_length=1)
    label: str = Field(min_length=1)
    status: Literal["done", "active", "pending", "blocked", "failed"]


class WorkflowActivityUpdate(BaseModel):
    model_config = ConfigDict(extra="forbid")

    artifact_id: str | None = None
    artifact_type: Literal["workflow_activity_update"] = "workflow_activity_update"
    visible_phase: Literal[
        "planning",
        "assigning_work",
        "executing",
        "checking",
        "summarizing",
        "completed",
        "blocked",
        "failed",
    ]
    user_message: str = Field(min_length=1, max_length=800)
    steps: list[WorkflowActivityStep]
    safety: list[dict[str, str]] = Field(default_factory=list)
    technical_refs: dict[str, Any] = Field(default_factory=dict)

    @model_validator(mode="after")
    def reject_full_technical_payloads(self) -> "WorkflowActivityUpdate":
        forbidden_keys = {
            "full_log",
            "full_logs",
            "raw_log",
            "raw_logs",
            "full_diff",
            "raw_diff",
            "full_prompt",
            "raw_prompt",
            "model_output",
            "raw_runtime_json",
        }
        lowered_keys = {str(key).lower() for key in self.technical_refs}
        blocked = sorted(lowered_keys.intersection(forbidden_keys))
        if blocked:
            raise ValueError(f"technical_refs must not include full technical payload keys: {blocked}")
        text = json.dumps(self.technical_refs, sort_keys=True, default=str)
        if len(text) > 4000:
            raise ValueError("technical_refs must stay compact and reference-oriented")
        return self


def validate_planner_chat_turn_for_mode(turn: PlannerChatTurn) -> PlannerChatTurn:
    """Enforce Coder's user-facing Discuss/Work semantics for Planner chat."""

    if turn.interaction_mode == "discuss" and turn.decision == "start_workflow":
        raise ValueError("Discuss mode must never return decision=start_workflow")

    if turn.task_state.readiness == "needs_clarification" and not turn.task_state.open_questions:
        raise ValueError("readiness=needs_clarification requires open_questions")

    if turn.decision == "start_workflow":
        if turn.interaction_mode != "work":
            raise ValueError("start_workflow is only valid in Work mode")
        if turn.task_state.readiness != "ready_to_execute":
            raise ValueError("start_workflow requires task_state.readiness=ready_to_execute")
        if not _present(turn.task_state.goal):
            raise ValueError("start_workflow requires task_state.goal")
        if not turn.task_state.success_criteria:
            raise ValueError("start_workflow requires success_criteria")
        if turn.task_state.open_questions:
            raise ValueError("start_workflow requires open_questions to be empty")
        if turn.handoff is None:
            raise ValueError("start_workflow requires handoff")

    if not turn.visible_thinking.summary.strip():
        raise ValueError("visible_thinking.summary is required")
    return turn


PLANNER_CHAT_ARTIFACT_MODELS: dict[str, type[BaseModel]] = {
    "planner_chat_turn": PlannerChatTurn,
    "workflow_activity_update": WorkflowActivityUpdate,
}


def planner_chat_artifact_summary(artifact: dict) -> dict:
    if artifact.get("artifact_type") == "planner_chat_turn":
        task_state = artifact.get("task_state") if isinstance(artifact.get("task_state"), dict) else {}
        return {
            "interaction_mode": artifact.get("interaction_mode"),
            "decision": artifact.get("decision"),
            "readiness": task_state.get("readiness"),
            "goal": task_state.get("goal"),
            "open_questions": len(task_state.get("open_questions", [])),
            "has_handoff": bool(artifact.get("handoff")),
        }
    if artifact.get("artifact_type") == "workflow_activity_update":
        return {
            "visible_phase": artifact.get("visible_phase"),
            "user_message": artifact.get("user_message"),
            "steps": len(artifact.get("steps", [])),
            "technical_refs": len(artifact.get("technical_refs", {})),
        }
    return {}


def _present(value: str | None) -> bool:
    return bool(str(value or "").strip())


__all__ = [
    "PLANNER_CHAT_ARTIFACT_MODELS",
    "PlannerChatDecision",
    "PlannerChatTurn",
    "PlannerInteractionMode",
    "PlannerPlanStep",
    "PlannerReadiness",
    "PlannerTaskState",
    "PlannerVisibleThinking",
    "PlannerWorkflowHandoff",
    "WorkflowActivityStep",
    "WorkflowActivityUpdate",
    "planner_chat_artifact_summary",
    "validate_planner_chat_turn_for_mode",
]
