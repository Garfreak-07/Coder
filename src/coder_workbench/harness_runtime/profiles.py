from __future__ import annotations

from typing import Any

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.agent_model.token_budget import TokenBudget

from .contracts import CONVERSATION_HARNESS_ID, TASK_EXECUTION_HARNESS_ID, HarnessMode, harness_contract_for_id


OPENHANDS_PROVIDER_ID = "openhands-sdk"
INTERNAL_FALLBACK_PROVIDER_ID = "internal-fallback"


class HarnessModeBinding(BaseModel):
    model_config = ConfigDict(extra="forbid")

    profile_id: str
    provider_id: str = OPENHANDS_PROVIDER_ID


class HarnessBindings(BaseModel):
    model_config = ConfigDict(extra="forbid")

    planning_chat: HarnessModeBinding = Field(
        default_factory=lambda: HarnessModeBinding(profile_id="openhands-planning-chat-default")
    )
    workflow_supervisor: HarnessModeBinding = Field(
        default_factory=lambda: HarnessModeBinding(profile_id="openhands-workflow-supervisor-default")
    )
    task_execution: HarnessModeBinding = Field(
        default_factory=lambda: HarnessModeBinding(profile_id="openhands-task-executor-default")
    )
    agent_overrides: dict[str, dict[str, HarnessModeBinding]] = Field(default_factory=dict)


class HarnessRuntimeProfile(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    provider_id: str = OPENHANDS_PROVIDER_ID
    harness_id: str
    mode: HarnessMode
    model_tier: str = "standard"
    context_profile: str
    token_budget: TokenBudget
    allowed_artifacts: list[str] = Field(default_factory=list)
    tool_policy: dict[str, Any] = Field(default_factory=dict)
    memory_policy: dict[str, Any] = Field(default_factory=dict)
    skill_policy: dict[str, Any] = Field(default_factory=dict)
    sandbox_policy: dict[str, Any] = Field(default_factory=dict)
    safety_policy: dict[str, Any] = Field(default_factory=dict)
    evaluation_profile: dict[str, Any] = Field(default_factory=dict)


def _profile(
    profile_id: str,
    *,
    provider_id: str,
    harness_id: str,
    mode: HarnessMode,
    context_profile: str,
    allowed_artifacts: list[str],
    token_budget: TokenBudget | None = None,
    tool_policy: dict[str, Any] | None = None,
    memory_policy: dict[str, Any] | None = None,
    skill_policy: dict[str, Any] | None = None,
    sandbox_policy: dict[str, Any] | None = None,
    safety_policy: dict[str, Any] | None = None,
) -> HarnessRuntimeProfile:
    contract = harness_contract_for_id(harness_id)
    if mode not in contract.modes:
        raise ValueError(f"profile {profile_id!r} uses mode {mode!r} outside harness {harness_id!r}")
    return HarnessRuntimeProfile(
        id=profile_id,
        provider_id=provider_id,
        harness_id=contract.harness_id,
        mode=mode,
        context_profile=context_profile,
        token_budget=token_budget or TokenBudget(),
        allowed_artifacts=allowed_artifacts,
        tool_policy=tool_policy or {},
        memory_policy=memory_policy or {},
        skill_policy=skill_policy or {},
        sandbox_policy=sandbox_policy or {},
        safety_policy=safety_policy or {},
    )


DEFAULT_HARNESS_RUNTIME_PROFILES: dict[str, HarnessRuntimeProfile] = {
    profile.id: profile
    for profile in (
        _profile(
            "openhands-planning-chat-default",
            provider_id=OPENHANDS_PROVIDER_ID,
            harness_id=CONVERSATION_HARNESS_ID,
            mode="planning_chat",
            context_profile="planning-chat",
            allowed_artifacts=["project_plan_draft", "run_contract_draft"],
            tool_policy={"write_files": False, "run_commands": False},
            memory_policy={"read": True, "write": False},
            skill_policy={"inspect": True},
            sandbox_policy={"workspace": "readonly"},
            safety_policy={"external_publish": False},
        ),
        _profile(
            "openhands-workflow-supervisor-default",
            provider_id=OPENHANDS_PROVIDER_ID,
            harness_id=CONVERSATION_HARNESS_ID,
            mode="workflow_supervisor",
            context_profile="workflow-supervisor",
            allowed_artifacts=["planner_order", "planner_decision", "final_report"],
            tool_policy={"write_files": False, "run_commands": False},
            memory_policy={"read": True, "write": False},
            skill_policy={"inspect": True},
            sandbox_policy={"workspace": "readonly"},
            safety_policy={"external_publish": False},
        ),
        _profile(
            "openhands-task-executor-default",
            provider_id=OPENHANDS_PROVIDER_ID,
            harness_id=TASK_EXECUTION_HARNESS_ID,
            mode="task_execution",
            context_profile="task-execution",
            allowed_artifacts=["execution_result"],
            tool_policy={"write_files": True, "run_commands": True, "ask_human": False},
            memory_policy={"read": "scoped_refs", "write": False},
            skill_policy={"load_scoped": True},
            sandbox_policy={"workspace": "temp_worktree"},
            safety_policy={"git_commit": False, "git_push": False, "deploy": False},
        ),
        _profile(
            "internal-fallback-planning-chat",
            provider_id=INTERNAL_FALLBACK_PROVIDER_ID,
            harness_id=CONVERSATION_HARNESS_ID,
            mode="planning_chat",
            context_profile="planning-chat",
            allowed_artifacts=["project_plan_draft", "run_contract_draft"],
            tool_policy={"write_files": False, "run_commands": False},
        ),
        _profile(
            "internal-fallback-workflow-supervisor",
            provider_id=INTERNAL_FALLBACK_PROVIDER_ID,
            harness_id=CONVERSATION_HARNESS_ID,
            mode="workflow_supervisor",
            context_profile="workflow-supervisor",
            allowed_artifacts=["planner_order", "planner_decision", "final_report"],
            tool_policy={"write_files": False, "run_commands": False},
        ),
        _profile(
            "internal-fallback-task-executor",
            provider_id=INTERNAL_FALLBACK_PROVIDER_ID,
            harness_id=TASK_EXECUTION_HARNESS_ID,
            mode="task_execution",
            context_profile="task-execution",
            allowed_artifacts=["execution_result"],
            tool_policy={"write_files": True, "run_commands": True, "ask_human": False},
            sandbox_policy={"workspace": "temp_worktree"},
            safety_policy={"git_commit": False, "git_push": False, "deploy": False},
        ),
    )
}


def default_harness_runtime_profiles() -> dict[str, HarnessRuntimeProfile]:
    return dict(DEFAULT_HARNESS_RUNTIME_PROFILES)


def harness_runtime_profile_for_id(profile_id: str) -> HarnessRuntimeProfile:
    try:
        return DEFAULT_HARNESS_RUNTIME_PROFILES[profile_id]
    except KeyError as exc:
        raise ValueError(f"unknown harness runtime profile {profile_id!r}") from exc


__all__ = [
    "DEFAULT_HARNESS_RUNTIME_PROFILES",
    "INTERNAL_FALLBACK_PROVIDER_ID",
    "OPENHANDS_PROVIDER_ID",
    "HarnessBindings",
    "HarnessModeBinding",
    "HarnessRuntimeProfile",
    "default_harness_runtime_profiles",
    "harness_runtime_profile_for_id",
]
