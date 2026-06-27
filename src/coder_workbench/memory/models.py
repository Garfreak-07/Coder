from __future__ import annotations

from datetime import datetime, timezone
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field


AgentMemoryRole = Literal[
    "planning_chat",
    "workflow_supervisor",
    "task_execution",
]

MemoryScope = Literal[
    "user",
    "project",
    "planner_session",
    "workflow_run",
    "knowledge_source",
    "agent_style",
]

MemorySourceType = Literal[
    "manual_note",
    "codebase",
    "document",
    "web_page",
    "project_memory",
    "user_memory",
    "planner_session",
    "workflow_run",
    "agent_style",
]

MemoryPurpose = Literal[
    "coding_knowledge",
    "project_rules",
    "planning_context",
    "execution_context",
    "persona_style",
    "historical_evidence",
    "workflow_checkpoint",
]

MemoryAllowedContext = Literal[
    "assistant_message",
    "planner_task_state",
    "planner_order",
    "execution_prompt",
    "workflow_supervision",
    "final_report",
]

MemorySensitivity = Literal[
    "public",
    "project",
    "private",
    "secret",
]

MemoryTrustLevel = Literal[
    "source",
    "user_confirmed",
    "system_recorded",
    "model_inferred",
]

MemoryStatus = Literal[
    "active",
    "superseded",
    "forgotten",
    "expired",
]


SECRET_MARKERS = (
    "deepseek_api_key",
    "llm_api_key",
    "api_key",
    "password",
    "token",
    "begin rsa",
)


class MemoryAcl(BaseModel):
    model_config = ConfigDict(extra="forbid")

    allowed_agents: list[AgentMemoryRole]
    allowed_contexts: list[MemoryAllowedContext]
    sensitivity: MemorySensitivity = "project"


class MemorySourceRef(BaseModel):
    model_config = ConfigDict(extra="forbid")

    kind: Literal["file", "url", "artifact", "native_event", "manual", "external"]
    ref: str
    title: str | None = None
    content_hash: str | None = None


class MemoryRecord(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    scope: MemoryScope
    source_type: MemorySourceType
    purpose: list[MemoryPurpose]

    title: str
    summary: str
    content: str | None = None

    project_id: str | None = None
    session_id: str | None = None
    run_id: str | None = None

    acl: MemoryAcl
    tags: list[str] = Field(default_factory=list)
    source_refs: list[MemorySourceRef] = Field(default_factory=list)
    evidence_refs: list[str] = Field(default_factory=list)

    trust_level: MemoryTrustLevel
    confidence: Literal["low", "medium", "high"] = "medium"
    status: MemoryStatus = "active"

    created_at: str
    updated_at: str
    expires_at: str | None = None
    supersedes: list[str] = Field(default_factory=list)
    content_hash: str | None = None
    token_estimate: int = 0


class KnowledgeSource(BaseModel):
    model_config = ConfigDict(extra="forbid")

    source_id: str
    kind: Literal["file", "folder", "url", "repo", "manual_note"]
    uri: str
    title: str
    owner_scope: Literal["user", "project", "team"] = "project"
    content_hash: str
    imported_at: str
    metadata: dict[str, Any] = Field(default_factory=dict)


class KnowledgeChunk(BaseModel):
    model_config = ConfigDict(extra="forbid")

    chunk_id: str
    source_id: str
    title: str
    text: str
    summary: str
    tags: list[str] = Field(default_factory=list)
    purpose: list[MemoryPurpose]
    acl: MemoryAcl
    sensitivity: MemorySensitivity = "project"
    trust_level: MemoryTrustLevel = "source"
    content_hash: str
    embedding_id: str | None = None
    token_estimate: int = 0


def validate_memory_record(record: MemoryRecord) -> MemoryRecord:
    """Validate retrieval safety rules for durable memory records."""

    _validate_acl_purpose(
        allowed_agents=record.acl.allowed_agents,
        allowed_contexts=record.acl.allowed_contexts,
        purpose=record.purpose,
        sensitivity=record.acl.sensitivity,
        scope=record.scope,
    )
    if (
        record.trust_level == "model_inferred"
        and record.scope in {"project", "user"}
        and record.confidence == "high"
        and not record.evidence_refs
    ):
        raise ValueError("model_inferred project/user memory requires evidence_refs for high confidence")
    _reject_secret_like_text(record.content)
    return record


def validate_knowledge_chunk(chunk: KnowledgeChunk) -> KnowledgeChunk:
    _validate_acl_purpose(
        allowed_agents=chunk.acl.allowed_agents,
        allowed_contexts=chunk.acl.allowed_contexts,
        purpose=chunk.purpose,
        sensitivity=chunk.sensitivity,
        scope="knowledge_source",
    )
    _reject_secret_like_text(chunk.text)
    return chunk


def memory_has_expired(record: MemoryRecord, *, now: datetime | None = None) -> bool:
    if not record.expires_at:
        return False
    try:
        expires_at = datetime.fromisoformat(record.expires_at.replace("Z", "+00:00"))
    except ValueError:
        return True
    if expires_at.tzinfo is None:
        expires_at = expires_at.replace(tzinfo=timezone.utc)
    current = now or datetime.now(timezone.utc)
    return expires_at <= current


def _validate_acl_purpose(
    *,
    allowed_agents: list[AgentMemoryRole],
    allowed_contexts: list[MemoryAllowedContext],
    purpose: list[MemoryPurpose],
    sensitivity: MemorySensitivity,
    scope: MemoryScope,
) -> None:
    if sensitivity == "secret" and allowed_agents:
        raise ValueError("secret memory must not be retrievable by agents")
    if "persona_style" in purpose:
        if allowed_agents != ["planning_chat"] or allowed_contexts != ["assistant_message"]:
            raise ValueError("persona_style memory is only allowed for planning_chat assistant_message")
    if "task_execution" in allowed_agents and scope == "user":
        raise ValueError("task_execution cannot receive user scope memory")
    if "task_execution" in allowed_agents and "persona_style" in purpose:
        raise ValueError("task_execution cannot receive persona_style memory")


def _reject_secret_like_text(value: str | None) -> None:
    if value is None:
        return
    lower = value.lower()
    for marker in SECRET_MARKERS:
        if marker in lower:
            raise ValueError(f"memory content contains secret-like marker: {marker}")
