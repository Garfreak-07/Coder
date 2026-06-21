from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field


class HarnessPolicy(BaseModel):
    model_config = ConfigDict(extra="forbid")

    max_steps: int = Field(default=4, ge=1)
    can_ask_human: bool = False
    can_modify_files: bool = False
    can_run_commands: bool = False
    can_write_memory: bool = False
    allowed_artifacts: list[str] = Field(default_factory=list)


def planner_policy() -> HarnessPolicy:
    return HarnessPolicy(
        max_steps=12,
        can_ask_human=True,
        can_modify_files=False,
        can_run_commands=False,
        can_write_memory=True,
        allowed_artifacts=["run_contract", "planner_order", "planner_decision", "round_summary"],
    )


def code_worker_policy() -> HarnessPolicy:
    return HarnessPolicy(
        max_steps=8,
        can_ask_human=False,
        can_modify_files=True,
        can_run_commands=False,
        can_write_memory=False,
        allowed_artifacts=["execution_result"],
    )


def tester_policy() -> HarnessPolicy:
    return HarnessPolicy(
        max_steps=6,
        can_ask_human=False,
        can_modify_files=False,
        can_run_commands=True,
        can_write_memory=False,
        allowed_artifacts=["test_result", "check_result", "debug_finding"],
    )


def final_review_policy() -> HarnessPolicy:
    return HarnessPolicy(
        max_steps=4,
        can_ask_human=False,
        can_modify_files=False,
        can_run_commands=False,
        can_write_memory=False,
        allowed_artifacts=["test_result", "round_summary"],
    )
