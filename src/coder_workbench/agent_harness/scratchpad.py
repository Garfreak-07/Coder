from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field

from .actions import HarnessAction
from .observations import HarnessObservation


class ScratchpadEntry(BaseModel):
    model_config = ConfigDict(extra="forbid")

    step: int
    action: HarnessAction
    observation: HarnessObservation


class Scratchpad(BaseModel):
    model_config = ConfigDict(extra="forbid")

    entries: list[ScratchpadEntry] = Field(default_factory=list)

    def append(self, *, step: int, action: HarnessAction, observation: HarnessObservation) -> None:
        self.entries.append(ScratchpadEntry(step=step, action=action, observation=observation))

    def summaries(self) -> list[str]:
        return [entry.observation.summary for entry in self.entries]
