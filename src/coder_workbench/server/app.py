from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from fastapi import FastAPI, HTTPException
from fastapi.responses import StreamingResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.core import WorkflowSpec, load_workflow
from coder_workbench.runtime import run_workflow
from coder_workbench.server.library import LibraryStore
from coder_workbench.server.manager import RunManager
from coder_workbench.server.storage import RunStore
from coder_workbench.tools import default_tool_registry
from coder_workbench.tools.filesystem import normalize_scope_paths, resolve_existing_dir
from coder_workbench.tools.patching import rollback_patch


class RunRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    repo: str
    request: str
    workflow_path: str | None = None
    workflow: dict[str, Any] | None = None
    approved: bool = False
    scopes: list[str] = Field(default_factory=list)
    initial_data: dict[str, Any] = Field(default_factory=dict)


class ApprovalRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    approved: bool = True
    reason: str | None = None
    data: dict[str, Any] = Field(default_factory=dict)


class RollbackRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    repo: str
    snapshot_id: str
    scopes: list[str] = Field(default_factory=list)


def create_app(store_root: str | Path = ".coder", frontend_dist: str | Path | None = None) -> FastAPI:
    app = FastAPI(title="Coder Runtime API", version="0.1.0")
    store = RunStore(store_root)
    library = LibraryStore(Path(store_root) / "library")
    manager = RunManager(store)

    @app.get("/api/v2/health")
    def health() -> dict[str, Any]:
        return {
            "status": "ok",
            "tools": default_tool_registry().names(),
        }

    @app.get("/api/v2/library")
    def library_index() -> dict[str, Any]:
        return {
            "agents": library.list_agents(),
            "workflows": library.list_workflows(),
        }

    @app.post("/api/v2/library/agents")
    def save_agent(agent: dict[str, Any]) -> dict[str, Any]:
        return {"agent": library.save_agent(agent)}

    @app.get("/api/v2/library/agents/{agent_id}")
    def get_agent(agent_id: str) -> dict[str, Any]:
        try:
            return {"agent": library.get_agent(agent_id)}
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="agent not found") from exc

    @app.post("/api/v2/library/workflows")
    def save_workflow(workflow: dict[str, Any]) -> dict[str, Any]:
        return {"workflow": library.save_workflow(workflow)}

    @app.get("/api/v2/library/workflows/{workflow_id}")
    def get_workflow(workflow_id: str) -> dict[str, Any]:
        try:
            return {"workflow": library.get_workflow(workflow_id)}
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="workflow not found") from exc

    @app.get("/api/v2/runs")
    def list_runs() -> dict[str, Any]:
        return {"runs": store.list()}

    @app.get("/api/v2/live-runs")
    def list_live_runs() -> dict[str, Any]:
        return {"runs": manager.list()}

    @app.post("/api/v2/runs")
    def create_run(body: RunRequest) -> dict[str, Any]:
        workflow = _load_workflow_from_request(body)
        repo_root = resolve_existing_dir(body.repo)
        initial_data = _initial_data_from_request(body, repo_root)
        result = run_workflow(
            workflow=workflow,
            request=body.request,
            repo_root=str(repo_root),
            initial_data=initial_data,
        )
        stored = store.save(
            workflow_id=workflow.id,
            repo_root=str(repo_root),
            request=body.request,
            result=result,
        )
        return {
            "run_id": stored.id,
            "status": result.status,
            "agent_calls": result.agent_calls,
            "tool_calls": result.tool_calls,
            "estimated_tokens_used": result.estimated_tokens_used,
            "events_url": f"/api/v2/runs/{stored.id}/events",
        }

    @app.post("/api/v2/live-runs")
    def create_live_run(body: RunRequest) -> dict[str, Any]:
        workflow = _load_workflow_from_request(body)
        repo_root = resolve_existing_dir(body.repo)
        initial_data = _initial_data_from_request(body, repo_root)
        live = manager.start(
            workflow=workflow,
            repo_root=str(repo_root),
            request=body.request,
            initial_data=initial_data,
        )
        return {
            "run_id": live.id,
            "status": live.status,
            "events_url": f"/api/v2/live-runs/{live.id}/events",
            "result_url": f"/api/v2/live-runs/{live.id}",
        }

    @app.post("/api/v2/live-runs/{run_id}/approve")
    def approve_live_run(run_id: str, body: ApprovalRequest) -> dict[str, Any]:
        try:
            live = manager.approve(run_id, approved=body.approved, data=body.data, reason=body.reason)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="live run not found") from exc
        except ValueError as exc:
            raise HTTPException(status_code=409, detail=str(exc)) from exc
        return {
            "run_id": live.id,
            "status": live.status,
            "events_url": f"/api/v2/live-runs/{live.id}/events",
            "result_url": f"/api/v2/live-runs/{live.id}",
        }

    @app.post("/api/v2/patches/rollback")
    def rollback_patch_endpoint(body: RollbackRequest) -> dict[str, Any]:
        repo_root = resolve_existing_dir(body.repo)
        try:
            scopes = normalize_scope_paths(repo_root, body.scopes)
            result = rollback_patch(
                {"snapshot_id": body.snapshot_id},
                {"repo_root": str(repo_root), "scopes": scopes, "data": {}},
            )
        except (FileNotFoundError, NotADirectoryError, ValueError) as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return {"rollback": result}

    @app.get("/api/v2/runs/{run_id}")
    def get_run(run_id: str) -> dict[str, Any]:
        try:
            stored = store.get(run_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="run not found") from exc
        return stored.model_dump(mode="json")

    @app.get("/api/v2/live-runs/{run_id}")
    def get_live_run(run_id: str) -> dict[str, Any]:
        try:
            live = manager.get(run_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="live run not found") from exc
        return {
            "id": live.id,
            "workflow_id": live.workflow.id,
            "repo_root": live.repo_root,
            "request": live.request,
            "status": live.status,
            "events": [event.model_dump(mode="json") for event in live.events],
            "result": live.result.model_dump(mode="json") if live.result else None,
            "stored_run_id": live.stored_run_id,
            "error": live.error,
        }

    @app.get("/api/v2/runs/{run_id}/events")
    def stream_events(run_id: str) -> StreamingResponse:
        try:
            stored = store.get(run_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="run not found") from exc

        def event_stream():
            for event in stored.result.events:
                payload = json.dumps(event.model_dump(mode="json"), ensure_ascii=False)
                yield f"event: {event.type}\n"
                yield f"data: {payload}\n\n"

        return StreamingResponse(event_stream(), media_type="text/event-stream")

    @app.get("/api/v2/live-runs/{run_id}/events")
    def stream_live_events(run_id: str) -> StreamingResponse:
        try:
            manager.get(run_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="live run not found") from exc

        def event_stream():
            for event in manager.stream(run_id):
                payload = json.dumps(event.model_dump(mode="json"), ensure_ascii=False)
                yield f"event: {event.type}\n"
                yield f"data: {payload}\n\n"

        return StreamingResponse(event_stream(), media_type="text/event-stream")

    frontend_path = Path(frontend_dist) if frontend_dist else Path.cwd() / "frontend" / "dist"
    if frontend_path.exists():
        app.mount("/", StaticFiles(directory=frontend_path, html=True), name="frontend")

    return app


def _load_workflow_from_request(body: RunRequest) -> WorkflowSpec:
    if body.workflow:
        return WorkflowSpec.model_validate(body.workflow)
    if body.workflow_path:
        return load_workflow(body.workflow_path)
    raise HTTPException(status_code=400, detail="workflow or workflow_path is required")


def _initial_data_from_request(body: RunRequest, repo_root: Path) -> dict[str, Any]:
    try:
        scopes = normalize_scope_paths(repo_root, body.scopes)
    except (FileNotFoundError, NotADirectoryError, ValueError) as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    initial_data = dict(body.initial_data)
    initial_data.update({
        "request": body.request,
        "approved": body.approved,
        "preapprove_all": body.approved,
        "scopes": scopes,
    })
    return initial_data
