from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Literal
from uuid import uuid4

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.memory.models import AgentMemoryRole, MemoryTrustLevel, SECRET_MARKERS


PlannerMemoryTargetFile = Literal[
    "planner.md",
    "architecture.md",
    "decisions.md",
    "roadmap.md",
    "preferences.md",
    "knowledge_sources.md",
]

RAW_PAYLOAD_MARKERS = (
    "raw log",
    "raw logs",
    "raw prompt",
    "raw prompts",
    "raw model output",
    "raw model outputs",
    "diff --git",
    "full diff",
)


class PlannerMemoryWriteProposal(BaseModel):
    model_config = ConfigDict(extra="forbid")

    artifact_type: Literal["planner_memory_write_proposal"] = "planner_memory_write_proposal"

    target_scope: Literal["user", "project"]
    target_file: PlannerMemoryTargetFile

    operation: Literal["append", "replace_section", "supersede"]
    title: str
    content: str
    reason: str

    evidence_refs: list[str] = Field(default_factory=list)
    confidence: Literal["low", "medium", "high"] = "medium"
    requires_user_confirmation: bool = True


class PlannerFileMemoryCommitter:
    def __init__(self, root: str | Path) -> None:
        self.root = _memory_root(root) / "planner"
        self.proposals_dir = self.root / "proposals"
        self.proposals_dir.mkdir(parents=True, exist_ok=True)

    def propose(
        self,
        proposal: PlannerMemoryWriteProposal | dict[str, Any],
        *,
        role: AgentMemoryRole = "planning_chat",
        project_id: str = "default",
        trust_level: MemoryTrustLevel = "model_inferred",
    ) -> dict[str, Any]:
        parsed = proposal if isinstance(proposal, PlannerMemoryWriteProposal) else PlannerMemoryWriteProposal.model_validate(proposal)
        validate_planner_memory_write_proposal(parsed, role=role, trust_level=trust_level)
        requires_confirmation = parsed.requires_user_confirmation
        if parsed.target_scope == "user":
            requires_confirmation = True
        elif not (trust_level == "system_recorded" and parsed.evidence_refs):
            requires_confirmation = True
        parsed = parsed.model_copy(update={"requires_user_confirmation": requires_confirmation})
        proposal_id = f"planner-memory-proposal-{uuid4().hex}"
        record = {
            "proposal_id": proposal_id,
            "status": "proposed",
            "role": role,
            "project_id": project_id,
            "trust_level": trust_level,
            "created_at": _now(),
            "updated_at": _now(),
            "proposal": parsed.model_dump(mode="json"),
        }
        self._proposal_path(proposal_id).write_text(
            json.dumps(record, ensure_ascii=False, indent=2, sort_keys=True),
            encoding="utf-8",
        )
        return record

    def commit(self, proposal_id: str, *, approved: bool) -> dict[str, Any]:
        path = self._proposal_path(proposal_id)
        if not path.exists():
            raise KeyError(proposal_id)
        record = json.loads(path.read_text(encoding="utf-8"))
        proposal = PlannerMemoryWriteProposal.model_validate(record["proposal"])
        if not approved:
            record["status"] = "rejected"
            record["updated_at"] = _now()
            path.write_text(json.dumps(record, ensure_ascii=False, indent=2, sort_keys=True), encoding="utf-8")
            return {"proposal_id": proposal_id, "status": "rejected", "written": False}

        target = self._target_path(proposal, str(record.get("project_id") or "default"))
        target.parent.mkdir(parents=True, exist_ok=True)
        existing = target.read_text(encoding="utf-8") if target.exists() else ""
        updated = _apply_markdown_operation(existing, proposal)
        target.write_text(updated, encoding="utf-8")
        record["status"] = "committed"
        record["committed_at"] = _now()
        record["updated_at"] = record["committed_at"]
        record["target_path"] = str(target)
        path.write_text(json.dumps(record, ensure_ascii=False, indent=2, sort_keys=True), encoding="utf-8")
        return {
            "proposal_id": proposal_id,
            "status": "committed",
            "written": True,
            "target_path": str(target),
        }

    def _proposal_path(self, proposal_id: str) -> Path:
        safe = "".join(char for char in proposal_id if char.isalnum() or char in {"-", "_"})
        if not safe:
            raise KeyError(proposal_id)
        return self.proposals_dir / f"{safe}.json"

    def _target_path(self, proposal: PlannerMemoryWriteProposal, project_id: str) -> Path:
        if proposal.target_scope == "user":
            return self.root / "user" / proposal.target_file
        safe_project = "".join(char for char in project_id if char.isalnum() or char in {"-", "_", "."}) or "default"
        return self.root / "projects" / safe_project / proposal.target_file


def validate_planner_memory_write_proposal(
    proposal: PlannerMemoryWriteProposal,
    *,
    role: AgentMemoryRole,
    trust_level: MemoryTrustLevel = "model_inferred",
) -> PlannerMemoryWriteProposal:
    if role != "planning_chat":
        raise ValueError("only planning_chat may create planner_memory_write_proposal")
    if proposal.target_scope == "user" and not proposal.requires_user_confirmation:
        raise ValueError("user scope planner memory always requires confirmation")
    if proposal.target_scope == "project" and not proposal.requires_user_confirmation:
        if not (trust_level == "system_recorded" and proposal.evidence_refs):
            raise ValueError("project planner memory requires confirmation unless system_recorded with evidence")
    _reject_unsafe_content(proposal.content)
    _reject_unsafe_content(proposal.reason)
    if not proposal.title.strip():
        raise ValueError("planner memory proposal title is required")
    if not proposal.content.strip():
        raise ValueError("planner memory proposal content is required")
    return proposal


def _apply_markdown_operation(existing: str, proposal: PlannerMemoryWriteProposal) -> str:
    section = _section_text(proposal)
    if proposal.operation == "replace_section":
        return _replace_section(existing, proposal.title, section)
    if proposal.operation == "supersede":
        section = _section_text(proposal, prefix="Supersedes prior planner memory entry.")
    return _append_section(existing, section)


def _section_text(proposal: PlannerMemoryWriteProposal, *, prefix: str | None = None) -> str:
    parts = [f"## {proposal.title.strip()}"]
    if prefix:
        parts.append(prefix)
    parts.append(proposal.content.strip())
    parts.append(f"Reason: {proposal.reason.strip()}")
    if proposal.evidence_refs:
        parts.append("Evidence refs: " + ", ".join(proposal.evidence_refs))
    return "\n\n".join(parts).strip() + "\n"


def _append_section(existing: str, section: str) -> str:
    if not existing.strip():
        return section
    return existing.rstrip() + "\n\n" + section


def _replace_section(existing: str, title: str, section: str) -> str:
    lines = existing.splitlines()
    heading = f"## {title.strip()}"
    start = next((index for index, line in enumerate(lines) if line.strip() == heading), None)
    if start is None:
        return _append_section(existing, section)
    end = len(lines)
    for index in range(start + 1, len(lines)):
        if lines[index].startswith("## "):
            end = index
            break
    replacement = section.rstrip().splitlines()
    updated = [*lines[:start], *replacement, *lines[end:]]
    return "\n".join(updated).rstrip() + "\n"


def _reject_unsafe_content(value: str) -> None:
    lower = value.lower()
    for marker in (*SECRET_MARKERS, *RAW_PAYLOAD_MARKERS):
        if marker in lower:
            raise ValueError(f"planner memory content contains unsafe marker: {marker}")


def _memory_root(root: str | Path) -> Path:
    path = Path(root)
    return path if path.name == "memory" else path / "memory"


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()
