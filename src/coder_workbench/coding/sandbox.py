from __future__ import annotations

import shutil
import tempfile
from pathlib import Path
from typing import Any

from coder_workbench.tools.filesystem import DEFAULT_IGNORE_DIRS
from coder_workbench.tools.patching import apply_patch

from .artifacts import CheckResultArtifact
from .checks import run_check_command


class SandboxedWorkspace:
    def __init__(self, repo_root: str | Path) -> None:
        self.repo_root = Path(repo_root).resolve()
        self._temp: tempfile.TemporaryDirectory[str] | None = None
        self.path: Path | None = None

    def __enter__(self) -> Path:
        self._temp = tempfile.TemporaryDirectory(prefix="coder-sandbox-")
        target = Path(self._temp.name) / "repo"
        shutil.copytree(
            self.repo_root,
            target,
            ignore=shutil.ignore_patterns(*DEFAULT_IGNORE_DIRS, "node_modules", "dist", "build", "*.egg-info"),
        )
        self.path = target.resolve()
        return self.path

    def __exit__(self, exc_type, exc, tb) -> None:
        if self._temp is not None:
            self._temp.cleanup()
        self.path = None


def apply_patch_in_sandbox(
    repo_root: str | Path,
    patch_or_changes: dict[str, Any] | list[dict[str, Any]],
    *,
    scopes: list[str] | None = None,
) -> dict[str, Any]:
    with SandboxedWorkspace(repo_root) as sandbox:
        patch_payload: dict[str, Any]
        if isinstance(patch_or_changes, list):
            patch_payload = {"files": patch_or_changes}
        else:
            patch_payload = patch_or_changes
        result = apply_patch(
            {"patch": patch_payload, "approved": True},
            {"repo_root": str(sandbox), "scopes": scopes or [], "data": {"preapprove_all": True}},
        )
        result["sandbox_root"] = str(sandbox)
        return result


def sandbox_apply_and_check(
    repo_root: str | Path,
    patch_or_changes: dict[str, Any] | list[dict[str, Any]],
    check_commands: list[dict[str, Any]],
    *,
    scopes: list[str] | None = None,
) -> dict[str, Any]:
    with SandboxedWorkspace(repo_root) as sandbox:
        if isinstance(patch_or_changes, list):
            patch_payload = {"files": patch_or_changes}
        else:
            patch_payload = patch_or_changes
        apply_result = apply_patch(
            {"patch": patch_payload, "approved": True},
            {"repo_root": str(sandbox), "scopes": scopes or [], "data": {"preapprove_all": True}},
        )
        checks: list[CheckResultArtifact] = []
        if apply_result.get("status") == "applied":
            for command in check_commands:
                checks.append(
                    run_check_command(
                        sandbox,
                        str(command.get("command") or ""),
                        cwd=str(command.get("cwd") or "."),
                        timeout_seconds=int(command.get("timeout_seconds") or 120),
                    )
                )
        return {
            "status": "pass" if checks and all(item.status == "pass" for item in checks) else "blocked" if not checks else "fail",
            "apply_result": apply_result,
            "check_results": [item.model_dump(mode="json") for item in checks],
        }
