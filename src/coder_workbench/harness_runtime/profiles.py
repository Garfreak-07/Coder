from __future__ import annotations

import os
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


class LLMProviderProfile(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    provider: str
    api_format: str = "openai"
    auth_env_candidates: list[str] = Field(default_factory=list)
    default_model: str
    base_url: str | None = None
    model_aliases: dict[str, str] = Field(default_factory=dict)
    credential_slot: str | None = None
    capabilities: dict[str, Any] = Field(default_factory=dict)


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
            allowed_artifacts=["project_plan_draft", "run_contract_draft", "planner_chat_turn"],
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
            allowed_artifacts=["planner_order", "planner_decision", "final_report", "workflow_activity_update"],
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
            allowed_artifacts=["project_plan_draft", "run_contract_draft", "planner_chat_turn"],
            tool_policy={"write_files": False, "run_commands": False},
        ),
        _profile(
            "internal-fallback-workflow-supervisor",
            provider_id=INTERNAL_FALLBACK_PROVIDER_ID,
            harness_id=CONVERSATION_HARNESS_ID,
            mode="workflow_supervisor",
            context_profile="workflow-supervisor",
            allowed_artifacts=["planner_order", "planner_decision", "final_report", "workflow_activity_update"],
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


DEFAULT_LLM_PROVIDER_PROFILES: dict[str, LLMProviderProfile] = {
    "deepseek-default": LLMProviderProfile(
        id="deepseek-default",
        provider="deepseek",
        api_format="openai",
        auth_env_candidates=["LLM_API_KEY", "DEEPSEEK_API_KEY"],
        default_model="deepseek-v4-flash",
        base_url="https://api.deepseek.com",
        model_aliases={
            "deepseek-v4-flash": "deepseek/deepseek-v4-flash",
            "deepseek-v4-pro": "deepseek/deepseek-v4-pro",
            "deepseek-chat": "deepseek/deepseek-chat",
            "deepseek-reasoner": "deepseek/deepseek-reasoner",
        },
        capabilities={
            "structured_json_contracts": True,
            "tool_use": True,
        },
    ),
    "openai-compatible-env": LLMProviderProfile(
        id="openai-compatible-env",
        provider="openai-compatible",
        api_format="openai",
        auth_env_candidates=["LLM_API_KEY"],
        default_model="gpt-5.5",
        base_url=None,
        capabilities={
            "structured_json_contracts": True,
            "tool_use": True,
        },
    ),
}


def default_harness_runtime_profiles() -> dict[str, HarnessRuntimeProfile]:
    return dict(DEFAULT_HARNESS_RUNTIME_PROFILES)


def default_llm_provider_profiles() -> dict[str, LLMProviderProfile]:
    return dict(DEFAULT_LLM_PROVIDER_PROFILES)


def harness_runtime_profile_for_id(profile_id: str) -> HarnessRuntimeProfile:
    try:
        return DEFAULT_HARNESS_RUNTIME_PROFILES[profile_id]
    except KeyError as exc:
        raise ValueError(f"unknown harness runtime profile {profile_id!r}") from exc


def llm_provider_profile_for_id(profile_id: str) -> LLMProviderProfile:
    try:
        return DEFAULT_LLM_PROVIDER_PROFILES[profile_id]
    except KeyError as exc:
        raise ValueError(f"unknown LLM provider profile {profile_id!r}") from exc


def resolve_llm_provider_profile(profile_id: str | None = None) -> LLMProviderProfile:
    selected = (profile_id or os.getenv("CODER_LLM_PROVIDER_PROFILE") or "deepseek-default").strip()
    return llm_provider_profile_for_id(selected or "deepseek-default")


def normalize_llm_model(model: str, *, profile: LLMProviderProfile, base_url: str | None = None) -> str:
    text = model.strip() or profile.default_model
    if "/" in text:
        return text
    alias = profile.model_aliases.get(text)
    if alias:
        return alias
    if profile.provider == "deepseek" and "deepseek.com" in str(base_url or profile.base_url or "").lower() and text.startswith("v"):
        return f"deepseek/{text}"
    return text


__all__ = [
    "DEFAULT_HARNESS_RUNTIME_PROFILES",
    "DEFAULT_LLM_PROVIDER_PROFILES",
    "INTERNAL_FALLBACK_PROVIDER_ID",
    "OPENHANDS_PROVIDER_ID",
    "HarnessBindings",
    "HarnessModeBinding",
    "HarnessRuntimeProfile",
    "LLMProviderProfile",
    "default_harness_runtime_profiles",
    "default_llm_provider_profiles",
    "harness_runtime_profile_for_id",
    "llm_provider_profile_for_id",
    "normalize_llm_model",
    "resolve_llm_provider_profile",
]
