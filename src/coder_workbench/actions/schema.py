from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field


ACTION_TYPES = {
    "build_context",
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


class RuntimeActionRecord(BaseModel):
    model_config = ConfigDict(extra="forbid")

    artifact_type: Literal["runtime_action"] = "runtime_action"
    effect_type: Literal["runtime_action"] = "runtime_action"
    action_type: str
    status: Literal["ok", "blocked", "failed"]
    work_item_id: str | None = None
    artifact_ref: str
    output_ref: str
    tool_result_ref: str
    requires_planner_replan: bool = False
    reason: str = ""
    error_code: str | None = None
    operation_id: str | None = None
    approval_key: str | None = None
    policy: dict[str, Any] = Field(default_factory=dict)
    action_spec: dict[str, Any]
    requested_action: dict[str, Any] = Field(default_factory=dict)
    replay_of: str | None = None
    action: dict[str, Any] = Field(default_factory=dict)
