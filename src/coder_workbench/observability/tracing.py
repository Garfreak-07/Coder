from __future__ import annotations

from datetime import datetime, timezone
from typing import Any, Literal
from uuid import uuid4

from pydantic import BaseModel, ConfigDict, Field


TraceStatus = Literal["running", "ok", "failed", "blocked"]


def _utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


class TraceSpan(BaseModel):
    model_config = ConfigDict(extra="forbid")

    trace_id: str
    span_id: str = Field(default_factory=lambda: str(uuid4()))
    parent_span_id: str | None = None
    name: str
    kind: str
    start_time: str = Field(default_factory=_utc_now)
    end_time: str | None = None
    status: TraceStatus = "running"
    attributes: dict[str, Any] = Field(default_factory=dict)

    def finish(self, status: TraceStatus = "ok", **attributes: Any) -> "TraceSpan":
        merged = dict(self.attributes)
        merged.update({key: value for key, value in attributes.items() if value is not None})
        return self.model_copy(update={"status": status, "end_time": _utc_now(), "attributes": merged})

    def event_payload(self) -> dict[str, str]:
        payload = {"trace_id": self.trace_id, "span_id": self.span_id}
        if self.parent_span_id:
            payload["parent_span_id"] = self.parent_span_id
        return payload


class TraceContext:
    def __init__(self, trace_id: str | None = None) -> None:
        self.trace_id = trace_id or str(uuid4())
        self.spans: list[TraceSpan] = []

    def start_span(
        self,
        *,
        name: str,
        kind: str,
        parent: TraceSpan | None = None,
        **attributes: Any,
    ) -> TraceSpan:
        span = TraceSpan(
            trace_id=self.trace_id,
            parent_span_id=parent.span_id if parent is not None else None,
            name=name,
            kind=kind,
            attributes={key: value for key, value in attributes.items() if value is not None},
        )
        self.spans.append(span)
        return span

    def finish_span(self, span: TraceSpan, status: TraceStatus = "ok", **attributes: Any) -> TraceSpan:
        finished = span.finish(status, **attributes)
        for index, current in enumerate(self.spans):
            if current.span_id == span.span_id:
                self.spans[index] = finished
                break
        return finished

    def spans_payload(self) -> list[dict[str, Any]]:
        return [span.model_dump(mode="json") for span in self.spans]
