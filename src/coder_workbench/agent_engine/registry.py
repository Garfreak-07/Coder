from __future__ import annotations

from typing import Any

from .runtime import (
    AgentEngine,
    CodeWorkerEngine,
    FinalReviewEngine,
    PlannerEngine,
    SynthesizerEngine,
    TesterEngine,
)


class AgentEngineRegistry:
    def __init__(self, engines: list[Any] | None = None) -> None:
        self._engines = {
            engine.id: engine
            for engine in (
                engines
                or [
                    PlannerEngine(),
                    CodeWorkerEngine(),
                    TesterEngine(),
                    FinalReviewEngine(),
                    SynthesizerEngine(),
                ]
            )
        }

    def get(self, engine_id: str) -> AgentEngine:
        try:
            return self._engines[engine_id]
        except KeyError as exc:
            raise KeyError(f"unknown AgentEngine {engine_id!r}") from exc

    def ids(self) -> list[str]:
        return sorted(self._engines)

    def planner(self) -> PlannerEngine:
        return self.get(PlannerEngine.id)  # type: ignore[return-value]

    def tester(self) -> TesterEngine:
        return self.get(TesterEngine.id)  # type: ignore[return-value]

    def final_review(self) -> FinalReviewEngine:
        return self.get(FinalReviewEngine.id)  # type: ignore[return-value]

    def synthesizer(self) -> SynthesizerEngine:
        return self.get(SynthesizerEngine.id)  # type: ignore[return-value]


def default_agent_engine_registry() -> AgentEngineRegistry:
    return AgentEngineRegistry()
