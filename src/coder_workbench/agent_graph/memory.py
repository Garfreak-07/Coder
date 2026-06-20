from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.agent_graph.schema import PlannerInputBundle, PlanRunSummary


class WorkflowMemory(BaseModel):
    model_config = ConfigDict(extra="forbid")

    workflow_id: str
    successful_assignments: list[dict[str, Any]] = Field(default_factory=list)
    common_blockers: list[dict[str, Any]] = Field(default_factory=list)
    planner_notes: list[dict[str, Any]] = Field(default_factory=list)
    updated_at: str


class PlannerMemoryStore:
    def __init__(self, repo_root: str | Path) -> None:
        self.repo_root = Path(repo_root)
        self.root = self.repo_root / ".coder" / "memory" / "workflows"

    def load_workflow_memory(self, workflow_id: str) -> WorkflowMemory:
        path = self._path(workflow_id)
        if not path.exists():
            return WorkflowMemory(workflow_id=workflow_id, updated_at=_now())
        try:
            return WorkflowMemory.model_validate(json.loads(path.read_text(encoding="utf-8")))
        except Exception:
            return WorkflowMemory(workflow_id=workflow_id, updated_at=_now())

    def record_round(
        self,
        *,
        workflow_id: str,
        bundle: PlannerInputBundle,
        round_summary: PlanRunSummary,
        planner_decision: dict[str, Any],
    ) -> WorkflowMemory:
        memory = self.load_workflow_memory(workflow_id)
        for item in bundle.items:
            if item.execution_status == "completed" and item.test_status in {"pass", "not_requested"}:
                memory.successful_assignments.append(
                    {
                        "round": bundle.round,
                        "work_item_id": item.work_item_id,
                        "task_summary": item.task_summary,
                        "refs": item.refs,
                    }
                )
        for interrupt in bundle.interrupts:
            memory.common_blockers.append(
                {
                    "round": interrupt.round,
                    "work_item_id": interrupt.work_item_id,
                    "blocker_type": interrupt.blocker_type,
                    "reason": interrupt.reason,
                    "planner_question": interrupt.planner_question,
                    "artifact_ref": interrupt.artifact_ref,
                }
            )
        memory.planner_notes.append(
            {
                "round": bundle.round,
                "plan_status": bundle.plan_status,
                "next_action": planner_decision.get("next_action"),
                "reason": planner_decision.get("reason"),
                "remaining_work": round_summary.remaining_work,
            }
        )
        memory.updated_at = _now()
        self.save_workflow_memory(memory)
        return memory

    def save_workflow_memory(self, memory: WorkflowMemory) -> None:
        self.root.mkdir(parents=True, exist_ok=True)
        self._path(memory.workflow_id).write_text(memory.model_dump_json(indent=2), encoding="utf-8")

    def _path(self, workflow_id: str) -> Path:
        safe = "".join(char if char.isalnum() or char in {"-", "_"} else "_" for char in workflow_id).strip("_")
        return self.root / f"{safe or 'workflow'}.json"


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()
