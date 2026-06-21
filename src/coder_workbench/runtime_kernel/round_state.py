from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, ConfigDict, Field


RoundStatus = Literal[
    "planning",
    "dispatching",
    "merging",
    "decision",
    "completed",
    "blocked",
    "failed",
]


class RoundState(BaseModel):
    model_config = ConfigDict(extra="forbid")

    round: int = Field(ge=1)
    planner_order_ref: str | None = None
    planner_decision_ref: str | None = None
    planner_input_bundle_ref: str | None = None
    plan_fingerprint: str | None = None
    status: RoundStatus
