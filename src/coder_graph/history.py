from __future__ import annotations

import json
import shutil
import time
from pathlib import Path

from .tools.filesystem import is_relative_to, summarize_project

MAX_SNAPSHOTS = 20


def create_snapshot(repo_root: Path, scopes: list[str], label: str) -> Path:
    """Create a lightweight file snapshot for future rollback.

    Snapshots are intentionally local and simple. They are not a replacement
    for git, but they support a Word-like undo/redo UX for edits made by Coder.
    """

    repo_root = repo_root.resolve()
    history_root = repo_root / ".coder_history"
    snapshots_root = history_root / "snapshots"
    snapshot_id = f"{int(time.time())}-{_safe_label(label)}"
    snapshot_root = snapshots_root / snapshot_id
    files_root = snapshot_root / "files"
    files_root.mkdir(parents=True, exist_ok=True)

    file_summaries = summarize_project(repo_root, scopes, max_files=2_000)
    copied: list[str] = []
    for item in file_summaries:
        source = (repo_root / item["path"]).resolve()
        if not is_relative_to(source, repo_root):
            continue
        destination = files_root / item["path"]
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        copied.append(item["path"])

    metadata = {
        "id": snapshot_id,
        "label": label,
        "created_at": int(time.time()),
        "scopes": scopes,
        "file_count": len(copied),
        "files": copied,
    }
    (snapshot_root / "metadata.json").write_text(
        json.dumps(metadata, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )

    prune_snapshots(repo_root, MAX_SNAPSHOTS)
    return snapshot_root


def list_snapshots(repo_root: Path) -> list[dict]:
    snapshots_root = repo_root.resolve() / ".coder_history" / "snapshots"
    if not snapshots_root.exists():
        return []

    snapshots: list[dict] = []
    for metadata_path in snapshots_root.glob("*/metadata.json"):
        try:
            snapshots.append(json.loads(metadata_path.read_text(encoding="utf-8")))
        except json.JSONDecodeError:
            continue

    return sorted(snapshots, key=lambda item: item.get("created_at", 0), reverse=True)


def prune_snapshots(repo_root: Path, keep: int = MAX_SNAPSHOTS) -> None:
    snapshots = list_snapshots(repo_root)
    for snapshot in snapshots[keep:]:
        snapshot_dir = repo_root.resolve() / ".coder_history" / "snapshots" / snapshot["id"]
        if snapshot_dir.exists():
            shutil.rmtree(snapshot_dir)


def _safe_label(label: str) -> str:
    cleaned = "".join(char if char.isalnum() else "-" for char in label.lower()).strip("-")
    return cleaned[:40] or "snapshot"
