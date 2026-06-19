from __future__ import annotations

from datetime import datetime, timezone
from typing import Any, Literal
from uuid import uuid4

from pydantic import BaseModel, ConfigDict, Field, PrivateAttr


EventType = Literal[
    "run.started",
    "node.started",
    "node.completed",
    "node.skipped",
    "agent.called",
    "tool.called",
    "approval.required",
    "edge.selected",
    "budget.warning",
    "run.completed",
    "run.blocked",
    "run.failed",
]


class RunEvent(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str = Field(default_factory=lambda: str(uuid4()))
    type: EventType
    node_id: str | None = None
    message: str
    payload: dict[str, Any] = Field(default_factory=dict)
    created_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))


class RunState(BaseModel):
    model_config = ConfigDict(extra="forbid")

    workflow_id: str
    request: str
    repo_root: str
    data: dict[str, Any] = Field(default_factory=dict)
    summaries: dict[str, str] = Field(default_factory=dict)
    artifacts: dict[str, Any] = Field(default_factory=dict)
    events: list[RunEvent] = Field(default_factory=list)
    visited_nodes: dict[str, int] = Field(default_factory=dict)
    traversed_edges: dict[str, int] = Field(default_factory=dict)
    token_budget: int | None = None
    estimated_tokens_used: int = 0
    agent_calls: int = 0
    tool_calls: int = 0
    status: Literal["running", "completed", "blocked", "failed"] = "running"
    current_node: str | None = None
    _event_sink: Any = PrivateAttr(default=None)

    def emit(self, event_type: EventType, message: str, node_id: str | None = None, **payload: Any) -> None:
        event = RunEvent(type=event_type, node_id=node_id, message=message, payload=payload)
        self.events.append(event)
        if self._event_sink:
            self._event_sink(event)

    def set_event_sink(self, event_sink: Any) -> None:
        self._event_sink = event_sink

    def set_value(self, key: str | None, value: Any) -> None:
        if key:
            self.data[key] = value
            self.summaries[key] = summarize_value(value)


class RunResult(BaseModel):
    model_config = ConfigDict(extra="forbid")

    status: Literal["completed", "blocked", "failed"]
    data: dict[str, Any]
    summaries: dict[str, str]
    events: list[RunEvent]
    estimated_tokens_used: int
    agent_calls: int
    tool_calls: int
    blocked_node_id: str | None = None
    resume_checkpoint: dict[str, Any] | None = None


def summarize_value(value: Any, max_chars: int = 800) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value[:max_chars]
    if isinstance(value, list):
        prefix = f"{len(value)} items"
        sample = value[:5]
        return f"{prefix}: {sample}"[:max_chars]
    if isinstance(value, dict):
        keys = list(value.keys())
        compact = {key: value[key] for key in keys[:8]}
        return str(compact)[:max_chars]
    return str(value)[:max_chars]
