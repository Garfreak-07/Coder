from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field


WorkItemMemoryState = Literal["pending", "running", "completed", "blocked", "failed", "skipped"]
RunMemoryState = Literal["running", "completed", "blocked", "failed"]


BANNED_SNAPSHOT_KEYS = {
    "raw_logs",
    "raw_log",
    "raw_events",
    "raw_prompts",
    "raw_prompt",
    "raw_model_outputs",
    "raw_model_output",
    "raw_runtime_json",
    "terminal_log",
    "full_diff",
    "full_diffs",
    "full_prompt",
    "model_output",
    "prompt",
}


class WorkItemMemoryStatus(BaseModel):
    model_config = ConfigDict(extra="forbid")

    work_item_id: str
    agent_id: str | None = None
    status: WorkItemMemoryState
    summary: str | None = None
    execution_result_ref: str | None = None
    evidence_refs: list[str] = Field(default_factory=list)


class RunMemorySnapshot(BaseModel):
    model_config = ConfigDict(extra="forbid")

    run_id: str
    planner_chat_session_id: str | None = None
    workflow_id: str
    status: RunMemoryState

    current_round: int
    planner_task_state: dict[str, Any] = Field(default_factory=dict)
    planner_order_ref: str | None = None
    planner_decision_ref: str | None = None
    final_report_ref: str | None = None

    work_items: list[WorkItemMemoryStatus] = Field(default_factory=list)
    execution_result_summaries: list[dict[str, Any]] = Field(default_factory=list)
    verification_summaries: list[dict[str, Any]] = Field(default_factory=list)
    blocked_reasons: list[str] = Field(default_factory=list)
    changed_files_summary: dict[str, Any] = Field(default_factory=dict)

    evidence_refs: list[str] = Field(default_factory=list)
    diff_refs: list[str] = Field(default_factory=list)
    log_refs: list[str] = Field(default_factory=list)
    native_event_refs: list[str] = Field(default_factory=list)

    unresolved_items: list[str] = Field(default_factory=list)
    next_recommended_action: str | None = None

    created_at: str
    updated_at: str


class RunMemoryStore:
    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)

    def write_checkpoint(self, snapshot: RunMemorySnapshot, *, phase: str) -> RunMemorySnapshot:
        run_dir = self._run_memory_dir(snapshot.run_id)
        run_dir.mkdir(parents=True, exist_ok=True)
        payload = {
            "phase": phase,
            "created_at": _now(),
            "snapshot": snapshot.model_dump(mode="json"),
        }
        with (run_dir / "checkpoints.jsonl").open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(payload, ensure_ascii=False, sort_keys=True))
            handle.write("\n")
        (run_dir / "latest_snapshot.json").write_text(
            json.dumps(snapshot.model_dump(mode="json"), ensure_ascii=False, indent=2, sort_keys=True),
            encoding="utf-8",
        )
        return snapshot

    def latest_snapshot(self, run_id: str) -> RunMemorySnapshot:
        path = self._run_memory_dir(run_id) / "latest_snapshot.json"
        if not path.exists():
            raise KeyError(run_id)
        return RunMemorySnapshot.model_validate(json.loads(path.read_text(encoding="utf-8")))

    def checkpoints(self, run_id: str) -> list[dict[str, Any]]:
        path = self._run_memory_dir(run_id) / "checkpoints.jsonl"
        if not path.exists():
            return []
        rows = []
        for line in path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                rows.append(json.loads(line))
        return rows

    def write_result_checkpoints(self, snapshot: RunMemorySnapshot) -> RunMemorySnapshot:
        phases = ["run_started"]
        if snapshot.planner_order_ref:
            phases.append("planner_order")
        phases.extend(["execution_result"] * len([item for item in snapshot.work_items if item.execution_result_ref]))
        if snapshot.planner_decision_ref:
            phases.append("planner_decision")
        if snapshot.final_report_ref:
            phases.append("final_report")
        if snapshot.status in {"blocked", "failed"}:
            phases.append(snapshot.status)
        for phase in phases:
            self.write_checkpoint(snapshot, phase=phase)
        return snapshot

    def _run_memory_dir(self, run_id: str) -> Path:
        safe = "".join(char for char in run_id if char.isalnum() or char in {"-", "_"})
        if not safe:
            raise KeyError(run_id)
        return self.root / "runs" / safe / "memory"


def build_run_memory_snapshot(
    *,
    run_id: str,
    workflow_id: str,
    status: str,
    data: dict[str, Any],
    artifacts: dict[str, Any],
) -> RunMemorySnapshot:
    now = _now()
    shared_state = data.get("shared_run_state") if isinstance(data.get("shared_run_state"), dict) else {}
    control = shared_state.get("control") if isinstance(shared_state.get("control"), dict) else {}
    planner = shared_state.get("planner") if isinstance(shared_state.get("planner"), dict) else {}
    final_report = data.get("final_report") if isinstance(data.get("final_report"), dict) else artifacts.get("final_report")
    execution_results = _execution_results(data, artifacts)
    work_items = _work_items(shared_state, execution_results)
    changed_files = _changed_files_summary(execution_results)
    blocked_reasons = _blocked_reasons(execution_results)
    evidence_refs = _evidence_refs(execution_results, final_report)
    graph_cache = data.get("graph_run_cache") if isinstance(data.get("graph_run_cache"), dict) else {}
    snapshot = RunMemorySnapshot(
        run_id=run_id,
        planner_chat_session_id=_optional_string(data.get("planner_chat_session_id")),
        workflow_id=workflow_id,
        status=_run_status(status),
        current_round=_int_value(control.get("round") or _last_round(data), 0),
        planner_task_state=_safe_mapping(data.get("planner_task_state")),
        planner_order_ref=_optional_string(planner.get("planner_order_ref") or _last_round_ref(data, "planner_order")),
        planner_decision_ref=_optional_string(planner.get("planner_decision_ref") or _last_round_ref(data, "planner_decision")),
        final_report_ref=_optional_string(shared_state.get("final_report_ref") or "final_report" if final_report else None),
        work_items=work_items,
        execution_result_summaries=[_execution_summary(result) for result in execution_results],
        verification_summaries=[_verification_summary(result) for result in execution_results if isinstance(result.get("verification"), dict)],
        blocked_reasons=blocked_reasons,
        changed_files_summary=changed_files,
        evidence_refs=evidence_refs,
        diff_refs=_flatten_ref_map(graph_cache.get("diff_refs")),
        log_refs=_flatten_ref_map(graph_cache.get("log_refs")),
        native_event_refs=_flatten_ref_map(graph_cache.get("native_runtime_refs")),
        unresolved_items=[item.work_item_id for item in work_items if item.status not in {"completed", "skipped"}],
        next_recommended_action=_next_action(final_report, status),
        created_at=now,
        updated_at=now,
    )
    _reject_banned_snapshot_keys(snapshot.model_dump(mode="json"))
    return snapshot


def _execution_results(data: dict[str, Any], artifacts: dict[str, Any]) -> list[dict[str, Any]]:
    results = [
        artifact
        for artifact in artifacts.values()
        if isinstance(artifact, dict) and artifact.get("artifact_type") == "execution_result"
    ]
    if results:
        return sorted(results, key=_execution_sort_key)
    graph_cache = data.get("graph_run_cache") if isinstance(data.get("graph_run_cache"), dict) else {}
    execution_cache = graph_cache.get("execution_cache") if isinstance(graph_cache.get("execution_cache"), dict) else {}
    for record in execution_cache.values():
        if not isinstance(record, dict):
            continue
        artifact = record.get("artifact_payload")
        if isinstance(artifact, dict):
            results.append(artifact)
    return sorted(results, key=_execution_sort_key)


def _work_items(shared_state: dict[str, Any], execution_results: list[dict[str, Any]]) -> list[WorkItemMemoryStatus]:
    items: list[WorkItemMemoryStatus] = []
    state_items = shared_state.get("work_items") if isinstance(shared_state.get("work_items"), dict) else {}
    for raw_item in state_items.values():
        if not isinstance(raw_item, dict):
            continue
        items.append(
            WorkItemMemoryStatus(
                work_item_id=str(raw_item.get("work_item_id") or ""),
                agent_id=_optional_string(raw_item.get("agent_id")),
                status=_work_item_status(str(raw_item.get("status") or "pending")),
                summary=_optional_string(raw_item.get("summary")),
                execution_result_ref=_optional_string(raw_item.get("execution_result_ref")),
                evidence_refs=[],
            )
        )
    if items:
        return items
    for result in execution_results:
        refs = _string_list(result.get("evidence_refs"))
        verification = result.get("verification") if isinstance(result.get("verification"), dict) else {}
        refs.extend(_string_list(verification.get("evidence_refs")))
        items.append(
            WorkItemMemoryStatus(
                work_item_id=str(result.get("work_item_id") or ""),
                agent_id=_optional_string(result.get("agent_id")),
                status=_work_item_status(str(result.get("status") or "completed")),
                summary=_optional_string(result.get("summary")),
                execution_result_ref=_optional_string(result.get("artifact_id")),
                evidence_refs=_unique_strings(refs),
            )
        )
    return items


def _execution_summary(result: dict[str, Any]) -> dict[str, Any]:
    keep = {
        "artifact_id",
        "work_item_id",
        "agent_id",
        "status",
        "summary",
        "changed_files",
        "created_files",
        "deleted_files",
        "patch_refs",
        "evidence_refs",
        "blocker_type",
        "blocker_reason",
    }
    return _safe_mapping({key: result.get(key) for key in keep if key in result})


def _verification_summary(result: dict[str, Any]) -> dict[str, Any]:
    verification = result.get("verification") if isinstance(result.get("verification"), dict) else {}
    return _safe_mapping(
        {
            "work_item_id": result.get("work_item_id"),
            "status": verification.get("status"),
            "evidence_refs": _string_list(verification.get("evidence_refs")),
            "remaining_work": _string_list(verification.get("remaining_work")),
            "no_check_rationale": verification.get("no_check_rationale"),
        }
    )


def _changed_files_summary(execution_results: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "created": _unique_strings(item for result in execution_results for item in _string_list(result.get("created_files"))),
        "modified": _unique_strings(item for result in execution_results for item in _string_list(result.get("changed_files"))),
        "deleted": _unique_strings(item for result in execution_results for item in _string_list(result.get("deleted_files"))),
    }


def _blocked_reasons(execution_results: list[dict[str, Any]]) -> list[str]:
    reasons = []
    for result in execution_results:
        if result.get("status") != "blocked":
            continue
        reason = str(result.get("blocker_reason") or result.get("summary") or "").strip()
        if reason:
            reasons.append(reason)
    return _unique_strings(reasons)


def _evidence_refs(execution_results: list[dict[str, Any]], final_report: Any) -> list[str]:
    refs: list[str] = []
    for result in execution_results:
        refs.extend(_string_list(result.get("evidence_refs")))
        refs.extend(_string_list(result.get("patch_refs")))
        verification = result.get("verification") if isinstance(result.get("verification"), dict) else {}
        refs.extend(_string_list(verification.get("evidence_refs")))
    if isinstance(final_report, dict):
        refs.extend(_string_list(final_report.get("evidence_refs")))
    return _unique_strings(refs)


def _safe_mapping(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        return {}
    output: dict[str, Any] = {}
    for key, item in value.items():
        key_text = str(key)
        if key_text in BANNED_SNAPSHOT_KEYS:
            continue
        if isinstance(item, dict):
            output[key_text] = _safe_mapping(item)
        elif isinstance(item, list):
            output[key_text] = [_safe_value(child) for child in item]
        else:
            output[key_text] = item
    return output


def _safe_value(value: Any) -> Any:
    if isinstance(value, dict):
        return _safe_mapping(value)
    if isinstance(value, list):
        return [_safe_value(item) for item in value]
    return value


def _reject_banned_snapshot_keys(value: Any) -> None:
    if isinstance(value, dict):
        for key, item in value.items():
            if str(key) in BANNED_SNAPSHOT_KEYS:
                raise ValueError(f"run memory snapshot contains banned key: {key}")
            _reject_banned_snapshot_keys(item)
    elif isinstance(value, list):
        for item in value:
            _reject_banned_snapshot_keys(item)


def _last_round(data: dict[str, Any]) -> int:
    rounds = data.get("rounds")
    if isinstance(rounds, list) and rounds:
        last = rounds[-1]
        if isinstance(last, dict):
            return _int_value(last.get("round"), 0)
    return 0


def _last_round_ref(data: dict[str, Any], key: str) -> str | None:
    rounds = data.get("rounds")
    if isinstance(rounds, list) and rounds:
        last = rounds[-1]
        if isinstance(last, dict):
            return _optional_string(last.get(key))
    return None


def _flatten_ref_map(value: Any) -> list[str]:
    refs: list[str] = []
    if isinstance(value, dict):
        for item in value.values():
            refs.extend(_string_list(item))
    return _unique_strings(refs)


def _next_action(final_report: Any, status: str) -> str | None:
    if isinstance(final_report, dict):
        steps = final_report.get("next_steps")
        if isinstance(steps, list) and steps:
            return str(steps[0])
    if status in {"blocked", "failed"}:
        return f"Inspect the {status} run and continue from the latest checkpoint."
    return None


def _run_status(status: str) -> RunMemoryState:
    if status in {"running", "completed", "blocked", "failed"}:
        return status  # type: ignore[return-value]
    if status == "cancelled":
        return "failed"
    return "failed"


def _work_item_status(status: str) -> WorkItemMemoryState:
    if status in {"pending", "running", "completed", "blocked", "failed", "skipped"}:
        return status  # type: ignore[return-value]
    return "failed"


def _execution_sort_key(item: dict[str, Any]) -> tuple[int, int, str]:
    return (
        _int_value(item.get("round"), 0),
        _int_value(item.get("merge_index"), 0),
        str(item.get("work_item_id") or ""),
    )


def _optional_string(value: Any) -> str | None:
    text = str(value or "").strip()
    return text or None


def _string_list(value: Any) -> list[str]:
    if isinstance(value, str):
        return [value] if value.strip() else []
    if isinstance(value, list):
        return [str(item) for item in value if str(item).strip()]
    return []


def _unique_strings(values: Any) -> list[str]:
    seen: set[str] = set()
    output: list[str] = []
    for value in values:
        text = str(value).strip()
        if not text or text in seen:
            continue
        seen.add(text)
        output.append(text)
    return output


def _int_value(value: Any, default: int) -> int:
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()
