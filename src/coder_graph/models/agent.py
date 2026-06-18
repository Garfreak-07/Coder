from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field


class ClaudeCodeRuntime(BaseModel):
    """Per-agent coding runtime configuration.

    This is a local, conservative equivalent of a Claude Code-style workbench:
    each agent can own a session, scoped workspace permissions, model settings,
    MCP/skill/tool configuration, and an approval policy.
    """

    model_config = ConfigDict(extra="forbid")

    enabled: bool = True
    page: str = "agent_workbench"
    session_id: str | None = None
    provider: str | None = None
    model: str | None = None
    system_prompt: str = ""
    context_files: list[str] = Field(default_factory=list)
    mcp_servers: list[dict] = Field(default_factory=list)
    skills: list[str] = Field(default_factory=list)
    tools: list[str] = Field(default_factory=lambda: ["read", "search", "edit", "shell"])
    enabled_capabilities: list[str] = Field(default_factory=list)
    permissions: dict[str, bool] = Field(
        default_factory=lambda: {
            "read_files": True,
            "edit_files": False,
            "run_commands": False,
            "use_network": False,
            "requires_approval": True,
        }
    )
    memory: dict = Field(default_factory=dict)


class A2ACapability(BaseModel):
    """Local A2A card metadata used for agent-to-agent routing."""

    model_config = ConfigDict(extra="forbid")

    enabled: bool = True
    endpoint: str | None = None
    protocol_version: str = "local-a2a-v1"
    input_modes: list[str] = Field(default_factory=lambda: ["application/json"])
    output_modes: list[str] = Field(default_factory=lambda: ["application/json"])
    message_types: list[str] = Field(default_factory=list)
    subscriptions: list[str] = Field(default_factory=list)


class AgentCard(BaseModel):
    """A small Agent Card inspired by A2A-style capability descriptions."""

    model_config = ConfigDict(extra="forbid")

    id: str
    name: str | None = None
    role: str
    goal: str
    instructions: str = ""
    skills: list[str] = Field(default_factory=list)
    tools: list[str] = Field(default_factory=list)
    input_keys: list[str] = Field(default_factory=list)
    output_schema: dict[str, str] = Field(default_factory=dict)
    stop_rules: list[str] = Field(default_factory=list)
    model: str | None = None
    runtime: ClaudeCodeRuntime = Field(default_factory=ClaudeCodeRuntime)
    a2a: A2ACapability = Field(default_factory=A2ACapability)

    def display_name(self) -> str:
        return self.name or self.id
