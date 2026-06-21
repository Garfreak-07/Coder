from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Any

from .artifacts import CheckCommand, CheckResultArtifact, CommandDiscoveryArtifact


def run_check_command(
    repo_root: str | Path,
    command: str,
    *,
    cwd: str = ".",
    timeout_seconds: int = 120,
) -> CheckResultArtifact:
    root = Path(repo_root).resolve()
    workdir = (root / cwd).resolve()
    try:
        workdir.relative_to(root)
    except ValueError:
        return CheckResultArtifact(
            command=command,
            cwd=cwd,
            status="blocked",
            summary="Check cwd escapes repository root.",
        )
    if not workdir.exists() or not workdir.is_dir():
        return CheckResultArtifact(
            command=command,
            cwd=cwd,
            status="blocked",
            summary="Check cwd does not exist.",
        )
    try:
        completed = subprocess.run(
            command,
            cwd=workdir,
            shell=True,
            text=True,
            capture_output=True,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired as exc:
        output = (exc.stdout or "") + (exc.stderr or "")
        return CheckResultArtifact(
            command=command,
            cwd=cwd,
            status="blocked",
            output=output[-8000:],
            summary=f"Check timed out after {timeout_seconds} seconds.",
        )
    output = (completed.stdout + completed.stderr)[-8000:]
    return CheckResultArtifact(
        command=command,
        cwd=cwd,
        status="pass" if completed.returncode == 0 else "fail",
        returncode=completed.returncode,
        output=output,
        summary=_summarize_output(output, completed.returncode),
    )


def run_discovered_checks(
    repo_root: str | Path,
    command_discovery: CommandDiscoveryArtifact | dict[str, Any],
    *,
    include_build: bool = True,
    limit: int = 3,
) -> list[CheckResultArtifact]:
    payload = (
        command_discovery
        if isinstance(command_discovery, dict)
        else command_discovery.model_dump(mode="python")
    )
    commands = [CheckCommand.model_validate(item) for item in payload.get("test_commands", [])]
    if include_build:
        commands.extend(CheckCommand.model_validate(item) for item in payload.get("build_commands", []))
    return [
        run_check_command(repo_root, command.command, cwd=command.cwd)
        for command in commands[:limit]
    ]


def _summarize_output(output: str, returncode: int) -> str:
    if returncode == 0:
        return "Check passed."
    lines = [line.strip() for line in output.splitlines() if line.strip()]
    return lines[-1] if lines else f"Check failed with exit code {returncode}."
