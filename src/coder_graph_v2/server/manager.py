from __future__ import annotations

from dataclasses import dataclass, field
from queue import Empty, Queue
from threading import Lock, Thread
from typing import Any, Literal
from uuid import uuid4

from coder_graph_v2.core import WorkflowSpec
from coder_graph_v2.runtime import RunEvent, RunResult, run_workflow
from coder_graph_v2.server.storage import RunStore


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

    def start(self, workflow: WorkflowSpec, repo_root: str, request: str, initial_data: dict[str, Any]) -> LiveRun:
        live = LiveRun(
            id=str(uuid4()),
            workflow=workflow,
            repo_root=repo_root,
            request=request,
            initial_data=initial_data,
        )
        with self._lock:
            self._runs[live.id] = live
        thread = Thread(target=self._execute, args=(live,), daemon=True)
        thread.start()
        return live

    def approve(self, run_id: str, approved: bool = True, data: dict[str, Any] | None = None) -> LiveRun:
        run = self.get(run_id)
        if run.status != "blocked" or not run.result or not run.result.resume_checkpoint or not run.result.blocked_node_id:
            raise ValueError("run is not waiting for approval")

        checkpoint = dict(run.result.resume_checkpoint)
        checkpoint_data = dict(checkpoint.get("data", {}))
        checkpoint_data["approved"] = approved
        checkpoint_data.update(data or {})

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
        thread = Thread(target=self._execute, args=(run, checkpoint, run.result.blocked_node_id, list(run.events)), daemon=True)
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
            run.queue.put(None)
