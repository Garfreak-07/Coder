from __future__ import annotations

import json
from pathlib import Path
from threading import Lock
from typing import Any
from uuid import uuid4

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.runtime import RunEvent, RunResult


class StoredRun(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str = Field(default_factory=lambda: str(uuid4()))
    workflow_id: str
    repo_root: str
    request: str
    result: RunResult


class StoredRunMetadata(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    workflow_id: str
    repo_root: str
    request: str
    status: str
    events: int
    agent_calls: int
    tool_calls: int
    estimated_tokens_used: int


class RunStore:
    """Small file-backed store for workflow runs.

    This is intentionally simple. It gives the upcoming app a stable run/event
    API without committing to a database before the frontend shape is settled.
    """

    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)
        self.runs_dir = self.root / "runs"
        self.live_runs_dir = self.root / "live-runs"
        self.runs_dir.mkdir(parents=True, exist_ok=True)
        self.live_runs_dir.mkdir(parents=True, exist_ok=True)
        self._lock = Lock()

    def save(self, workflow_id: str, repo_root: str, request: str, result: RunResult) -> StoredRun:
        stored = StoredRun(workflow_id=workflow_id, repo_root=repo_root, request=request, result=result)
        with self._lock:
            run_dir = self._run_dir(stored.id)
            run_dir.mkdir(parents=True, exist_ok=True)
            (run_dir / "contexts").mkdir(parents=True, exist_ok=True)
            metadata = StoredRunMetadata(
                id=stored.id,
                workflow_id=workflow_id,
                repo_root=repo_root,
                request=request,
                status=result.status,
                events=len(result.events),
                agent_calls=result.agent_calls,
                tool_calls=result.tool_calls,
                estimated_tokens_used=result.estimated_tokens_used,
            )
            (run_dir / "metadata.json").write_text(metadata.model_dump_json(indent=2), encoding="utf-8")
            result_payload = result.model_dump(mode="json")
            result_payload["events"] = []
            (run_dir / "result.json").write_text(json.dumps(result_payload, indent=2), encoding="utf-8")
            events = self._externalize_context_packets(run_dir, result.events)
            self._write_events(run_dir / "events.jsonl", events)
        return stored

    def get(self, run_id: str, *, include_events: bool = True) -> StoredRun:
        run_dir = self._run_dir(run_id)
        if run_dir.exists():
            return self._read_split_run(run_dir, include_events=include_events)

        path = self._legacy_path(run_id)
        if not path.exists():
            raise KeyError(run_id)
        stored = StoredRun.model_validate(json.loads(path.read_text(encoding="utf-8")))
        if include_events:
            return stored
        result_payload = stored.result.model_dump(mode="json")
        result_payload["events"] = []
        return StoredRun(
            id=stored.id,
            workflow_id=stored.workflow_id,
            repo_root=stored.repo_root,
            request=stored.request,
            result=RunResult.model_validate(result_payload),
        )

    def list(self) -> list[dict[str, Any]]:
        runs: list[dict[str, Any]] = []
        items = list(self.runs_dir.glob("*/metadata.json")) + list(self.runs_dir.glob("*.json"))
        for path in sorted(items, key=lambda item: item.stat().st_mtime, reverse=True):
            try:
                if path.name == "metadata.json":
                    metadata = StoredRunMetadata.model_validate(json.loads(path.read_text(encoding="utf-8")))
                    runs.append(metadata.model_dump(mode="json"))
                    continue
                stored = StoredRun.model_validate(json.loads(path.read_text(encoding="utf-8")))
            except Exception:
                continue
            runs.append(
                {
                    "id": stored.id,
                    "workflow_id": stored.workflow_id,
                    "repo_root": stored.repo_root,
                    "request": stored.request,
                    "status": stored.result.status,
                    "events": len(stored.result.events),
                    "agent_calls": stored.result.agent_calls,
                    "tool_calls": stored.result.tool_calls,
                    "estimated_tokens_used": stored.result.estimated_tokens_used,
                }
            )
        return runs

    def get_events(self, run_id: str, *, cursor: int = 0, limit: int | None = None) -> dict[str, Any]:
        if cursor < 0:
            raise ValueError("cursor must be greater than or equal to zero")
        if limit is not None and limit < 1:
            raise ValueError("limit must be greater than zero")

        run_dir = self._run_dir(run_id)
        if run_dir.exists():
            all_events = self._read_events(run_dir / "events.jsonl")
        else:
            stored = self.get(run_id)
            all_events = stored.result.events

        end = None if limit is None else cursor + limit
        events = all_events[cursor:end]
        next_cursor = cursor + len(events)
        return {
            "events": [event.model_dump(mode="json") for event in events],
            "cursor": cursor,
            "next_cursor": next_cursor,
            "has_more": next_cursor < len(all_events),
        }

    def get_context_packet(self, run_id: str, packet_id: str) -> dict[str, Any]:
        safe_packet_id = self._safe_object_id(packet_id)
        run_dir = self._run_dir(run_id)
        if run_dir.exists():
            path = run_dir / "contexts" / f"{safe_packet_id}.json"
            if path.exists():
                return json.loads(path.read_text(encoding="utf-8"))
            for event in self._read_events(run_dir / "events.jsonl"):
                packet = self._embedded_context_packet(event, safe_packet_id)
                if packet is not None:
                    return packet
            raise KeyError(packet_id)

        path = self._legacy_path(run_id)
        if not path.exists():
            raise KeyError(run_id)
        stored = StoredRun.model_validate(json.loads(path.read_text(encoding="utf-8")))
        for event in stored.result.events:
            packet = self._embedded_context_packet(event, safe_packet_id)
            if packet is not None:
                return packet
        raise KeyError(packet_id)

    def _safe_run_id(self, run_id: str) -> str:
        safe = "".join(char for char in run_id if char.isalnum() or char in {"-", "_"})
        if not safe:
            raise KeyError(run_id)
        return safe

    def _safe_object_id(self, object_id: str) -> str:
        safe = "".join(char for char in object_id if char.isalnum() or char in {"-", "_"})
        if not safe or safe != object_id:
            raise KeyError(object_id)
        return safe

    def _run_dir(self, run_id: str) -> Path:
        return self.runs_dir / self._safe_run_id(run_id)

    def _legacy_path(self, run_id: str) -> Path:
        return self.runs_dir / f"{self._safe_run_id(run_id)}.json"

    def _read_split_run(self, run_dir: Path, *, include_events: bool) -> StoredRun:
        metadata = StoredRunMetadata.model_validate(json.loads((run_dir / "metadata.json").read_text(encoding="utf-8")))
        result_payload = json.loads((run_dir / "result.json").read_text(encoding="utf-8"))
        result_payload["events"] = [
            event.model_dump(mode="json")
            for event in self._read_events(run_dir / "events.jsonl")
        ] if include_events else []
        return StoredRun(
            id=metadata.id,
            workflow_id=metadata.workflow_id,
            repo_root=metadata.repo_root,
            request=metadata.request,
            result=RunResult.model_validate(result_payload),
        )

    def _write_events(self, path: Path, events: list[RunEvent]) -> None:
        lines = [json.dumps(event.model_dump(mode="json"), ensure_ascii=False) for event in events]
        path.write_text(("\n".join(lines) + "\n") if lines else "", encoding="utf-8")

    def _externalize_context_packets(self, run_dir: Path, events: list[RunEvent]) -> list[RunEvent]:
        context_dir = run_dir / "contexts"
        context_dir.mkdir(parents=True, exist_ok=True)
        compact_events: list[RunEvent] = []
        for event in events:
            if event.type != "agent.context_packet":
                compact_events.append(event)
                continue

            packet = event.payload.get("packet")
            if packet is None:
                compact_events.append(event)
                continue

            raw_packet_id = str(event.payload.get("packet_id") or event.id)
            try:
                packet_id = self._safe_object_id(raw_packet_id)
            except KeyError:
                packet_id = self._safe_object_id(event.id)
            packet_json = json.dumps(packet, ensure_ascii=False, indent=2)
            (context_dir / f"{packet_id}.json").write_text(packet_json, encoding="utf-8")
            compact_payload = {
                "packet_id": packet_id,
                "summary": self._context_packet_summary(packet),
                "size_chars": len(json.dumps(packet, ensure_ascii=False)),
            }
            compact_events.append(event.model_copy(update={"payload": compact_payload}))
        return compact_events

    def _read_events(self, path: Path) -> list[RunEvent]:
        if not path.exists():
            return []
        events = []
        for line in path.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            events.append(RunEvent.model_validate(json.loads(line)))
        return events

    def _embedded_context_packet(self, event: RunEvent, packet_id: str) -> dict[str, Any] | None:
        if event.type != "agent.context_packet":
            return None
        if str(event.payload.get("packet_id") or event.id) != packet_id:
            return None
        packet = event.payload.get("packet")
        return packet if isinstance(packet, dict) else None

    def _context_packet_summary(self, packet: Any) -> dict[str, Any]:
        if not isinstance(packet, dict):
            return {"type": type(packet).__name__}

        agent = packet.get("agent") if isinstance(packet.get("agent"), dict) else {}
        token_estimate = packet.get("token_estimate") if isinstance(packet.get("token_estimate"), dict) else {}
        loop = packet.get("loop") if isinstance(packet.get("loop"), dict) else {}
        selected_state_keys = packet.get("selected_state_keys")
        state_summaries = packet.get("state_summaries")
        allowed_tools = packet.get("allowed_tools")

        summary = {
            "agent_id": agent.get("id"),
            "agent_name": agent.get("name"),
            "node_id": packet.get("node_id"),
            "selected_state_keys": selected_state_keys if isinstance(selected_state_keys, list) else [],
            "state_summary_keys": sorted(state_summaries.keys()) if isinstance(state_summaries, dict) else [],
            "tool_count": len(allowed_tools) if isinstance(allowed_tools, list) else 0,
            "estimated_tokens": token_estimate.get("packet"),
            "budget": token_estimate.get("budget"),
            "loop_node_id": loop.get("node_id"),
            "loop_iteration": loop.get("iteration"),
        }
        return {key: value for key, value in summary.items() if value is not None}

    def save_live(self, payload: dict[str, Any]) -> None:
        run_id = str(payload.get("id") or "")
        if not run_id:
            raise ValueError("live run payload requires id")
        with self._lock:
            self._live_path(run_id).write_text(json.dumps(payload, indent=2), encoding="utf-8")

    def list_live(self) -> list[dict[str, Any]]:
        items: list[dict[str, Any]] = []
        for path in sorted(self.live_runs_dir.glob("*.json"), key=lambda item: item.stat().st_mtime, reverse=True):
            try:
                payload = json.loads(path.read_text(encoding="utf-8"))
            except Exception:
                continue
            if isinstance(payload, dict):
                items.append(payload)
        return items

    def _live_path(self, run_id: str) -> Path:
        safe = "".join(char for char in run_id if char.isalnum() or char in {"-", "_"})
        return self.live_runs_dir / f"{safe}.json"
