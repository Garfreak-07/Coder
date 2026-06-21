from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from .artifacts import CheckResultArtifact, DebugFindingArtifact


PATH_RE = re.compile(r"(?P<path>(?:[A-Za-z]:)?[\\/]?[\w .-]+(?:[\\/][\w .-]+)*\.(?:py|ts|tsx|js|jsx))")
ERROR_PATTERNS = [
    "Traceback",
    "AssertionError",
    "ModuleNotFoundError",
    "ImportError",
    "TypeError",
    "ValueError",
    "FAILED",
    "Error:",
    "TS",
]


def build_debug_finding(
    check_result: CheckResultArtifact | dict[str, Any],
    *,
    work_item_id: str = "",
    repo_root: str | Path | None = None,
) -> DebugFindingArtifact:
    payload = check_result if isinstance(check_result, dict) else check_result.model_dump(mode="json")
    output = str(payload.get("output") or payload.get("summary") or "")
    status = "blocked" if payload.get("status") == "blocked" else "failed"
    return DebugFindingArtifact(
        work_item_id=work_item_id,
        command=str(payload.get("command") or ""),
        status=status,
        failure_summary=_failure_summary(output, str(payload.get("summary") or "")),
        likely_files=_likely_files(output, repo_root),
        error_patterns=_error_patterns(output),
        raw_output_ref=str(payload.get("output_ref") or ""),
    )


def _failure_summary(output: str, fallback: str) -> str:
    lines = [line.strip() for line in output.splitlines() if line.strip()]
    for line in reversed(lines[-20:]):
        if any(pattern in line for pattern in ERROR_PATTERNS):
            return line[:500]
    return (fallback or (lines[-1] if lines else "Check failed."))[:500]


def _likely_files(output: str, repo_root: str | Path | None) -> list[str]:
    root = Path(repo_root).resolve() if repo_root else None
    files: list[str] = []
    for match in PATH_RE.finditer(output):
        raw = match.group("path").strip()
        normalized = raw.replace("\\", "/").lstrip("/")
        if root:
            try:
                path = Path(raw)
                if path.is_absolute():
                    normalized = path.resolve().relative_to(root).as_posix()
            except (OSError, ValueError):
                pass
        if normalized not in files:
            files.append(normalized)
    return files[:8]


def _error_patterns(output: str) -> list[str]:
    return [pattern for pattern in ERROR_PATTERNS if pattern in output][:8]
