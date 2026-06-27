from __future__ import annotations

import os
import shutil
import subprocess
from datetime import datetime, timezone
from pathlib import Path

from .repo_models import RepoFileRef
from .repo_safety import (
    DEFAULT_IGNORED_DIRS,
    ignored_by_default,
    normalize_repo_path,
    path_is_within_scopes,
    resolve_repo_root,
    sensitive_repo_path,
)


class RepoFileDiscoveryService:
    def __init__(self, *, repo_root: str | Path, scope_paths: list[str] | None = None) -> None:
        self.repo_root = resolve_repo_root(repo_root)
        self.scope_paths = [normalize_repo_path(scope) for scope in scope_paths or [] if str(scope).strip()]

    def list_files(
        self,
        *,
        query: str | None = None,
        extensions: list[str] | None = None,
        max_results: int = 200,
    ) -> list[RepoFileRef]:
        limit = _bounded_limit(max_results, upper=1000)
        extension_filter = {_normalize_extension(item) for item in extensions or [] if str(item).strip()}
        query_text = str(query or "").strip().lower()
        candidates = self._candidate_paths()
        refs: list[RepoFileRef] = []
        seen: set[str] = set()
        for relative_path in candidates:
            normalized = normalize_repo_path(relative_path)
            if not normalized or normalized in seen:
                continue
            if not self._allowed_path(normalized):
                continue
            if extension_filter and Path(normalized).suffix.lower() not in extension_filter:
                continue
            if query_text and query_text not in normalized.lower():
                continue
            absolute = self.repo_root / normalized
            if not absolute.is_file():
                continue
            refs.append(
                RepoFileRef(
                    path=normalized,
                    normalized_path=normalized,
                    size_bytes=absolute.stat().st_size,
                    modified_at=_modified_at(absolute),
                    language=_language_for_path(normalized),
                )
            )
            seen.add(normalized)
            if len(refs) >= limit:
                break
        return refs

    def _candidate_paths(self) -> list[str]:
        for provider in (self._git_files, self._rg_files, self._fd_files, self._walk_files):
            paths = provider()
            if paths:
                return paths
        return []

    def _git_files(self) -> list[str]:
        if not self._command_available("git"):
            return []
        inside = self._run_command(["git", "-C", str(self.repo_root), "rev-parse", "--is-inside-work-tree"])
        if inside.returncode != 0 or "true" not in inside.stdout.lower():
            return []
        result = self._run_command(["git", "-C", str(self.repo_root), "ls-files"])
        if result.returncode != 0:
            return []
        return [line.strip() for line in result.stdout.splitlines() if line.strip()]

    def _rg_files(self) -> list[str]:
        if not self._command_available("rg"):
            return []
        args = ["rg", "--files", *(_rg_ignore_globs())]
        result = self._run_command(args)
        if result.returncode != 0:
            return []
        return [line.strip() for line in result.stdout.splitlines() if line.strip()]

    def _fd_files(self) -> list[str]:
        if not self._command_available("fd"):
            return []
        args = ["fd", "--type", "f", "--strip-cwd-prefix"]
        result = self._run_command(args)
        if result.returncode != 0:
            return []
        return [line.strip() for line in result.stdout.splitlines() if line.strip()]

    def _walk_files(self) -> list[str]:
        paths: list[str] = []
        for root, dirs, files in os.walk(self.repo_root):
            relative_dir = normalize_repo_path(Path(root).relative_to(self.repo_root))
            dirs[:] = sorted(
                dirname
                for dirname in dirs
                if not ignored_by_default(normalize_repo_path(Path(relative_dir) / dirname), self.scope_paths)
            )
            for filename in sorted(files):
                paths.append(normalize_repo_path(Path(relative_dir) / filename))
        return paths

    def _allowed_path(self, relative_path: str) -> bool:
        if ignored_by_default(relative_path, self.scope_paths):
            return False
        if sensitive_repo_path(relative_path):
            return False
        return path_is_within_scopes(relative_path, self.scope_paths)

    def _command_available(self, command: str) -> bool:
        return shutil.which(command) is not None

    def _run_command(self, args: list[str]) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            args,
            cwd=self.repo_root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )


def _bounded_limit(value: int, *, upper: int) -> int:
    try:
        parsed = int(value)
    except (TypeError, ValueError):
        parsed = upper
    return min(max(parsed, 1), upper)


def _normalize_extension(value: str) -> str:
    text = str(value).strip().lower()
    return text if text.startswith(".") else f".{text}"


def _modified_at(path: Path) -> str:
    return datetime.fromtimestamp(path.stat().st_mtime, timezone.utc).isoformat()


def _language_for_path(path: str) -> str | None:
    suffix = Path(path).suffix.lower()
    return {
        ".py": "python",
        ".ts": "typescript",
        ".tsx": "typescriptreact",
        ".js": "javascript",
        ".jsx": "javascriptreact",
        ".md": "markdown",
        ".json": "json",
        ".yml": "yaml",
        ".yaml": "yaml",
    }.get(suffix)


def _rg_ignore_globs() -> list[str]:
    args: list[str] = []
    for dirname in sorted(DEFAULT_IGNORED_DIRS):
        args.extend(["--glob", f"!{dirname}/**"])
    return args


__all__ = ["RepoFileDiscoveryService"]
