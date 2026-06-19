from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field, model_validator


NodeType = Literal["start", "agent", "tool", "mcp_tool", "condition", "loop", "human_gate", "end"]
LoopMode = Literal["while", "for_each", "retry_until"]


class ContextPolicy(BaseModel):
    """Controls how much state is sent into an agent call.

    The default is deliberately token-conservative: pass structured fields and
    short summaries, not full transcripts or repository dumps.
    """

    model_config = ConfigDict(extra="forbid")

    input_keys: list[str] = Field(default_factory=list)
    summary_keys: list[str] = Field(default_factory=list)
    max_items_per_key: int = Field(default=20, ge=1, le=200)
    max_chars_per_value: int = Field(default=4000, ge=500, le=50000)
    include_all_state: bool = False
    include_event_history: bool = False
    include_full_outputs: bool = False


class PermissionPolicy(BaseModel):
    model_config = ConfigDict(extra="forbid")

    read_files: bool = True
    edit_files: bool = False
    run_commands: bool = False
    use_network: bool = False
    requires_approval: bool = True


class AgentSpec(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    name: str | None = None
    role: str
    goal: str
    instructions: str = ""
    provider: str | None = None
    model: str | None = None
    tools: list[str] = Field(default_factory=list)
    output_key: str | None = None
    permissions: PermissionPolicy = Field(default_factory=PermissionPolicy)
    context: ContextPolicy = Field(default_factory=ContextPolicy)


class NodeSpec(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    type: NodeType
    agent_id: str | None = None
    tool: str | None = None
    input: dict[str, Any] = Field(default_factory=dict)
    output_key: str | None = None
    condition: str | None = None
    approval_reason: str | None = None
    loop_mode: LoopMode | None = None
    items_key: str | None = None
    item_key: str | None = None
    iteration_key: str | None = None
    max_iterations: int | None = Field(default=None, ge=1, le=50)
    collect_key: str | None = None
    summary_key: str | None = None

    @model_validator(mode="after")
    def validate_node_ref(self) -> "NodeSpec":
        if self.type == "agent" and not self.agent_id:
            raise ValueError(f"agent node {self.id} requires agent_id")
        if self.type in {"tool", "mcp_tool"} and not self.tool:
            raise ValueError(f"tool node {self.id} requires tool")
        if self.type == "condition" and not self.condition:
            raise ValueError(f"condition node {self.id} requires condition")
        if self.type == "loop":
            if not self.loop_mode:
                self.loop_mode = "retry_until"
            if self.max_iterations is None:
                self.max_iterations = 3
            if self.loop_mode == "for_each" and not self.items_key:
                raise ValueError(f"loop node {self.id} in for_each mode requires items_key")
        return self


class EdgeSpec(BaseModel):
    model_config = ConfigDict(extra="forbid")

    from_node: str = Field(alias="from")
    to_node: str = Field(alias="to")
    when: str | None = None
    priority: int = 0
    max_traversals: int | None = Field(default=None, ge=1, le=50)


class WorkflowSpec(BaseModel):
    model_config = ConfigDict(extra="forbid", populate_by_name=True)

    id: str
    version: str = "0.2"
    name: str
    description: str = ""
    max_steps: int = Field(default=50, ge=1, le=500)
    max_agent_calls: int = Field(default=12, ge=0, le=100)
    max_tool_calls: int = Field(default=30, ge=0, le=200)
    token_budget: int | None = Field(default=120000, ge=1000)
    agents: list[AgentSpec] = Field(default_factory=list)
    nodes: list[NodeSpec]
    edges: list[EdgeSpec] = Field(default_factory=list)
    stop_conditions: list[str] = Field(default_factory=list)

    @model_validator(mode="after")
    def validate_graph(self) -> "WorkflowSpec":
        node_ids = {node.id for node in self.nodes}
        agent_ids = {agent.id for agent in self.agents}

        if not any(node.type == "start" for node in self.nodes):
            raise ValueError("workflow requires at least one start node")
        if not any(node.type == "end" for node in self.nodes):
            raise ValueError("workflow requires at least one end node")

        for node in self.nodes:
            if node.agent_id and node.agent_id not in agent_ids:
                raise ValueError(f"node {node.id} references unknown agent: {node.agent_id}")

        for edge in self.edges:
            if edge.from_node not in node_ids:
                raise ValueError(f"edge source not found: {edge.from_node}")
            if edge.to_node not in node_ids:
                raise ValueError(f"edge target not found: {edge.to_node}")

        return self

    def node_by_id(self) -> dict[str, NodeSpec]:
        return {node.id: node for node in self.nodes}

    def agent_by_id(self) -> dict[str, AgentSpec]:
        return {agent.id: agent for agent in self.agents}


def load_workflow(path: str | Path) -> WorkflowSpec:
    workflow_path = Path(path).expanduser().resolve()
    data = json.loads(workflow_path.read_text(encoding="utf-8"))
    return WorkflowSpec.model_validate(data)
