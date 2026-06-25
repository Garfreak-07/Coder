from __future__ import annotations

from datetime import datetime, timezone
from typing import Any

from .schema import SharedRunState


class RunStateCheckpointer:
    """In-memory helper for tests and live runtime staging only.

    Durable checkpoint state is carried in RunResult resume checkpoints and the
    persisted `data["shared_run_state"]` payload.
    """

    def __init__(self) -> None:
        self._checkpoints: dict[str, list[dict[str, Any]]] = {}

    def checkpoint(self, run_id: str, phase: str, state: SharedRunState) -> dict[str, Any]:
        record = {
            "checkpoint_id": f"{run_id}:{len(self._checkpoints.get(run_id, [])) + 1}",
            "run_id": run_id,
            "phase": phase,
            "created_at": datetime.now(timezone.utc).isoformat(),
            "state": state.model_dump(mode="json"),
        }
        self._checkpoints.setdefault(run_id, []).append(record)
        return record

    def resume(self, run_id: str, checkpoint_id: str | None = None) -> SharedRunState | None:
        records = self._checkpoints.get(run_id, [])
        if not records:
            return None
        if checkpoint_id is None:
            return SharedRunState.model_validate(records[-1]["state"])
        for record in records:
            if record["checkpoint_id"] == checkpoint_id:
                return SharedRunState.model_validate(record["state"])
        return None
