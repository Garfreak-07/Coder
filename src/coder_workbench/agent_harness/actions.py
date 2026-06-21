from __future__ import annotations

from typing import Any

from pydantic import BaseModel, ConfigDict, Field


class HarnessAction(BaseModel):
    model_config = ConfigDict(extra="forbid")

    type: str
    payload: dict[str, Any] = Field(default_factory=dict)
