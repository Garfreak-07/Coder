from __future__ import annotations

from pathlib import Path
from typing import Any

from .artifacts import CheckCommand, CheckResultArtifact, CommandDiscoveryArtifact
from .command_service import CommandService


def run_check_command(
    repo_root: str | Path,
    command: str,
    *,
    cwd: str = ".",
    timeout_seconds: int = 120,
    sandbox: bool = False,
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
    result = CommandService(root).run_check(
        command,
        cwd=cwd,
        timeout_seconds=timeout_seconds,
        require_approval=False,
        source="discovered",
        sandbox=sandbox,
    )
    if result.get("status") == "blocked":
        return CheckResultArtifact(
            command=command,
            cwd=cwd,
            status="blocked",
            output=str(result.get("output") or "")[-8000:],
            summary=str(result.get("message") or result.get("output") or "Check blocked."),
        )
    output = str(result.get("output") or "")[-8000:]
    returncode = result.get("returncode")
    return CheckResultArtifact(
        command=command,
        cwd=cwd,
        status="pass" if result.get("passed") else "fail",
        returncode=int(returncode) if returncode is not None else None,
        output=output,
        summary=_summarize_output(output, int(returncode or 1)),
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
