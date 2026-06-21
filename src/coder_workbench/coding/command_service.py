from __future__ import annotations

import hashlib
import subprocess
from pathlib import Path
from typing import Any

from coder_workbench.coding.command_policy import evaluate_command_policy


class CommandService:
    def __init__(self, repo_root: str | Path, *, scopes: list[str] | None = None, data: dict[str, Any] | None = None) -> None:
        self.repo_root = Path(repo_root).resolve()
        self.scopes = scopes or []
        self.data = data or {}

    def run_check(
        self,
        command: str = "",
        *,
        argv: list[str] | None = None,
        cwd: str = ".",
        timeout_seconds: int = 120,
        require_approval: bool = True,
        shell: bool | None = None,
        source: str = "model",
        sandbox: bool = False,
    ) -> dict[str, Any]:
        command = command.strip()
        argv = [str(item) for item in (argv or []) if str(item)]
        command_text = command or " ".join(argv)
        if not command_text:
            return {"passed": True, "output": "No check command configured.", "skipped": True}
        default_cwd = self.scopes[0] if self.scopes else "."
        workdir = resolve_scoped_path(self.repo_root, cwd or default_cwd, self.scopes)
        cwd_relative = workdir.relative_to(self.repo_root).as_posix() if workdir != self.repo_root else "."
        active_shell = True if shell is None and not argv else bool(shell)
        policy = evaluate_command_policy(
            command=command,
            argv=argv,
            shell=active_shell,
            source=source,
            sandbox=sandbox,
        )
        if not policy.allowed:
            return {
                "passed": False,
                "status": "blocked",
                "output": policy.reason or "Command blocked by policy.",
                "blocked": True,
                "requires_approval": False,
                "approval_type": "command",
                "command": command_text,
                "cwd": cwd_relative,
                "message": policy.reason or "Command blocked by policy.",
                "policy": policy.as_dict(),
            }

        effective_require_approval = require_approval or policy.requires_approval
        approval_key = command_approval_key(command_text, cwd_relative)
        if effective_require_approval and not self.command_is_approved(approval_key):
            return {
                "passed": False,
                "status": "blocked",
                "output": f"Check command requires explicit approval: {command_text}",
                "blocked": True,
                "requires_approval": True,
                "approval_type": "command",
                "approval_key": approval_key,
                "command": command_text,
                "cwd": cwd_relative,
                "message": f"Approve command before running: {command_text}",
                "policy": policy.as_dict(),
            }
        try:
            if argv:
                completed = subprocess.run(
                    argv,
                    cwd=workdir,
                    shell=False,
                    text=True,
                    capture_output=True,
                    timeout=timeout_seconds,
                )
            else:
                completed = subprocess.run(
                    command,
                    cwd=workdir,
                    shell=active_shell,
                    text=True,
                    capture_output=True,
                    timeout=timeout_seconds,
                )
        except subprocess.TimeoutExpired as exc:
            output = ((exc.stdout or "") + (exc.stderr or ""))[-8000:]
            return {
                "passed": False,
                "status": "blocked",
                "returncode": None,
                "cwd": cwd_relative,
                "command": command_text,
                "approval_key": approval_key,
                "output": output,
                "message": f"Check timed out after {timeout_seconds} seconds.",
                "policy": policy.as_dict(),
            }
        return {
            "passed": completed.returncode == 0,
            "returncode": completed.returncode,
            "cwd": cwd_relative,
            "command": command_text,
            "approval_key": approval_key,
            "output": (completed.stdout + completed.stderr)[-8000:],
            "policy": policy.as_dict(),
        }

    def command_is_approved(self, approval_key: str) -> bool:
        approvals = self.data.get("command_approvals", {})
        return bool(
            self.data.get("preapprove_all")
            or (isinstance(approvals, dict) and approvals.get(approval_key) is True)
        )


def command_approval_key(command: str, cwd: str) -> str:
    digest = hashlib.sha256(f"{cwd}\0{command}".encode("utf-8")).hexdigest()
    return f"cmd:{digest}"


def resolve_scoped_path(root: Path, path: str, scopes: list[str] | None = None) -> Path:
    candidate = (root / path).resolve()
    if not _is_relative_to(candidate, root):
        raise ValueError(f"Path escapes repo root: {path}")
    allowed_scopes = _normalize_scope_paths(root, scopes)
    if allowed_scopes and not any(_is_relative_to(candidate, (root / scope).resolve()) for scope in allowed_scopes):
        raise ValueError(f"Path is outside allowed scopes: {path}")
    return candidate


def _normalize_scope_paths(root: Path, scopes: list[str] | None) -> list[str]:
    normalized: list[str] = []
    for item in scopes or []:
        scope = item.strip()
        if not scope or scope in {".", "./"}:
            continue
        candidate = (root / scope).resolve()
        if not _is_relative_to(candidate, root):
            raise ValueError(f"Scope escapes repo root: {scope}")
        if not candidate.exists():
            raise FileNotFoundError(f"Scope does not exist: {scope}")
        if not candidate.is_dir():
            raise NotADirectoryError(f"Scope is not a directory: {scope}")
        normalized.append(candidate.relative_to(root.resolve()).as_posix())
    return normalized


def _is_relative_to(path: Path, root: Path) -> bool:
    try:
        path.resolve().relative_to(root.resolve())
        return True
    except ValueError:
        return False
