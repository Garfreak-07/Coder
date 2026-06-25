from __future__ import annotations

from .reducers import apply_state_update
from .schema import SharedRunState, StateUpdate


class RunStateStore:
    """In-memory helper for tests and live runtime staging only.

    Durable SharedRunState persistence is the `data["shared_run_state"]` payload
    inside the stored RunResult.
    """

    def __init__(self) -> None:
        self._states: dict[str, SharedRunState] = {}

    def load(self, run_id: str) -> SharedRunState | None:
        return self._states.get(run_id)

    def save(self, state: SharedRunState) -> None:
        self._states[state.run_id] = state

    def append_update(self, run_id: str, update: StateUpdate) -> SharedRunState:
        state = self.load(run_id)
        if state is None:
            raise KeyError(run_id)
        state = apply_state_update(state, update)
        self.save(state)
        return state
