from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field


ACTION_TYPES = {
    "build_context",
    "load_skill",
    "call_plugin",
    "call_mcp",
    "repo_index",
    "propose_patch",
    "apply_patch_sandbox",
    "run_command_sandbox",
    "run_command",
    "validate_artifact",
    "repair_artifact",
}


class ActionSpec(BaseModel):
    model_config = ConfigDict(extra="forbid")

    action_id: str
    action_type: str
    input: dict[str, Any] = Field(default_factory=dict)
    risk_level: Literal["low", "medium", "high"] = "low"
    estimated_tokens: int = Field(default=0, ge=0)
    requires_permission: bool = False


class ActionResult(BaseModel):
    model_config = ConfigDict(extra="forbid")

    status: Literal["ok", "blocked", "failed"]
    output_ref: str | None = None
    summary: str = ""
    token_used: int = Field(default=0, ge=0)
    error_code: str | None = None
    payload: dict[str, Any] = Field(default_factory=dict)
