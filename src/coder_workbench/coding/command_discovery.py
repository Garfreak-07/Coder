from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from .artifacts import CheckCommand, CommandDiscoveryArtifact


def discover_commands(repo_root: str | Path) -> CommandDiscoveryArtifact:
    root = Path(repo_root).resolve()
    test_commands: list[CheckCommand] = []
    build_commands: list[CheckCommand] = []
    lint_commands: list[CheckCommand] = []

    if (root / "tests").is_dir():
        test_commands.append(CheckCommand(command="python -m unittest discover -s tests", cwd=".", confidence="high"))
    if _has_python_sources(root):
        build_commands.append(CheckCommand(command="python -m compileall src tests", cwd=".", confidence="medium"))

    for package_json in sorted(_package_json_files(root)):
        package = _read_package_json(package_json)
        scripts = package.get("scripts") if isinstance(package.get("scripts"), dict) else {}
        cwd = package_json.parent.relative_to(root).as_posix() if package_json.parent != root else "."
        if "test" in scripts:
            test_commands.append(CheckCommand(command="npm run test", cwd=cwd, confidence="high"))
        if "build" in scripts:
            build_commands.append(CheckCommand(command="npm run build", cwd=cwd, confidence="high"))
        if "lint" in scripts:
            lint_commands.append(CheckCommand(command="npm run lint", cwd=cwd, confidence="high"))

    return CommandDiscoveryArtifact(
        test_commands=_dedupe_commands(test_commands),
        build_commands=_dedupe_commands(build_commands),
        lint_commands=_dedupe_commands(lint_commands),
        confidence="high" if test_commands or build_commands or lint_commands else "medium",
    )


def _has_python_sources(root: Path) -> bool:
    return (root / "src").is_dir() or (root / "tests").is_dir()


def _package_json_files(root: Path) -> list[Path]:
    ignored = {".git", "node_modules", ".venv", "venv", "dist", "build"}
    files: list[Path] = []
    for path in root.rglob("package.json"):
        if any(part in ignored for part in path.relative_to(root).parts):
            continue
        files.append(path)
    return files


def _read_package_json(path: Path) -> dict[str, Any]:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}


def _dedupe_commands(commands: list[CheckCommand]) -> list[CheckCommand]:
    seen: set[tuple[str, str]] = set()
    result: list[CheckCommand] = []
    for command in commands:
        key = (command.cwd, command.command)
        if key in seen:
            continue
        seen.add(key)
        result.append(command)
    return result
