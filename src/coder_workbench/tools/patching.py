from __future__ import annotations

import difflib
import json
import shutil
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from uuid import uuid4

from coder_workbench.tools.filesystem import resolve_scoped_path


PATCH_ACTIONS = {"create", "update", "delete"}


@dataclass(frozen=True)
class _PreparedPatch:
    item: dict[str, Any]
    path: str
    action: str
    target: Path
    relative: str
    existed: bool
    content: str | None


def propose_patch(args: dict[str, Any], runtime_context: dict[str, Any]) -> dict[str, Any]:
    repo_root = Path(runtime_context["repo_root"]).resolve()
    scopes = _list_value(runtime_context.get("scopes"))
    changes = _normalize_changes(args.get("changes") or args.get("files") or args)
    previews: list[dict[str, Any]] = []

    for change in changes:
        path = _change_path(change)
        action = _change_action(change)
        content = change.get("content")
        target = resolve_scoped_path(repo_root, path, scopes)
        current = _read_text_if_exists(target)

        if action == "create" and target.exists():
            action = "update"
        if action == "update" and current is None:
            action = "create"
        if action == "delete":
            next_content = None
        else:
            next_content = "" if content is None else str(content)

        previews.append(
            {
                "path": target.relative_to(repo_root).as_posix(),
                "action": action,
                "exists": target.exists(),
                "expected_before": current,
                "diff": _unified_diff(
                    path=target.relative_to(repo_root).as_posix(),
                    before=current,
                    after=next_content,
                ),
                "content": next_content,
            }
        )

    return {
        "status": "proposed",
        "patch_id": str(uuid4()),
        "requires_approval": True,
        "change_count": len(previews),
        "files": previews,
    }


def apply_patch(args: dict[str, Any], runtime_context: dict[str, Any]) -> dict[str, Any]:
    if not _is_approved(args, runtime_context):
        return {
            "status": "blocked",
            "blocked": True,
            "requires_approval": True,
            "message": "Patch apply requires explicit patch approval.",
        }

    repo_root = Path(runtime_context["repo_root"]).resolve()
    scopes = _list_value(runtime_context.get("scopes"))
    patch = args.get("patch") or runtime_context.get("data", {}).get(str(args.get("patch_key") or "patch_preview")) or args
    files = _patch_files(patch)
    prepared, rejection = _prepare_patch_files(repo_root, scopes, files)
    if rejection:
        return rejection

    snapshot_id = str(uuid4())
    snapshot_root = repo_root / ".coder_history" / "snapshots" / snapshot_id
    snapshot_root.mkdir(parents=True, exist_ok=True)
    manifest: dict[str, Any] = {
        "id": snapshot_id,
        "created_at": datetime.now(timezone.utc).isoformat(),
        "files": [],
    }
    applied: list[dict[str, Any]] = []

    for change in prepared:
        snapshot_file = snapshot_root / "files" / change.relative
        manifest["files"].append(
            {
                "path": change.relative,
                "existed": change.existed,
                "snapshot_path": f"files/{change.relative}" if change.existed else None,
            }
        )
        if change.existed:
            snapshot_file.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(change.target, snapshot_file)

        if change.action == "delete":
            if change.target.exists():
                change.target.unlink()
        else:
            if change.content is None:
                raise ValueError(f"Patch item for {change.relative} is missing content")
            change.target.parent.mkdir(parents=True, exist_ok=True)
            change.target.write_text(change.content, encoding="utf-8")

        applied.append({"path": change.relative, "action": change.action})

    (snapshot_root / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    return {
        "status": "applied",
        "snapshot_id": snapshot_id,
        "applied": applied,
        "message": f"Applied {len(applied)} file change(s).",
    }


def rollback_patch(args: dict[str, Any], runtime_context: dict[str, Any]) -> dict[str, Any]:
    repo_root = Path(runtime_context["repo_root"]).resolve()
    snapshot_id = str(args.get("snapshot_id") or "").strip()
    if not snapshot_id:
        patch_result = args.get("patch_result") or runtime_context.get("data", {}).get("patch_apply")
        if isinstance(patch_result, dict):
            snapshot_id = str(patch_result.get("snapshot_id") or "").strip()
    if not snapshot_id:
        raise ValueError("snapshot_id is required for rollback")

    snapshot_root = (repo_root / ".coder_history" / "snapshots" / snapshot_id).resolve()
    expected_root = (repo_root / ".coder_history" / "snapshots").resolve()
    if not str(snapshot_root).startswith(str(expected_root)):
        raise ValueError("Invalid snapshot_id")

    manifest_path = snapshot_root / "manifest.json"
    if not manifest_path.exists():
        raise FileNotFoundError(f"Snapshot not found: {snapshot_id}")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    restored: list[dict[str, Any]] = []

    for item in manifest.get("files", []):
        relative = str(item["path"])
        target = resolve_scoped_path(repo_root, relative, _list_value(runtime_context.get("scopes")))
        if item.get("existed"):
            source = snapshot_root / str(item["snapshot_path"])
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
            restored.append({"path": relative, "action": "restored"})
        elif target.exists():
            target.unlink()
            restored.append({"path": relative, "action": "removed"})
        else:
            restored.append({"path": relative, "action": "unchanged"})

    return {
        "status": "rolled_back",
        "snapshot_id": snapshot_id,
        "restored": restored,
        "message": f"Rolled back {len(restored)} file change(s).",
    }


def _normalize_changes(value: Any) -> list[dict[str, Any]]:
    if isinstance(value, dict):
        for key in ("changes", "files", "patches"):
            if isinstance(value.get(key), list):
                return [dict(item) for item in value[key] if isinstance(item, dict)]
        if "path" in value:
            return [dict(value)]
        return []
    if isinstance(value, list):
        return [dict(item) for item in value if isinstance(item, dict)]
    return []


def _patch_files(value: Any) -> list[dict[str, Any]]:
    if isinstance(value, dict) and isinstance(value.get("files"), list):
        return [dict(item) for item in value["files"] if isinstance(item, dict)]
    return _normalize_changes(value)


def _prepare_patch_files(
    repo_root: Path, scopes: list[str], files: list[dict[str, Any]]
) -> tuple[list[_PreparedPatch], dict[str, Any] | None]:
    prepared: list[_PreparedPatch] = []
    errors: list[dict[str, Any]] = []

    for item in files:
        try:
            path = _change_path(item)
            action = _change_action(item)
            target = resolve_scoped_path(repo_root, path, scopes)
            relative = target.relative_to(repo_root).as_posix()
            existed = target.exists()
            current = _read_text_if_exists(target) if existed else None
            expected_was_provided = _has_expected_before(item)
            expected_before = _expected_before(item) if expected_was_provided else None

            if action == "create" and existed:
                errors.append(_patch_error(relative, "target_exists", "Create patch target already exists."))
                continue
            if action in {"update", "delete"} and not existed:
                errors.append(_patch_error(relative, "target_missing", f"{action.title()} patch target does not exist."))
                continue
            if expected_was_provided:
                conflict = _expected_before_conflict(relative, existed, current, expected_before)
                if conflict:
                    errors.append(conflict)
                    continue

            content: str | None = None
            if action != "delete":
                raw_content = item.get("content")
                if raw_content is None:
                    errors.append(_patch_error(relative, "missing_content", "Patch item is missing content."))
                    continue
                content = str(raw_content)
                if _looks_binary_text(content):
                    errors.append(_patch_error(relative, "binary_content", "Patch content appears to be binary."))
                    continue

            prepared.append(
                _PreparedPatch(
                    item=item,
                    path=path,
                    action=action,
                    target=target,
                    relative=relative,
                    existed=existed,
                    content=content,
                )
            )
        except UnicodeDecodeError:
            path = str(item.get("path") or item.get("file") or "unknown")
            errors.append(_patch_error(path, "binary_file", "Patch target is binary or not valid UTF-8."))
        except (FileNotFoundError, IsADirectoryError, NotADirectoryError, ValueError) as exc:
            path = str(item.get("path") or item.get("file") or "unknown")
            errors.append(_patch_error(path, "invalid_target", str(exc)))

    if errors:
        return [], {
            "status": "rejected",
            "error_code": "patch_preflight_failed",
            "message": f"Patch rejected before apply: {len(errors)} issue(s).",
            "errors": errors,
        }
    return prepared, None


def _change_path(change: dict[str, Any]) -> str:
    path = str(change.get("path") or change.get("file") or "").strip()
    if not path:
        raise ValueError("Patch change requires path")
    return path


def _change_action(change: dict[str, Any]) -> str:
    action = str(change.get("action") or "update").strip().lower()
    if action not in PATCH_ACTIONS:
        raise ValueError(f"Unsupported patch action: {action}")
    return action


def _read_text_if_exists(path: Path) -> str | None:
    if not path.exists():
        return None
    if not path.is_file():
        raise IsADirectoryError(f"Patch target is not a file: {path}")
    if _is_binary_file(path):
        raise UnicodeDecodeError("utf-8", b"", 0, 1, "binary file")
    return path.read_text(encoding="utf-8")


def _unified_diff(path: str, before: str | None, after: str | None) -> str:
    before_lines = [] if before is None else before.splitlines(keepends=True)
    after_lines = [] if after is None else after.splitlines(keepends=True)
    return "".join(
        difflib.unified_diff(
            before_lines,
            after_lines,
            fromfile=f"a/{path}",
            tofile=f"b/{path}",
            lineterm="",
        )
    )


def _is_approved(args: dict[str, Any], runtime_context: dict[str, Any]) -> bool:
    data = runtime_context.get("data", {})
    patch_approval = data.get("patch_approval", {})
    return bool(
        args.get("approved")
        or args.get("patch_approved")
        or data.get("patch_approved")
        or data.get("preapprove_all")
        or (isinstance(patch_approval, dict) and patch_approval.get("approved"))
    )


def _has_expected_before(change: dict[str, Any]) -> bool:
    return any(key in change for key in ("expected_before", "before", "base_content"))


def _expected_before(change: dict[str, Any]) -> str | None:
    if "expected_before" in change:
        value = change.get("expected_before")
    elif "before" in change:
        value = change.get("before")
    else:
        value = change.get("base_content")
    return None if value is None else str(value)


def _expected_before_conflict(
    relative: str, existed: bool, current: str | None, expected_before: str | None
) -> dict[str, Any] | None:
    if expected_before is None and existed:
        return _patch_error(relative, "stale_base", "Patch expected the file to be absent, but it exists.")
    if expected_before is not None and not existed:
        return _patch_error(relative, "stale_base", "Patch expected the file to exist, but it is missing.")
    if expected_before is not None and current != expected_before:
        return _patch_error(relative, "stale_base", "File content changed since patch preview.")
    return None


def _patch_error(path: str, code: str, message: str) -> dict[str, Any]:
    return {
        "path": path,
        "code": code,
        "message": message,
    }


def _is_binary_file(path: Path) -> bool:
    with path.open("rb") as handle:
        chunk = handle.read(8192)
    return b"\0" in chunk


def _looks_binary_text(value: str) -> bool:
    return "\0" in value


def _list_value(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        return [value] if value.strip() else []
    if isinstance(value, list):
        return [str(item) for item in value if str(item).strip()]
    return [str(value)]
