from __future__ import annotations

from pydantic import BaseModel, ConfigDict


class NativeRuntimeEvent(BaseModel):
    model_config = ConfigDict(extra="forbid")

    event_id: str
    run_id: str
    round: int | None = None
    work_item_id: str | None = None
    agent_id: str | None = None
    provider_id: str
    harness_id: str
    mode: str
    native_type: str
    status: str | None = None
    summary: str | None = None
    payload_ref: str | None = None
    payload_preview: str | None = None
    payload_size: int | None = None
    created_at: str


__all__ = ["NativeRuntimeEvent"]
