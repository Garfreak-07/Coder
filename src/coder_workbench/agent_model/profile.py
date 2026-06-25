from __future__ import annotations

from typing import Any

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.agent_model.token_budget import TokenBudget
from coder_workbench.core.authority import AgentAuthorityProfile


class AgentRuntimeProfile(BaseModel):
    """Internal profile compiled from AgentRecipe and installed extensions."""

    model_config = ConfigDict(extra="forbid")

    agent_id: str
    agent_name: str = ""
    role: str
    role_card: str | None = None
    agent_archetype: str = ""
    engine_id: str
    harness_id: str | None = None
    harness_runtime_profile_id: str | None = None
    harness_provider_id: str | None = None
    harness_mode: str | None = None
    authority: AgentAuthorityProfile | None = None
    context_profile: str
    context_policy: dict[str, Any] = Field(default_factory=dict)
    token_budget: TokenBudget
    allowed_artifacts: list[str] = Field(default_factory=list)
    plugin_policy: dict[str, Any] = Field(default_factory=dict)
    skill_policy: dict[str, Any] = Field(default_factory=dict)
    memory_policy: dict[str, Any] = Field(default_factory=dict)
    prompt_layers: dict[str, Any] | None = None
    internal_loops: dict[str, Any] = Field(default_factory=dict)
    repair_policy: dict[str, Any] = Field(default_factory=dict)
    tool_policy: dict[str, Any] = Field(default_factory=dict)
    evaluation_profile: dict[str, Any] = Field(default_factory=dict)
