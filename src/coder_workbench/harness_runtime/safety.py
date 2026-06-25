from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field

from .contracts import HarnessContract
from .profiles import HarnessRuntimeProfile


class SafetyDecision(BaseModel):
    model_config = ConfigDict(extra="forbid")

    allowed: bool
    reasons: list[str] = Field(default_factory=list)


def evaluate_harness_safety(contract: HarnessContract, profile: HarnessRuntimeProfile) -> SafetyDecision:
    """Deny profile policies that violate canonical harness boundaries."""

    reasons: list[str] = []
    tool_policy = profile.tool_policy
    memory_policy = profile.memory_policy
    safety_policy = profile.safety_policy

    if contract.role == "planner":
        if bool(tool_policy.get("write_files")):
            reasons.append("Conversation Harness cannot write files.")
        if bool(tool_policy.get("run_commands")):
            reasons.append("Conversation Harness cannot run commands.")
        if bool(safety_policy.get("git_commit")) or bool(safety_policy.get("git_push")):
            reasons.append("Conversation Harness cannot commit or push.")
        if bool(safety_policy.get("deploy")) or bool(safety_policy.get("external_publish")):
            reasons.append("Conversation Harness cannot publish externally.")

    if contract.role == "executor":
        if bool(tool_policy.get("ask_human")):
            reasons.append("Task Execution Harness cannot talk to the user.")
        if bool(memory_policy.get("write")):
            reasons.append("Task Execution Harness cannot write long-term memory directly.")
        if bool(safety_policy.get("git_commit")):
            reasons.append("Task Execution Harness cannot commit changes.")
        if bool(safety_policy.get("git_push")):
            reasons.append("Task Execution Harness cannot push changes.")
        if bool(safety_policy.get("deploy")) or bool(safety_policy.get("external_publish")):
            reasons.append("Task Execution Harness cannot deploy or publish.")

    return SafetyDecision(allowed=not reasons, reasons=reasons)


def enforce_harness_safety(contract: HarnessContract, profile: HarnessRuntimeProfile) -> None:
    decision = evaluate_harness_safety(contract, profile)
    if not decision.allowed:
        raise ValueError("; ".join(decision.reasons))


__all__ = ["SafetyDecision", "enforce_harness_safety", "evaluate_harness_safety"]
