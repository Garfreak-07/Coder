from __future__ import annotations

from pathlib import Path
from typing import Any
from uuid import uuid4

from coder_workbench.tools.patching import propose_patch

from .artifacts import PatchPreviewArtifact
from .risk_map import build_risk_map, is_risk_path


def build_patch_preview(
    repo_root: str | Path,
    proposed_changes: list[dict[str, Any]],
    *,
    scopes: list[str] | None = None,
    risk_map: dict[str, Any] | None = None,
) -> PatchPreviewArtifact:
    active_risk_map = risk_map or build_risk_map(repo_root).model_dump(mode="json")
    rejected = [
        {"path": str(change.get("path") or change.get("file") or ""), "code": "risk_path", "message": "Change targets a risk path."}
        for change in proposed_changes
        if is_risk_path(str(change.get("path") or change.get("file") or ""), active_risk_map)
    ]
    if rejected:
        return PatchPreviewArtifact(
            status="blocked",
            patch_ref=f"patch_preview_{uuid4()}",
            change_count=len(proposed_changes),
            errors=rejected,
        )

    preview = propose_patch(
        {"changes": proposed_changes},
        {"repo_root": str(Path(repo_root).resolve()), "scopes": scopes or [], "data": {}},
    )
    status = "patch_preview_created" if preview.get("status") == "proposed" else "rejected"
    return PatchPreviewArtifact(
        status=status,
        patch_ref=f"patch_preview_{preview.get('patch_id') or uuid4()}",
        change_count=int(preview.get("change_count") or 0),
        files=list(preview.get("files") or []),
        errors=list(preview.get("errors") or []),
        requires_approval=bool(preview.get("requires_approval", True)),
    )


def attach_patch_preview_to_execution(
    execution_result: dict[str, Any],
    preview: PatchPreviewArtifact,
) -> dict[str, Any]:
    payload = dict(execution_result)
    refs = list(payload.get("patch_refs") or [])
    if preview.patch_ref and preview.status == "patch_preview_created" and preview.patch_ref not in refs:
        refs.append(preview.patch_ref)
    payload["patch_refs"] = refs
    if preview.status != "patch_preview_created":
        payload["needs_planner_decision"] = True
        payload["blocker_type"] = "risk_boundary"
        payload["planner_question"] = "Proposed changes could not be converted to a safe patch preview."
    return payload
