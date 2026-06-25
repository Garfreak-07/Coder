from __future__ import annotations

import hashlib
import json
import uuid
from datetime import datetime, timezone
from typing import Any

from .native_events import NativeRuntimeEvent


class NativeRuntimeStore:
    """Append-only native runtime event index with ref-backed payload storage."""

    def __init__(self, *, blob_store: Any | None = None, backing: dict[str, Any] | None = None) -> None:
        self.blob_store = blob_store
        self.backing = backing if backing is not None else {}
        self._events: list[NativeRuntimeEvent] = []

    def append_event(
        self,
        *,
        run_id: str,
        provider_id: str,
        harness_id: str,
        mode: str,
        native_type: str,
        round: int | None = None,
        work_item_id: str | None = None,
        agent_id: str | None = None,
        status: str | None = None,
        summary: str | None = None,
        payload: Any | None = None,
        payload_ref: str | None = None,
        event_id: str | None = None,
        created_at: str | None = None,
        preview_chars: int = 600,
    ) -> NativeRuntimeEvent:
        serialized_payload = _serialize_payload(payload) if payload is not None else None
        ref = payload_ref
        if serialized_payload is not None and ref is None:
            ref = self._write_payload(serialized_payload)
        event = NativeRuntimeEvent(
            event_id=event_id or str(uuid.uuid4()),
            run_id=run_id,
            round=round,
            work_item_id=work_item_id,
            agent_id=agent_id,
            provider_id=provider_id,
            harness_id=harness_id,
            mode=mode,
            native_type=native_type,
            status=status,
            summary=summary,
            payload_ref=ref,
            payload_preview=_preview(serialized_payload, preview_chars) if serialized_payload is not None else None,
            payload_size=len(serialized_payload) if serialized_payload is not None else None,
            created_at=created_at or datetime.now(timezone.utc).isoformat(),
        )
        self._events.append(event)
        return event

    def list_events(self, run_id: str, work_item_id: str | None = None) -> list[NativeRuntimeEvent]:
        return [
            event
            for event in self._events
            if event.run_id == run_id and (work_item_id is None or event.work_item_id == work_item_id)
        ]

    def refs_for_run(self, run_id: str) -> list[str]:
        return [event.event_id for event in self.list_events(run_id)]

    def refs_for_work_item(self, run_id: str, work_item_id: str) -> list[str]:
        return [event.event_id for event in self.list_events(run_id, work_item_id=work_item_id)]

    def read_payload(self, payload_ref: str) -> str:
        if self.blob_store is not None:
            return self.blob_store.read_text(payload_ref)
        try:
            return str(self.backing[payload_ref])
        except KeyError as exc:
            raise KeyError(payload_ref) from exc

    def _write_payload(self, serialized_payload: str) -> str:
        if self.blob_store is not None:
            return str(self.blob_store.write_text(serialized_payload))
        digest = hashlib.sha256(serialized_payload.encode("utf-8")).hexdigest()
        payload_ref = f"sha256:{digest}"
        self.backing[payload_ref] = serialized_payload
        return payload_ref


def _serialize_payload(payload: Any) -> str:
    if isinstance(payload, str):
        return payload
    return json.dumps(payload, ensure_ascii=False, sort_keys=True, default=str)


def _preview(value: str, limit: int) -> str:
    if len(value) <= limit:
        return value
    head = max(1, limit // 2)
    tail = max(1, limit - head)
    return f"{value[:head]}\n...<truncated>...\n{value[-tail:]}"


__all__ = ["NativeRuntimeStore"]
