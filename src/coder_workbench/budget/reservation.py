from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field


class BudgetLimit(BaseModel):
    model_config = ConfigDict(extra="forbid")

    max_estimated_tokens: int = Field(default=200_000, ge=0)
    max_model_calls: int = Field(default=40, ge=0)
    max_tool_calls: int = Field(default=80, ge=0)
    max_context_tokens_per_call: int = Field(default=18_000, ge=0)


class BudgetReservation(BaseModel):
    model_config = ConfigDict(extra="forbid")

    reservation_id: str
    run_id: str
    agent_id: str | None = None
    action_type: str
    estimated_tokens: int = Field(default=0, ge=0)
    estimated_tool_calls: int = Field(default=0, ge=0)
    estimated_model_calls: int = Field(default=0, ge=0)
    approved: bool
    reason: str = ""
    committed: bool = False
    released: bool = False
    actual_tokens: int = Field(default=0, ge=0)
    actual_tool_calls: int = Field(default=0, ge=0)
