from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from .artifacts import (
    CodingContextPacketArtifact,
    IncludedSnippet,
)
from .risk_map import is_risk_path


FILE_REF_RE = re.compile(r"(?P<path>(?:[\w.-]+/)+[\w.-]+\.(?:py|ts|tsx|js|jsx|json|md|toml|css|html))")
TOKEN_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]+")


class CodingContextBuilder:
    def __init__(self, repo_root: str | Path, *, max_snippet_lines: int = 80) -> None:
        self.repo_root = Path(repo_root).resolve()
        self.max_snippet_lines = max_snippet_lines

    def build(
        self,
        *,
        envelope: Any,
        repo_index: Any | None = None,
        symbol_index: Any | None = None,
        command_discovery: Any | None = None,
        risk_map: Any | None = None,
        upstream_refs: list[str] | None = None,
        selected_skills: list[dict[str, Any]] | None = None,
        token_budget: int = 4000,
    ) -> CodingContextPacketArtifact:
        task_summary = _field(envelope, "task_summary")
        work_item_id = _field(envelope, "work_item_id")
        selected: list[str] = []
        reasons: list[str] = []

        for path in _direct_file_refs(task_summary):
            if self._readable_file(path) and not is_risk_path(path, risk_map):
                selected.append(path)
                reasons.append(f"direct file reference: {path}")

        for path in _symbol_matches(task_summary, symbol_index):
            if self._readable_file(path) and not is_risk_path(path, risk_map):
                selected.append(path)
                reasons.append(f"symbol/task match: {path}")

        for path in list(selected):
            for related in self._related_tests(path):
                if not is_risk_path(related, risk_map):
                    selected.append(related)
                    reasons.append(f"related test: {related}")

        for path in _summary_files(repo_index):
            if self._readable_file(path) and not is_risk_path(path, risk_map):
                selected.append(path)
                reasons.append(f"repo summary file: {path}")

        included_files = _dedupe(selected)
        snippets: list[IncludedSnippet] = []
        estimated_tokens = 0
        omitted_files: list[str] = []
        for path in included_files:
            snippet = self._snippet(path)
            if snippet is None:
                continue
            next_tokens = _estimate_tokens(snippet.content)
            if estimated_tokens + next_tokens > token_budget and snippets:
                omitted_files.append(path)
                continue
            snippets.append(snippet)
            estimated_tokens += next_tokens

        included_files = [snippet.path for snippet in snippets]
        omitted_files.extend(path for path in selected if path not in included_files)

        artifacts = _included_artifacts(repo_index, command_discovery, risk_map, upstream_refs)
        return CodingContextPacketArtifact(
            work_item_id=work_item_id,
            included_files=included_files,
            included_snippets=snippets,
            included_artifacts=artifacts,
            included_skills=selected_skills or [],
            omitted_files=_dedupe(omitted_files),
            estimated_input_tokens=estimated_tokens + sum(_estimate_tokens(str(item)) for item in artifacts),
            estimated_omitted_tokens=sum(_estimate_tokens(path) for path in omitted_files),
            selection_reason=_dedupe(reasons),
        )

    def _readable_file(self, path: str) -> bool:
        target = (self.repo_root / path).resolve()
        try:
            target.relative_to(self.repo_root)
        except ValueError:
            return False
        return target.is_file()

    def _related_tests(self, path: str) -> list[str]:
        source = Path(path)
        stem = source.stem
        candidates = [
            Path("tests") / f"test_{stem}.py",
            Path("tests") / f"{stem}_test.py",
        ]
        if path.startswith("src/"):
            relative = source.relative_to("src")
            candidates.append(Path("tests") / relative)
        return [candidate.as_posix() for candidate in candidates if (self.repo_root / candidate).is_file()]

    def _snippet(self, path: str) -> IncludedSnippet | None:
        target = (self.repo_root / path).resolve()
        try:
            lines = target.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeDecodeError):
            return None
        selected = lines[: self.max_snippet_lines]
        return IncludedSnippet(
            path=path,
            start_line=1,
            end_line=max(1, len(selected)),
            content="\n".join(selected),
        )


def build_coding_context_packet(repo_root: str | Path, **kwargs: Any) -> CodingContextPacketArtifact:
    return CodingContextBuilder(repo_root).build(**kwargs)


def _field(value: Any, name: str) -> str:
    if isinstance(value, dict):
        return str(value.get(name) or "")
    return str(getattr(value, name, "") or "")


def _direct_file_refs(task_summary: str) -> list[str]:
    return [match.group("path") for match in FILE_REF_RE.finditer(task_summary)]


def _symbol_matches(task_summary: str, symbol_index: Any | None) -> list[str]:
    if not symbol_index:
        return []
    payload = symbol_index if isinstance(symbol_index, dict) else symbol_index.model_dump(mode="json")
    words = {word.lower() for word in TOKEN_RE.findall(task_summary)}
    paths: list[str] = []
    for file_item in payload.get("files", []):
        path = str(file_item.get("path") or "")
        haystack = {word.lower() for word in TOKEN_RE.findall(path)}
        for symbol in file_item.get("symbols", []):
            haystack.update(word.lower() for word in TOKEN_RE.findall(str(symbol.get("name") or "")))
        if words & haystack:
            paths.append(path)
    return paths


def _summary_files(repo_index: Any | None) -> list[str]:
    if not repo_index:
        return []
    payload = repo_index if isinstance(repo_index, dict) else repo_index.model_dump(mode="json")
    preferred = ["README.md", "pyproject.toml", "frontend/package.json"]
    important = [str(path) for path in payload.get("important_files", [])]
    return [path for path in preferred if path in important][:3]


def _included_artifacts(
    repo_index: Any | None,
    command_discovery: Any | None,
    risk_map: Any | None,
    upstream_refs: list[str] | None,
) -> list[dict[str, Any]]:
    artifacts: list[dict[str, Any]] = []
    for value in (repo_index, command_discovery, risk_map):
        if value is None:
            continue
        artifacts.append(value if isinstance(value, dict) else value.model_dump(mode="json"))
    if upstream_refs:
        artifacts.append({"artifact_type": "upstream_refs", "refs": upstream_refs})
    return artifacts


def _estimate_tokens(value: str) -> int:
    return max(1, len(value) // 4)


def _dedupe(values: list[str]) -> list[str]:
    seen: set[str] = set()
    result: list[str] = []
    for value in values:
        if not value or value in seen:
            continue
        seen.add(value)
        result.append(value)
    return result
