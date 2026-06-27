from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.memory.models import (
    AgentMemoryRole,
    MemoryAllowedContext,
    MemoryPurpose,
    MemoryScope,
)


class AgentMemoryPolicy(BaseModel):
    model_config = ConfigDict(extra="forbid")

    role: AgentMemoryRole
    allowed_scopes: list[MemoryScope]
    allowed_purposes: list[MemoryPurpose]
    allowed_contexts: list[MemoryAllowedContext]
    max_records: int = Field(ge=0)
    max_tokens: int = Field(ge=0)


PLANNING_CHAT_MEMORY_POLICY = AgentMemoryPolicy(
    role="planning_chat",
    allowed_scopes=[
        "user",
        "project",
        "planner_session",
        "workflow_run",
        "knowledge_source",
        "agent_style",
    ],
    allowed_purposes=[
        "coding_knowledge",
        "project_rules",
        "planning_context",
        "persona_style",
        "historical_evidence",
        "workflow_checkpoint",
    ],
    allowed_contexts=[
        "assistant_message",
        "planner_task_state",
    ],
    max_records=12,
    max_tokens=4000,
)

WORKFLOW_SUPERVISOR_MEMORY_POLICY = AgentMemoryPolicy(
    role="workflow_supervisor",
    allowed_scopes=[
        "project",
        "workflow_run",
        "knowledge_source",
    ],
    allowed_purposes=[
        "coding_knowledge",
        "project_rules",
        "planning_context",
        "historical_evidence",
        "workflow_checkpoint",
    ],
    allowed_contexts=[
        "workflow_supervision",
        "planner_order",
        "final_report",
    ],
    max_records=10,
    max_tokens=3000,
)

TASK_EXECUTION_MEMORY_POLICY = AgentMemoryPolicy(
    role="task_execution",
    allowed_scopes=[
        "knowledge_source",
        "workflow_run",
    ],
    allowed_purposes=[
        "coding_knowledge",
        "execution_context",
        "historical_evidence",
    ],
    allowed_contexts=[
        "execution_prompt",
    ],
    max_records=6,
    max_tokens=2000,
)


DEFAULT_MEMORY_POLICIES: dict[AgentMemoryRole, AgentMemoryPolicy] = {
    "planning_chat": PLANNING_CHAT_MEMORY_POLICY,
    "workflow_supervisor": WORKFLOW_SUPERVISOR_MEMORY_POLICY,
    "task_execution": TASK_EXECUTION_MEMORY_POLICY,
}


def policy_for_role(role: AgentMemoryRole) -> AgentMemoryPolicy:
    return DEFAULT_MEMORY_POLICIES[role]
