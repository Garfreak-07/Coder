from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field


class RunGuard(BaseModel):
    """Hard limits enforced by RunController before another Planner round."""

    model_config = ConfigDict(extra="forbid")

    max_rounds: int = Field(default=5, ge=1)
    max_agent_calls: int = Field(default=40, ge=0)
    max_tool_calls: int = Field(default=80, ge=0)
    max_wall_seconds: int = Field(default=1800, ge=1)
    max_total_estimated_tokens: int = Field(default=200_000, ge=0)
    max_same_plan_repeats: int = Field(default=2, ge=0)
    max_same_error_repeats: int = Field(default=2, ge=0)
