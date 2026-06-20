from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.agent_graph.schema import WorkItem, WorkItemStatus


class BlockedWorkItem(BaseModel):
    model_config = ConfigDict(extra="forbid")

    work_item: WorkItem
    blocked_by: list[str] = Field(default_factory=list)


class AgentGraphScheduler:
    """Dependency scheduler for PlannerOrder work items.

    It enforces wait-all dependencies and dispatches ready work items in
    Planner order_index order.
    """

    def __init__(self, work_items: list[WorkItem], *, max_concurrency: int = 4) -> None:
        self.work_items = sorted(work_items, key=lambda item: item.order_index)
        self.max_concurrency = max(1, max_concurrency)
        self.status_by_id: dict[str, WorkItemStatus] = {item.work_item_id: "pending" for item in self.work_items}

    def has_pending(self) -> bool:
        return any(status == "pending" for status in self.status_by_id.values())

    def ready_items(self) -> list[WorkItem]:
        ready = [
            item
            for item in self.work_items
            if self.status_by_id.get(item.work_item_id) == "pending"
            and all(self.status_by_id.get(upstream_id) == "completed" for upstream_id in item.depends_on)
        ]
        return ready[: self.max_concurrency]

    def waiting_items(self) -> list[WorkItem]:
        return [
            item
            for item in self.work_items
            if self.status_by_id.get(item.work_item_id) == "pending"
            and item not in self.ready_items()
            and not self.failed_upstreams(item)
        ]

    def block_items_with_failed_upstreams(self) -> list[BlockedWorkItem]:
        blocked: list[BlockedWorkItem] = []
        for item in self.work_items:
            if self.status_by_id.get(item.work_item_id) != "pending":
                continue
            failed = self.failed_upstreams(item)
            if not failed:
                continue
            self.status_by_id[item.work_item_id] = "blocked"
            blocked.append(BlockedWorkItem(work_item=item, blocked_by=failed))
        return blocked

    def failed_upstreams(self, item: WorkItem) -> list[str]:
        return [
            upstream_id
            for upstream_id in item.depends_on
            if self.status_by_id.get(upstream_id) in {"blocked", "failed"}
        ]

    def mark_running(self, work_item_id: str) -> None:
        self._set_status(work_item_id, "running")

    def mark_completed(self, work_item_id: str) -> None:
        self._set_status(work_item_id, "completed")

    def mark_failed(self, work_item_id: str) -> None:
        self._set_status(work_item_id, "failed")

    def mark_blocked(self, work_item_id: str) -> None:
        self._set_status(work_item_id, "blocked")

    def _set_status(self, work_item_id: str, status: WorkItemStatus) -> None:
        if work_item_id not in self.status_by_id:
            raise KeyError(work_item_id)
        self.status_by_id[work_item_id] = status
