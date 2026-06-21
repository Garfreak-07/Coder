from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field


class HarnessObservation(BaseModel):
    model_config = ConfigDict(extra="forbid")

    action_type: str
    summary: str
    refs: list[str] = Field(default_factory=list)
