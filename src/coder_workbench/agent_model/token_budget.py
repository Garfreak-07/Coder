from __future__ import annotations

from typing import Any

from pydantic import BaseModel, ConfigDict


class TokenBudget(BaseModel):
    model_config = ConfigDict(extra="forbid")

    max_input_tokens: int = 8000
    max_output_tokens: int = 2000
    max_total_tokens: int | None = None
    managed_by_runtime: bool = True

    def __getitem__(self, key: str) -> Any:
        return getattr(self, key)


__all__ = ["TokenBudget"]
