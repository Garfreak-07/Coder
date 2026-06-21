from __future__ import annotations

from typing import Any
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, model_validator


WorkItemStatus = Literal["pending", "running", "completed", "blocked", "failed"]
ExecutionStatus = Literal["completed", "blocked", "failed"]
TestStatus = Literal["pass", "fail", "blocked", "not_requested"]
PlanStatus = Literal["pending", "running", "completed", "partial_failed", "blocked", "failed", "interrupted"]


class MergeIndexedModel(BaseModel):
    model_config = ConfigDict(extra="forbid")

    merge_index: int = Field(ge=1)

    @model_validator(mode="before")
    @classmethod
    def accept_legacy_order_index(cls, data: Any) -> Any:
        if not isinstance(data, dict) or "order_index" not in data:
            return data
        migrated = dict(data)
        order_index = migrated.pop("order_index")
        if "merge_index" in migrated and migrated["merge_index"] != order_index:
            raise ValueError("merge_index and legacy order_index must match when both are provided")
        migrated.setdefault("merge_index", order_index)
        return migrated

    @property
    def order_index(self) -> int:
        return self.merge_index


class WorkItem(MergeIndexedModel):
    work_item_id: str
    assignee_agent_id: str
    task_summary: str
    depends_on: list[str] = Field(default_factory=list)
    tester_agent_ids: list[str] = Field(default_factory=list)

    @model_validator(mode="after")
    def require_non_empty_fields(self) -> "WorkItem":
        if not self.work_item_id.strip():
            raise ValueError("work_item_id is required")
        if not self.assignee_agent_id.strip():
            raise ValueError("assignee_agent_id is required")
        if not self.task_summary.strip():
            raise ValueError("task_summary is required")
        return self


class PlannerOrderPlanGraph(BaseModel):
    model_config = ConfigDict(extra="forbid")

    work_items: list[WorkItem]
    final_tester_agent_id: str | None = None


class PlannerOrder(BaseModel):
    model_config = ConfigDict(extra="forbid")

    artifact_type: Literal["planner_order"] = "planner_order"
    round: int = Field(default=1, ge=1)
    round_goal: str
    plan_graph: PlannerOrderPlanGraph


class CachedWorkItem(WorkItem):
    status: WorkItemStatus = "pending"


class AgentTaskEnvelope(MergeIndexedModel):
    artifact_type: Literal["agent_task"] = "agent_task"
    round: int = Field(ge=1)
    work_item_id: str
    assigned_agent_id: str
    task_summary: str
    constraints: list[str] = Field(default_factory=list)
    upstream_refs: list[str] = Field(default_factory=list)
    planner_order_ref: str
    allowed_skill_ids: list[str] = Field(default_factory=list)
    loaded_skill_refs: list[str] = Field(default_factory=list)
    omitted_skill_ids: list[str] = Field(default_factory=list)
    estimated_skill_tokens: int = 0
    selected_skill_context: list[dict[str, Any]] = Field(default_factory=list)
    coding_context_packet: dict[str, Any] = Field(default_factory=dict)


class ExecutionRecord(MergeIndexedModel):
    artifact_type: Literal["execution_result", "synthesis_artifact"] = "execution_result"
    work_item_id: str
    agent_id: str
    status: ExecutionStatus
    execution_summary: str
    execution_result_ref: str
    artifact_payload: dict[str, Any] | None = None


class TestRecord(MergeIndexedModel):
    work_item_id: str
    tester_agent_id: str
    status: TestStatus
    test_summary: str
    test_result_ref: str | None = None
    artifact_payload: dict[str, Any] | None = None


class WorkItemOutcome(MergeIndexedModel):
    work_item_id: str
    execution: ExecutionRecord
    tests: list[TestRecord] = Field(default_factory=list)


class FinalTestRecord(BaseModel):
    model_config = ConfigDict(extra="forbid")

    round: int = Field(ge=1)
    final_tester_agent_id: str
    status: TestStatus
    summary: str
    final_test_result_ref: str | None = None
    artifact_payload: dict[str, Any] | None = None


class PlanCache(BaseModel):
    model_config = ConfigDict(extra="forbid")

    round: int = Field(ge=1)
    planner_order_ref: str
    work_items: list[CachedWorkItem]


class PlannerInputBundleItem(MergeIndexedModel):
    work_item_id: str
    task_summary: str
    execution_status: ExecutionStatus | Literal["not_started"]
    execution_summary: str
    test_status: TestStatus
    test_summary: str
    refs: list[str] = Field(default_factory=list)


class PlannerInputInterrupt(BaseModel):
    model_config = ConfigDict(extra="forbid")

    round: int
    work_item_id: str
    merge_index: int
    agent_id: str
    blocker_type: str
    reason: str
    planner_question: str | None = None
    continue_without_human_possible: bool | None = None
    candidate_options: list[dict[str, Any]] = Field(default_factory=list)
    artifact_ref: str


class PlannerInputBundle(BaseModel):
    model_config = ConfigDict(extra="forbid")

    artifact_type: Literal["planner_input_bundle"] = "planner_input_bundle"
    round: int = Field(ge=1)
    planner_order_ref: str
    plan_status: PlanStatus
    items: list[PlannerInputBundleItem]
    final_test_summary: str | None = None
    final_test_ref: str | None = None
    effects: list[dict[str, Any]] = Field(default_factory=list)
    interrupts: list[PlannerInputInterrupt] = Field(default_factory=list)


class RoundSummaryItem(MergeIndexedModel):
    work_item_id: str
    status: str
    summary: str
    refs: list[str] = Field(default_factory=list)


class PlanRunSummary(BaseModel):
    model_config = ConfigDict(extra="forbid")

    artifact_type: Literal["round_summary"] = "round_summary"
    round: int = Field(ge=1)
    planner_order_ref: str
    plan_status: PlanStatus
    completed_count: int = 0
    failed_count: int = 0
    blocked_count: int = 0
    ordered_state: list[RoundSummaryItem] = Field(default_factory=list)
    remaining_work: list[str] = Field(default_factory=list)
    carry_forward_constraints: list[str] = Field(default_factory=list)
