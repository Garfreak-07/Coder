from __future__ import annotations

import re
from pathlib import Path

from coder_workbench.tools.filesystem import DEFAULT_IGNORE_DIRS

from .artifacts import SymbolFile, SymbolIndexArtifact, SymbolRecord


PYTHON_SYMBOL_RE = re.compile(r"^(?P<indent>\s*)(?P<kind>class|def|async\s+def)\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)")
TS_SYMBOL_PATTERNS = [
    re.compile(r"^\s*export\s+(?:default\s+)?(?P<kind>class|function|interface|type)\s+(?P<name>[A-Za-z_$][A-Za-z0-9_$]*)"),
    re.compile(r"^\s*(?P<kind>class|function|interface|type)\s+(?P<name>[A-Za-z_$][A-Za-z0-9_$]*)"),
    re.compile(r"^\s*export\s+const\s+(?P<name>[A-Za-z_$][A-Za-z0-9_$]*)\s*="),
    re.compile(r"^\s*const\s+(?P<name>[A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*(?:async\s*)?\("),
]
SYMBOL_EXTENSIONS = {".py", ".ts", ".tsx", ".js", ".jsx"}


def build_symbol_index(
    repo_root: str | Path,
    *,
    paths: list[str] | None = None,
    max_files: int = 200,
) -> SymbolIndexArtifact:
    root = Path(repo_root).resolve()
    files = _candidate_files(root, paths=paths, max_files=max_files)
    symbol_files: list[SymbolFile] = []
    languages: set[str] = set()
    for file_path in files:
        relative = file_path.relative_to(root).as_posix()
        symbols = _symbols_for_file(file_path)
        if symbols:
            symbol_files.append(SymbolFile(path=relative, symbols=symbols))
        if file_path.suffix == ".py":
            languages.add("python")
        elif file_path.suffix in {".ts", ".tsx"}:
            languages.add("typescript")
        elif file_path.suffix in {".js", ".jsx"}:
            languages.add("javascript")
    return SymbolIndexArtifact(
        files=symbol_files,
        parser="tree_sitter" if _tree_sitter_available() else "regex_fallback",
        languages=sorted(languages),
        confidence="medium",
    )


def _candidate_files(root: Path, *, paths: list[str] | None, max_files: int) -> list[Path]:
    if paths:
        selected = []
        for item in paths:
            candidate = (root / item).resolve()
            try:
                candidate.relative_to(root)
            except ValueError:
                continue
            if candidate.is_file() and candidate.suffix.lower() in SYMBOL_EXTENSIONS:
                selected.append(candidate)
        return selected[:max_files]

    files: list[Path] = []
    for path in root.rglob("*"):
        if len(files) >= max_files:
            break
        if not path.is_file() or path.suffix.lower() not in SYMBOL_EXTENSIONS:
            continue
        relative_parts = path.relative_to(root).parts
        if any(part in DEFAULT_IGNORE_DIRS or part.endswith(".egg-info") for part in relative_parts):
            continue
        files.append(path)
    return files


def _symbols_for_file(path: Path) -> list[SymbolRecord]:
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError):
        return []
    if path.suffix == ".py":
        return _python_symbols(lines)
    return _typescript_symbols(lines)


def _python_symbols(lines: list[str]) -> list[SymbolRecord]:
    symbols: list[SymbolRecord] = []
    for index, line in enumerate(lines, start=1):
        match = PYTHON_SYMBOL_RE.match(line)
        if not match:
            continue
        kind = match.group("kind").replace("async ", "")
        symbols.append(SymbolRecord(name=match.group("name"), kind=kind, line=index))
    return symbols


def _typescript_symbols(lines: list[str]) -> list[SymbolRecord]:
    symbols: list[SymbolRecord] = []
    for index, line in enumerate(lines, start=1):
        for pattern in TS_SYMBOL_PATTERNS:
            match = pattern.match(line)
            if not match:
                continue
            kind = match.groupdict().get("kind") or "const"
            symbols.append(SymbolRecord(name=match.group("name"), kind=kind, line=index))
            break
    return symbols


def _tree_sitter_available() -> bool:
    try:
        import tree_sitter  # noqa: F401
    except Exception:
        return False
    return True
