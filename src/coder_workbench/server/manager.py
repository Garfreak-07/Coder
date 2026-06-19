from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from queue import Empty, Queue
from threading import Lock, Thread
from typing import Any, Literal
from uuid import uuid4

from coder_workbench.core import WorkflowSpec
from coder_workbench.runtime import RunEvent, RunResult, run_workflow
from coder_workbench.server.storage import RunStore


LiveStatus = Literal["queued", "running", "completed", "blocked", "failed"]


@dataclass
class LiveRun:
    id: str
    workflow: WorkflowSpec
    repo_root: str
    request: str
    initial_data: dict[str, Any]
    status: LiveStatus = "queued"
    events: list[RunEvent] = field(default_factory=list)
    queue: Queue[RunEvent | None] = field(default_factory=Queue)
    result: RunResult | None = None
    stored_run_id: str | None = None
    error: str | None = None


class RunManager:
    def __init__(self, store: RunStore) -> None:
        self.store = store
        self._runs: dict[str, LiveRun] = {}
        self._lock = Lock()
        self._load_persisted_live_runs()

    def start(self, workflow: WorkflowSpec, repo_root: str, request: str, initial_data: dict[str, Any]) -> LiveRun:
        live = LiveRun(
            id=str(uuid4()),
            workflow=workflow,
            repo_root=repo_root,
            request=request,
            initial_data=dict(initial_data),
        )
        live.initial_data["run_id"] = live.id
        with self._lock:
            self._runs[live.id] = live
        self._persist_live(live)
        thread = Thread(target=self._execute, args=(live,), daemon=True)
        thread.start()
        return live

    def approve(
        self,
        run_id: str,
        approved: bool = True,
        data: dict[str, Any] | None = None,
        reason: str | None = None,
    ) -> LiveRun:
        run = self.get(run_id)
        if run.status != "blocked" or not run.result or not run.result.resume_checkpoint or not run.result.blocked_node_id:
            raise ValueError("run is not waiting for approval")

        checkpoint = dict(run.result.resume_checkpoint)
        checkpoint_data = dict(checkpoint.get("data", {}))
        checkpoint_data[f"{run.result.blocked_node_id}_approved"] = approved
        checkpoint_data.update(data or {})
        approval_record = self._approval_record(run, checkpoint_data, approved, reason)
        if approval_record["approval_type"] == "command" and approval_record.get("approval_key"):
            command_approvals = dict(checkpoint_data.get("command_approvals", {}))
            command_approvals[str(approval_record["approval_key"])] = approved
            checkpoint_data["command_approvals"] = command_approvals
        if approval_record["approval_type"] == "mcp_tool" and approval_record.get("approval_key"):
            mcp_approvals = dict(checkpoint_data.get("mcp_approvals", {}))
            mcp_approvals[str(approval_record["approval_key"])] = approved
            checkpoint_data["mcp_approvals"] = mcp_approvals

        approval_audit = list(checkpoint_data.get("approval_audit", []))
        approval_audit.append(approval_record)
        checkpoint_data["approval_audit"] = approval_audit

        blocked_node = run.workflow.node_by_id().get(run.result.blocked_node_id)
        if blocked_node and blocked_node.type == "human_gate":
            approval_key = blocked_node.output_key or blocked_node.id
            approval_value = dict(checkpoint_data.get(approval_key, {}))
            approval_value["approved"] = approved
            checkpoint_data[approval_key] = approval_value

        checkpoint["data"] = checkpoint_data

        run.initial_data = checkpoint_data
        run.status = "queued"
        run.error = None
        run.queue = Queue()
        approval_event = RunEvent(
            type="approval.recorded",
            node_id=run.result.blocked_node_id,
            message=f"Approval recorded for {approval_record['approval_type']}",
            payload=approval_record,
        )
        run.events.append(approval_event)
        run.queue.put(approval_event)
        if not approved:
            checkpoint["data"] = checkpoint_data
            run.initial_data = checkpoint_data
            run.status = "failed"
            run.error = reason or "approval rejected"
            failed_event = RunEvent(
                type="run.failed",
                node_id=run.result.blocked_node_id,
                message=run.error,
                payload={"approval": approval_record},
            )
            run.events.append(failed_event)
            run.queue.put(failed_event)
            self._persist_live(run)
            run.queue.put(None)
            return run

        prior_events = list(run.events)
        thread = Thread(target=self._execute, args=(run, checkpoint, run.result.blocked_node_id, prior_events), daemon=True)
        thread.start()
        return run

    def get(self, run_id: str) -> LiveRun:
        with self._lock:
            if run_id not in self._runs:
                raise KeyError(run_id)
            return self._runs[run_id]

    def list(self) -> list[dict[str, Any]]:
        with self._lock:
            return [
                {
                    "id": run.id,
                    "workflow_id": run.workflow.id,
                    "repo_root": run.repo_root,
                    "request": run.request,
                    "status": run.status,
                    "events": len(run.events),
                    "stored_run_id": run.stored_run_id,
                    "error": run.error,
                }
                for run in self._runs.values()
            ]

    def _load_persisted_live_runs(self) -> None:
        for payload in self.store.list_live():
            try:
                workflow = WorkflowSpec.model_validate(payload["workflow"])
                result_payload = payload.get("result")
                result = RunResult.model_validate(result_payload) if isinstance(result_payload, dict) else None
                events = [
                    RunEvent.model_validate(event)
                    for event in payload.get("events", [])
                    if isinstance(event, dict)
                ]
                status = payload.get("status", "failed")
                if status in {"queued", "running"}:
                    status = "failed"
                live = LiveRun(
                    id=str(payload["id"]),
                    workflow=workflow,
                    repo_root=str(payload["repo_root"]),
                    request=str(payload["request"]),
                    initial_data=dict(payload.get("initial_data", {})),
                    status=status,
                    events=events,
                    result=result,
                    stored_run_id=payload.get("stored_run_id"),
                    error=payload.get("error"),
                )
                self._runs[live.id] = live
            except Exception:
                continue

    def stream(self, run_id: str):
        run = self.get(run_id)
        sent = 0
        while sent < len(run.events):
            yield run.events[sent]
            sent += 1
        while True:
            try:
                event = run.queue.get(timeout=1)
            except Empty:
                if run.status not in {"queued", "running"}:
                    break
                continue
            if event is None:
                break
            yield event

    def _execute(
        self,
        run: LiveRun,
        resume_checkpoint: dict[str, Any] | None = None,
        resume_after_node: str | None = None,
        prior_events: list[RunEvent] | None = None,
    ) -> None:
        run.status = "running"

        def sink(event: RunEvent) -> None:
            run.events.append(event)
            run.queue.put(event)
            self._persist_live(run)

        try:
            result = run_workflow(
                workflow=run.workflow,
                request=run.request,
                repo_root=run.repo_root,
                initial_data=run.initial_data,
                event_sink=sink,
                resume_checkpoint=resume_checkpoint,
                prior_events=prior_events,
                resume_after_node=resume_after_node,
            )
            run.result = result
            run.status = result.status
            stored = self.store.save(
                workflow_id=run.workflow.id,
                repo_root=run.repo_root,
                request=run.request,
                result=result,
            )
            run.stored_run_id = stored.id
        except Exception as exc:  # pragma: no cover - background boundary
            run.status = "failed"
            run.error = str(exc)
        finally:
            self._persist_live(run)
            run.queue.put(None)

    def _approval_record(
        self,
        run: LiveRun,
        checkpoint_data: dict[str, Any],
        approved: bool,
        reason: str | None = None,
    ) -> dict[str, Any]:
        blocked_node_id = run.result.blocked_node_id if run.result else None
        blocked_value = checkpoint_data.get(blocked_node_id or "")
        if not isinstance(blocked_value, dict) and run.result and blocked_node_id:
            blocked_node = run.workflow.node_by_id().get(blocked_node_id)
            if blocked_node:
                blocked_value = checkpoint_data.get(blocked_node.output_key or blocked_node.id)
        if not isinstance(blocked_value, dict):
            blocked_value = {}

        approval_type = str(blocked_value.get("approval_type") or "human_gate")
        return {
            "run_id": run.id,
            "node_id": blocked_node_id,
            "approval_type": approval_type,
            "approved": approved,
            "approval_key": blocked_value.get("approval_key"),
            "command": blocked_value.get("command"),
            "cwd": blocked_value.get("cwd"),
            "reason": reason or blocked_value.get("reason") or blocked_value.get("message"),
            "actor": "local_user",
            "created_at": datetime.now(timezone.utc).isoformat(),
        }

    def _persist_live(self, run: LiveRun) -> None:
        self.store.save_live(
            {
                "id": run.id,
                "workflow": run.workflow.model_dump(mode="json", by_alias=True),
                "repo_root": run.repo_root,
                "request": run.request,
                "initial_data": run.initial_data,
                "status": run.status,
                "events": [event.model_dump(mode="json") for event in run.events],
                "result": run.result.model_dump(mode="json") if run.result else None,
                "stored_run_id": run.stored_run_id,
                "error": run.error,
            }
        )
