from __future__ import annotations

import hashlib
import json
import shutil
import sqlite3
import time
from pathlib import Path
from threading import Lock
from typing import Any
from uuid import uuid4

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.core.artifacts import artifact_summary
from coder_workbench.runtime import RunEvent, RunResult
from coder_workbench.server.storage_objects import (
    collect_blob_ids,
    context_packet_summary,
    embedded_context_packet,
    embedded_tool_result,
    externalize_large_values,
)


BLOB_STRING_THRESHOLD = 8192


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
    status_reason: str | None = None
    status_code: str | None = None


class RunStore:
    """Small file-backed store for workflow runs.

    This is intentionally simple. It gives the upcoming app a stable run/event
    API without committing to a database before the frontend shape is settled.
    """

    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)
        self.runs_dir = self.root / "runs"
        self.index_path = self.runs_dir / "index.sqlite"
        self.live_runs_dir = self.root / "live-runs"
        self.blobs_dir = self.root / "blobs"
        from coder_workbench.server.stores import PartitionedRunStores

        self.partitions = PartitionedRunStores(self.root)
        self.runs_dir.mkdir(parents=True, exist_ok=True)
        self.live_runs_dir.mkdir(parents=True, exist_ok=True)
        self.blobs_dir.mkdir(parents=True, exist_ok=True)
        self._lock = Lock()
        self._init_index()

    def save(self, workflow_id: str, repo_root: str, request: str, result: RunResult) -> StoredRun:
        stored = StoredRun(workflow_id=workflow_id, repo_root=repo_root, request=request, result=result)
        with self._lock:
            run_dir = self._run_dir(stored.id)
            run_dir.mkdir(parents=True, exist_ok=True)
            (run_dir / "contexts").mkdir(parents=True, exist_ok=True)
            (run_dir / "artifacts").mkdir(parents=True, exist_ok=True)
            (run_dir / "tool-results").mkdir(parents=True, exist_ok=True)
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
                status_reason=result.status_reason,
                status_code=result.status_code,
            )
            (run_dir / "metadata.json").write_text(metadata.model_dump_json(indent=2), encoding="utf-8")
            self._upsert_index(metadata, updated_at=time.time())
            result_payload = result.model_dump(mode="json")
            result_payload["events"] = []
            result_payload["artifacts"] = self._write_artifacts(run_dir, result.artifacts)
            self._write_ledgers(run_dir, result_payload.get("data", {}).get("token_ledger"))
            (run_dir / "result.json").write_text(json.dumps(result_payload, indent=2), encoding="utf-8")
            events = self._externalize_context_packets(run_dir, result.events)
            events = self._externalize_tool_results(run_dir, events)
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
        indexed = self._list_from_index()
        if indexed:
            return indexed

        runs = self._scan_run_metadata()
        if runs:
            self._rebuild_index(runs)
        return runs

    def delete(self, run_id: str) -> dict[str, Any]:
        with self._lock:
            run_dir = self._run_dir(run_id)
            legacy_path = self._legacy_path(run_id)
            if run_dir.exists():
                candidate_blob_ids = self._blob_ids_in_path(run_dir)
                shutil.rmtree(run_dir)
            elif legacy_path.exists():
                candidate_blob_ids = self._blob_ids_in_path(legacy_path)
                legacy_path.unlink()
            else:
                raise KeyError(run_id)
            self._delete_index(self._safe_run_id(run_id))
            removed_blobs = self.cleanup_orphan_blobs(candidate_blob_ids)
        return {
            "run_id": run_id,
            "deleted": True,
            "orphan_blobs_removed": removed_blobs,
        }

    def cleanup_orphan_blobs(self, candidate_blob_ids: set[str] | None = None) -> int:
        referenced = self._referenced_blob_ids()
        candidates = candidate_blob_ids if candidate_blob_ids is not None else self._stored_blob_ids()
        removed = 0
        for blob_id in candidates:
            if blob_id in referenced:
                continue
            try:
                path = self._blob_path(blob_id)
            except KeyError:
                continue
            if not path.exists():
                continue
            path.unlink()
            self._prune_empty_blob_parents(path.parent)
            removed += 1
        return removed

    def _scan_run_metadata(self) -> list[dict[str, Any]]:
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
                    "status_reason": stored.result.status_reason,
                    "status_code": stored.result.status_code,
                }
            )
        return runs

    def _init_index(self) -> None:
        with sqlite3.connect(self.index_path) as connection:
            connection.execute(
                """
                CREATE TABLE IF NOT EXISTS runs (
                    id TEXT PRIMARY KEY,
                    workflow_id TEXT NOT NULL,
                    repo_root TEXT NOT NULL,
                    request TEXT NOT NULL,
                    status TEXT NOT NULL,
                    events INTEGER NOT NULL,
                    agent_calls INTEGER NOT NULL,
                    tool_calls INTEGER NOT NULL,
                    estimated_tokens_used INTEGER NOT NULL,
                    updated_at REAL NOT NULL
                )
                """
            )
            self._ensure_index_columns(connection)

    def _upsert_index(self, metadata: StoredRunMetadata, *, updated_at: float) -> None:
        with sqlite3.connect(self.index_path) as connection:
            connection.execute(
                """
                INSERT INTO runs (
                    id, workflow_id, repo_root, request, status, events,
                    agent_calls, tool_calls, estimated_tokens_used,
                    status_reason, status_code, updated_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                    workflow_id=excluded.workflow_id,
                    repo_root=excluded.repo_root,
                    request=excluded.request,
                    status=excluded.status,
                    events=excluded.events,
                    agent_calls=excluded.agent_calls,
                    tool_calls=excluded.tool_calls,
                    estimated_tokens_used=excluded.estimated_tokens_used,
                    status_reason=excluded.status_reason,
                    status_code=excluded.status_code,
                    updated_at=excluded.updated_at
                """,
                (
                    metadata.id,
                    metadata.workflow_id,
                    metadata.repo_root,
                    metadata.request,
                    metadata.status,
                    metadata.events,
                    metadata.agent_calls,
                    metadata.tool_calls,
                    metadata.estimated_tokens_used,
                    metadata.status_reason,
                    metadata.status_code,
                    updated_at,
                ),
            )

    def _ensure_index_columns(self, connection: sqlite3.Connection) -> None:
        columns = {row[1] for row in connection.execute("PRAGMA table_info(runs)").fetchall()}
        if "status_reason" not in columns:
            connection.execute("ALTER TABLE runs ADD COLUMN status_reason TEXT")
        if "status_code" not in columns:
            connection.execute("ALTER TABLE runs ADD COLUMN status_code TEXT")

    def _delete_index(self, run_id: str) -> None:
        with sqlite3.connect(self.index_path) as connection:
            connection.execute("DELETE FROM runs WHERE id = ?", (run_id,))

    def _list_from_index(self) -> list[dict[str, Any]]:
        with sqlite3.connect(self.index_path) as connection:
            connection.row_factory = sqlite3.Row
            rows = connection.execute(
                """
                SELECT id, workflow_id, repo_root, request, status, events,
                       agent_calls, tool_calls, estimated_tokens_used,
                       status_reason, status_code
                FROM runs
                ORDER BY updated_at DESC
                """
            ).fetchall()
        return [dict(row) for row in rows]

    def _rebuild_index(self, runs: list[dict[str, Any]]) -> None:
        with sqlite3.connect(self.index_path) as connection:
            connection.execute("DELETE FROM runs")
            for index, run in enumerate(reversed(runs)):
                connection.execute(
                    """
                    INSERT OR REPLACE INTO runs (
                        id, workflow_id, repo_root, request, status, events,
                        agent_calls, tool_calls, estimated_tokens_used,
                        status_reason, status_code, updated_at
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        run["id"],
                        run["workflow_id"],
                        run["repo_root"],
                        run["request"],
                        run["status"],
                        int(run["events"]),
                        int(run["agent_calls"]),
                        int(run["tool_calls"]),
                        int(run["estimated_tokens_used"]),
                        run.get("status_reason"),
                        run.get("status_code"),
                        time.time() + index,
                    ),
                )

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

    def get_artifact(self, run_id: str, artifact_id: str) -> dict[str, Any]:
        safe_artifact_id = self._safe_object_id(artifact_id)
        run_dir = self._run_dir(run_id)
        if run_dir.exists():
            path = run_dir / "artifacts" / f"{safe_artifact_id}.json"
            if path.exists():
                return json.loads(path.read_text(encoding="utf-8"))
            raise KeyError(artifact_id)

        path = self._legacy_path(run_id)
        if not path.exists():
            raise KeyError(run_id)
        stored = StoredRun.model_validate(json.loads(path.read_text(encoding="utf-8")))
        artifact = stored.result.artifacts.get(safe_artifact_id)
        if isinstance(artifact, dict):
            return artifact
        raise KeyError(artifact_id)

    def get_tool_result(self, run_id: str, tool_result_id: str) -> dict[str, Any]:
        safe_tool_result_id = self._safe_object_id(tool_result_id)
        run_dir = self._run_dir(run_id)
        if run_dir.exists():
            path = run_dir / "tool-results" / f"{safe_tool_result_id}.json"
            if path.exists():
                return json.loads(path.read_text(encoding="utf-8"))
            for event in self._read_events(run_dir / "events.jsonl"):
                result = self._embedded_tool_result(event, safe_tool_result_id)
                if result is not None:
                    return result
            raise KeyError(tool_result_id)

        path = self._legacy_path(run_id)
        if not path.exists():
            raise KeyError(run_id)
        stored = StoredRun.model_validate(json.loads(path.read_text(encoding="utf-8")))
        for event in stored.result.events:
            result = self._embedded_tool_result(event, safe_tool_result_id)
            if result is not None:
                return result
        raise KeyError(tool_result_id)

    def get_blob(self, blob_id: str) -> dict[str, Any]:
        path = self._blob_path(blob_id)
        if not path.exists():
            raise KeyError(blob_id)
        content = path.read_text(encoding="utf-8")
        return {
            "blob_id": blob_id,
            "size_bytes": path.stat().st_size,
            "media_type": "text/plain; charset=utf-8",
            "content": content,
        }

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

    def _safe_blob_id(self, blob_id: str) -> str:
        prefix = "sha256:"
        if not blob_id.startswith(prefix):
            raise KeyError(blob_id)
        digest = blob_id[len(prefix):]
        if len(digest) != 64 or any(char not in "0123456789abcdef" for char in digest):
            raise KeyError(blob_id)
        return digest

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

    def _write_artifacts(self, run_dir: Path, artifacts: dict[str, Any]) -> dict[str, Any]:
        artifact_dir = run_dir / "artifacts"
        artifact_dir.mkdir(parents=True, exist_ok=True)
        refs: dict[str, Any] = {}
        for raw_artifact_id, artifact in artifacts.items():
            try:
                artifact_id = self._safe_object_id(str(raw_artifact_id))
            except KeyError:
                continue
            if not isinstance(artifact, dict):
                continue
            stored_artifact = self._externalize_large_values(artifact)
            artifact_json = json.dumps(stored_artifact, ensure_ascii=False, indent=2)
            (artifact_dir / f"{artifact_id}.json").write_text(artifact_json, encoding="utf-8")
            summary = artifact_summary(artifact)
            refs[artifact_id] = {
                "artifact_id": artifact_id,
                "artifact_type": artifact.get("artifact_type"),
                "summary": summary,
                "size_chars": len(json.dumps(artifact, ensure_ascii=False)),
            }
        return refs

    def _write_ledgers(self, run_dir: Path, ledger_entries: Any) -> None:
        if not isinstance(ledger_entries, list):
            return
        ledger_dir = run_dir / "ledgers"
        ledger_dir.mkdir(parents=True, exist_ok=True)
        for index, entry in enumerate(ledger_entries, start=1):
            if not isinstance(entry, dict):
                continue
            ledger_id = self._safe_object_id(str(entry.get("ledger_id") or f"token_ledger_{index}"))
            (ledger_dir / f"{ledger_id}.json").write_text(
                json.dumps(entry, ensure_ascii=False, indent=2),
                encoding="utf-8",
            )

    def _externalize_large_values(self, value: Any) -> Any:
        return externalize_large_values(value, write_blob=self._write_blob, threshold=BLOB_STRING_THRESHOLD)

    def _write_blob(self, content: str) -> str:
        digest = hashlib.sha256(content.encode("utf-8")).hexdigest()
        blob_id = f"sha256:{digest}"
        path = self._blob_path(blob_id)
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists():
            path.write_text(content, encoding="utf-8")
        return blob_id

    def _blob_path(self, blob_id: str) -> Path:
        digest = self._safe_blob_id(blob_id)
        return self.blobs_dir / "sha256" / digest[:2] / f"sha256-{digest}"

    def _stored_blob_ids(self) -> set[str]:
        blob_ids: set[str] = set()
        root = self.blobs_dir / "sha256"
        if not root.exists():
            return blob_ids
        for path in root.glob("*/*"):
            name = path.name
            if not path.is_file() or not name.startswith("sha256-"):
                continue
            digest = name.removeprefix("sha256-")
            if len(digest) == 64:
                blob_ids.add(f"sha256:{digest}")
        return blob_ids

    def _referenced_blob_ids(self) -> set[str]:
        blob_ids: set[str] = set()
        run_items = self.runs_dir.iterdir() if self.runs_dir.exists() else []
        for run_dir in run_items:
            if run_dir.is_dir():
                blob_ids.update(self._blob_ids_in_path(run_dir))
            elif run_dir.is_file() and run_dir.suffix == ".json":
                blob_ids.update(self._blob_ids_in_path(run_dir))
        return blob_ids

    def _blob_ids_in_path(self, path: Path) -> set[str]:
        blob_ids: set[str] = set()
        paths = [path] if path.is_file() else list(path.rglob("*"))
        for item in paths:
            if not item.is_file() or item.suffix not in {".json", ".jsonl"}:
                continue
            try:
                text = item.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                continue
            if item.suffix == ".jsonl":
                for line in text.splitlines():
                    if not line.strip():
                        continue
                    try:
                        self._collect_blob_ids(json.loads(line), blob_ids)
                    except json.JSONDecodeError:
                        continue
                continue
            try:
                self._collect_blob_ids(json.loads(text), blob_ids)
            except json.JSONDecodeError:
                continue
        return blob_ids

    def _collect_blob_ids(self, value: Any, blob_ids: set[str]) -> None:
        collect_blob_ids(value, blob_ids, validate_blob_id=self._safe_blob_id)

    def _prune_empty_blob_parents(self, path: Path) -> None:
        current = path
        while current != self.blobs_dir and self.blobs_dir in current.parents:
            try:
                current.rmdir()
            except OSError:
                break
            current = current.parent

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

    def _externalize_tool_results(self, run_dir: Path, events: list[RunEvent]) -> list[RunEvent]:
        tool_result_dir = run_dir / "tool-results"
        tool_result_dir.mkdir(parents=True, exist_ok=True)
        compact_events: list[RunEvent] = []
        for event in events:
            if event.type != "tool.result":
                compact_events.append(event)
                continue

            result = event.payload.get("result")
            if result is None:
                compact_events.append(event)
                continue

            raw_tool_result_id = str(event.payload.get("tool_result_id") or event.id)
            try:
                tool_result_id = self._safe_object_id(raw_tool_result_id)
            except KeyError:
                tool_result_id = self._safe_object_id(event.id)
            stored_result = self._externalize_large_values(result)
            result_json = json.dumps(stored_result, ensure_ascii=False, indent=2)
            (tool_result_dir / f"{tool_result_id}.json").write_text(result_json, encoding="utf-8")
            compact_payload = {
                "tool_result_id": tool_result_id,
                "tool": event.payload.get("tool"),
                "result_summary": event.payload.get("result_summary"),
                "result_status": event.payload.get("result_status"),
                "result_keys": event.payload.get("result_keys"),
                "result_size_chars": len(json.dumps(result, ensure_ascii=False)),
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
        return embedded_context_packet(event, packet_id)

    def _embedded_tool_result(self, event: RunEvent, tool_result_id: str) -> dict[str, Any] | None:
        return embedded_tool_result(event, tool_result_id)

    def _context_packet_summary(self, packet: Any) -> dict[str, Any]:
        return context_packet_summary(packet)

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
