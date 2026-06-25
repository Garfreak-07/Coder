from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, ConfigDict, Field

from .contracts import HarnessContract
from .profiles import HarnessRuntimeProfile


WorkspaceMode = Literal["none", "readonly", "temp_worktree", "docker", "remote_workspace"]


class SandboxPolicy(BaseModel):
    model_config = ConfigDict(extra="forbid")

    workspace_mode: WorkspaceMode = "none"
    command_timeout_seconds: int = 120
    allowed_scopes: list[str] = Field(default_factory=list)
    enforce_scope_restrictions: bool = True
    collect_diff_refs: bool = True
    collect_log_refs: bool = True


def sandbox_policy_for_profile(profile: HarnessRuntimeProfile) -> SandboxPolicy:
    raw = dict(profile.sandbox_policy)
    workspace = raw.pop("workspace", raw.pop("workspace_mode", None))
    if workspace is not None:
        raw["workspace_mode"] = workspace
    return SandboxPolicy(**raw)


def enforce_sandbox_policy(contract: HarnessContract, profile: HarnessRuntimeProfile) -> None:
    policy = sandbox_policy_for_profile(profile)
    if contract.role == "planner" and policy.workspace_mode not in {"none", "readonly"}:
        raise ValueError("Conversation Harness workspace must be none or readonly.")
    if contract.role == "executor" and policy.workspace_mode not in {"temp_worktree", "docker", "remote_workspace"}:
        raise ValueError("Task Execution Harness requires an isolated workspace.")


__all__ = ["SandboxPolicy", "WorkspaceMode", "enforce_sandbox_policy", "sandbox_policy_for_profile"]
