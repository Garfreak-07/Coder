from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import TYPE_CHECKING, Any

from coder_workbench.memory.models import (
    KnowledgeChunk,
    KnowledgeSource,
    MemoryRecord,
    validate_knowledge_chunk,
    validate_memory_record,
)
from coder_workbench.memory.schema import WorkflowMemoryCollection

if TYPE_CHECKING:
    from coder_workbench.agent_graph.memory import WorkflowMemory


class WorkflowMemoryStore:
    """Workflow memory adapter over the existing PlannerMemoryStore layout."""

    def __init__(self, repo_root: str | Path) -> None:
        from coder_workbench.agent_graph.memory import PlannerMemoryStore

        self._legacy = PlannerMemoryStore(repo_root)

    def load_workflow_memory(self, workflow_id: str) -> WorkflowMemory:
        return self._legacy.load_workflow_memory(workflow_id)

    def save_workflow_memory(self, memory: WorkflowMemory) -> None:
        self._legacy.save_workflow_memory(memory)

    def append_entry(
        self,
        *,
        workflow_id: str,
        collection: WorkflowMemoryCollection,
        entry: dict[str, Any],
    ) -> WorkflowMemory:
        memory = self.load_workflow_memory(workflow_id)
        getattr(memory, collection).append(dict(entry))
        memory.updated_at = _now()
        self.save_workflow_memory(memory)
        return memory


class AgentScopedMemoryStore:
    """Append-only store for Batch D agent-scoped MemoryRecord objects."""

    def __init__(self, root: str | Path) -> None:
        self.root = _memory_root(root)
        self.records_path = self.root / "records.jsonl"
        self.tombstones_path = self.root / "tombstones.jsonl"
        self.root.mkdir(parents=True, exist_ok=True)

    def append_record(self, record: MemoryRecord) -> MemoryRecord:
        parsed = validate_memory_record(record)
        if self._latest_record(parsed.id) is not None:
            raise ValueError(f"memory record already exists: {parsed.id}")
        self._append_jsonl(self.records_path, parsed.model_dump(mode="json"))
        return parsed

    def get_record(self, record_id: str) -> MemoryRecord:
        record = self._latest_record(record_id)
        if record is None or record.status == "forgotten":
            raise KeyError(record_id)
        return record

    def list_records(self, *, scope: str | None = None, status: str = "active") -> list[MemoryRecord]:
        records = list(self._latest_records().values())
        if scope is not None:
            records = [record for record in records if record.scope == scope]
        if status is not None:
            records = [record for record in records if record.status == status]
        return sorted(records, key=lambda record: record.updated_at, reverse=True)

    def supersede_record(self, old_id: str, new_record: MemoryRecord) -> MemoryRecord:
        old = self.get_record(old_id)
        parsed = validate_memory_record(new_record)
        if self._latest_record(parsed.id) is not None:
            raise ValueError(f"memory record already exists: {parsed.id}")
        updated_old = old.model_copy(update={"status": "superseded", "updated_at": parsed.updated_at})
        supersedes = list(dict.fromkeys([*parsed.supersedes, old_id]))
        parsed = parsed.model_copy(update={"supersedes": supersedes})
        self._append_jsonl(self.records_path, updated_old.model_dump(mode="json"))
        self._append_jsonl(self.records_path, parsed.model_dump(mode="json"))
        return parsed

    def forget_record(self, record_id: str, *, hard: bool = False) -> None:
        record = self.get_record(record_id)
        self._append_jsonl(
            self.tombstones_path,
            {
                "record_id": record_id,
                "hard": hard,
                "created_at": _now(),
            },
        )
        if hard:
            kept = [
                item
                for item in self._read_jsonl(self.records_path)
                if item.get("id") != record_id
            ]
            self._write_jsonl(self.records_path, kept)
            return
        forgotten = record.model_copy(update={"status": "forgotten", "updated_at": _now()})
        self._append_jsonl(self.records_path, forgotten.model_dump(mode="json"))

    def _latest_record(self, record_id: str) -> MemoryRecord | None:
        return self._latest_records().get(record_id)

    def _latest_records(self) -> dict[str, MemoryRecord]:
        records: dict[str, MemoryRecord] = {}
        for item in self._read_jsonl(self.records_path):
            try:
                record = MemoryRecord.model_validate(item)
                validate_memory_record(record)
            except Exception as exc:
                raise ValueError(f"invalid memory record in {self.records_path}: {exc}") from exc
            records[record.id] = record
        return records

    def _append_jsonl(self, path: Path, item: dict[str, Any]) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(item, ensure_ascii=False, sort_keys=True))
            handle.write("\n")

    def _write_jsonl(self, path: Path, items: list[dict[str, Any]]) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("w", encoding="utf-8") as handle:
            for item in items:
                handle.write(json.dumps(item, ensure_ascii=False, sort_keys=True))
                handle.write("\n")

    def _read_jsonl(self, path: Path) -> list[dict[str, Any]]:
        if not path.exists():
            return []
        rows: list[dict[str, Any]] = []
        for line in path.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            value = json.loads(line)
            if not isinstance(value, dict):
                raise ValueError(f"JSONL row must be an object: {path}")
            rows.append(value)
        return rows


class KnowledgeStore:
    """Append-only store for importable knowledge sources and chunks."""

    def __init__(self, root: str | Path) -> None:
        self.root = _memory_root(root)
        self.sources_path = self.root / "knowledge_sources.jsonl"
        self.chunks_path = self.root / "knowledge_chunks.jsonl"
        self.root.mkdir(parents=True, exist_ok=True)

    def append_source(self, source: KnowledgeSource) -> KnowledgeSource:
        parsed = KnowledgeSource.model_validate(source)
        if any(item.source_id == parsed.source_id for item in self.list_sources()):
            raise ValueError(f"knowledge source already exists: {parsed.source_id}")
        self._append_jsonl(self.sources_path, parsed.model_dump(mode="json"))
        return parsed

    def append_chunk(self, chunk: KnowledgeChunk) -> KnowledgeChunk:
        parsed = validate_knowledge_chunk(chunk)
        if any(item.chunk_id == parsed.chunk_id for item in self.list_chunks()):
            raise ValueError(f"knowledge chunk already exists: {parsed.chunk_id}")
        self._append_jsonl(self.chunks_path, parsed.model_dump(mode="json"))
        return parsed

    def list_sources(self) -> list[KnowledgeSource]:
        sources: list[KnowledgeSource] = []
        for item in self._read_jsonl(self.sources_path):
            sources.append(KnowledgeSource.model_validate(item))
        return sources

    def list_chunks(self, *, source_id: str | None = None) -> list[KnowledgeChunk]:
        chunks: list[KnowledgeChunk] = []
        for item in self._read_jsonl(self.chunks_path):
            chunk = KnowledgeChunk.model_validate(item)
            validate_knowledge_chunk(chunk)
            if source_id is None or chunk.source_id == source_id:
                chunks.append(chunk)
        return chunks

    def _append_jsonl(self, path: Path, item: dict[str, Any]) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(item, ensure_ascii=False, sort_keys=True))
            handle.write("\n")

    def _read_jsonl(self, path: Path) -> list[dict[str, Any]]:
        if not path.exists():
            return []
        rows: list[dict[str, Any]] = []
        for line in path.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            value = json.loads(line)
            if not isinstance(value, dict):
                raise ValueError(f"JSONL row must be an object: {path}")
            rows.append(value)
        return rows


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _memory_root(root: str | Path) -> Path:
    path = Path(root)
    return path if path.name == "memory" else path / "memory"
