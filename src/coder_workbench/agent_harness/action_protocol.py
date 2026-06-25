from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field


class HarnessActionRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    artifact_type: Literal["harness_action"] = "harness_action"
    action_id: str
    action_type: str
    payload: dict[str, Any] = Field(default_factory=dict)
    reason: str = ""
    risk_level: Literal["low", "medium", "high"] = "low"
    expected_evidence: list[str] = Field(default_factory=list)


class HarnessActionBatch(BaseModel):
    model_config = ConfigDict(extra="forbid")

    artifact_type: Literal["harness_action_batch"] = "harness_action_batch"
    actions: list[HarnessActionRequest] = Field(default_factory=list, min_length=1)


class HarnessObservation(BaseModel):
    model_config = ConfigDict(extra="forbid")

    artifact_type: Literal["harness_observation"] = "harness_observation"
    action_id: str
    action_type: str
    status: Literal["ok", "blocked", "failed"]
    summary: str
    output_ref: str | None = None
    evidence_refs: list[str] = Field(default_factory=list)
    payload_preview: dict[str, Any] = Field(default_factory=dict)
    error_code: str | None = None
