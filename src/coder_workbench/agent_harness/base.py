from __future__ import annotations

from collections.abc import Callable
from typing import Any

from pydantic import BaseModel, ConfigDict, Field

from .actions import HarnessAction
from .observations import HarnessObservation
from .permissions import HarnessPermissionPolicy
from .policies import HarnessPolicy
from .scratchpad import Scratchpad


DecisionFn = Callable[["HarnessTask", dict[str, Any], Scratchpad], HarnessAction]
ToolFn = Callable[[HarnessAction, dict[str, Any]], HarnessObservation]


class HarnessTask(BaseModel):
    model_config = ConfigDict(extra="forbid")

    task_id: str = ""
    goal: str
    payload: dict[str, Any] = Field(default_factory=dict)


class HarnessResult(BaseModel):
    model_config = ConfigDict(extra="forbid")

    status: str
    artifact: dict[str, Any] | None = None
    observations: list[HarnessObservation] = Field(default_factory=list)
    blocker_type: str | None = None
    planner_question: str | None = None


class AgentHarness:
    def __init__(
        self,
        *,
        policy: HarnessPolicy,
        decision_fn: DecisionFn | None = None,
        tool_fn: ToolFn | None = None,
        permission_policy: HarnessPermissionPolicy | None = None,
    ) -> None:
        self.policy = policy
        self.decision_fn = decision_fn or self._default_decision
        self.tool_fn = tool_fn or self._default_tool
        self.permission_policy = permission_policy or HarnessPermissionPolicy()

    def build_context(self, task: HarnessTask) -> dict[str, Any]:
        return dict(task.payload)

    def run(self, task: HarnessTask) -> HarnessResult:
        context = self.build_context(task)
        scratchpad = Scratchpad()
        observations: list[HarnessObservation] = []
        for step in range(1, self.policy.max_steps + 1):
            action = self.decision_fn(task, context, scratchpad)
            if action.type == "finish":
                artifact = action.payload.get("artifact")
                return HarnessResult(status="completed", artifact=artifact if isinstance(artifact, dict) else action.payload, observations=observations)
            if not self.permission_policy.allow(action, self.policy):
                return HarnessResult(
                    status="blocked",
                    observations=observations,
                    blocker_type="risk_boundary",
                    planner_question="This action exceeds this Agent's authority.",
                )
            observation = self.tool_fn(action, context)
            observations.append(observation)
            scratchpad.append(step=step, action=action, observation=observation)
            context.setdefault("observations", []).append(observation.model_dump(mode="json"))
        return HarnessResult(
            status="blocked",
            observations=observations,
            blocker_type="technical_blocker",
            planner_question="Harness reached max_steps before producing an artifact.",
        )

    def _default_decision(self, task: HarnessTask, context: dict[str, Any], scratchpad: Scratchpad) -> HarnessAction:
        return HarnessAction(type="finish", payload={"artifact": {"summary": task.goal}})

    def _default_tool(self, action: HarnessAction, context: dict[str, Any]) -> HarnessObservation:
        return HarnessObservation(action_type=action.type, summary=f"Observed action {action.type}.")
