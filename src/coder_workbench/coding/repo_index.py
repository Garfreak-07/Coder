from __future__ import annotations

import json
from pathlib import Path
from typing import Any

try:  # pragma: no cover - Python 3.11 path
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python 3.10 local fallback
    tomllib = None  # type: ignore[assignment]

from coder_workbench.tools.filesystem import summarize_project

from .artifacts import RepoIndexArtifact
from .command_discovery import discover_commands
from .risk_map import build_risk_map
from .symbol_index import build_symbol_index


LANGUAGE_BY_SUFFIX = {
    ".py": "python",
    ".ts": "typescript",
    ".tsx": "typescript",
    ".js": "javascript",
    ".jsx": "javascript",
    ".go": "go",
    ".rs": "rust",
}


def build_repo_index(repo_root: str | Path, *, max_files: int = 1000) -> RepoIndexArtifact:
    root = Path(repo_root).resolve()
    files = summarize_project(root, max_files=max_files)
    paths = {str(item.get("path") or "") for item in files}
    languages = _detect_languages(paths)
    frameworks = _detect_frameworks(root, paths)
    source_dirs = _existing_dirs(root, ["src", "frontend/src", "app", "lib", "packages"])
    test_dirs = _existing_dirs(root, ["tests", "test", "frontend/tests", "frontend/src/__tests__"])
    important_files = _important_files(paths)
    package_managers = _package_managers(root)
    risk_map = build_risk_map(root)
    confidence = "high" if important_files or source_dirs else "medium"
    return RepoIndexArtifact(
        languages=languages,
        frameworks=frameworks,
        source_dirs=source_dirs,
        test_dirs=test_dirs,
        important_files=important_files,
        risk_files=risk_map.risk_files,
        package_managers=package_managers,
        file_count=len(files),
        confidence=confidence,
    )


def build_repo_intelligence(repo_root: str | Path, *, max_symbol_files: int = 200) -> dict[str, Any]:
    root = Path(repo_root).resolve()
    repo_index = build_repo_index(root)
    command_discovery = discover_commands(root)
    risk_map = build_risk_map(root)
    symbol_index = build_symbol_index(root, max_files=max_symbol_files)
    return {
        "repo_index": repo_index.model_dump(mode="json"),
        "command_discovery": command_discovery.model_dump(mode="json"),
        "risk_map": risk_map.model_dump(mode="json"),
        "symbol_index": symbol_index.model_dump(mode="json"),
    }


def _detect_languages(paths: set[str]) -> list[str]:
    languages = {LANGUAGE_BY_SUFFIX[Path(path).suffix.lower()] for path in paths if Path(path).suffix.lower() in LANGUAGE_BY_SUFFIX}
    return sorted(languages)


def _detect_frameworks(root: Path, paths: set[str]) -> list[str]:
    frameworks: set[str] = set()
    pyproject = _read_pyproject(root / "pyproject.toml")
    dependencies = " ".join(str(item).lower() for item in pyproject.get("project", {}).get("dependencies", []))
    if not dependencies and (root / "pyproject.toml").exists():
        dependencies = _read_lower_text(root / "pyproject.toml")
    if "fastapi" in dependencies:
        frameworks.add("fastapi")
    if "pydantic" in dependencies:
        frameworks.add("pydantic")
    for package_path in sorted(path for path in paths if path == "package.json" or path.endswith("/package.json")):
        package = _read_package_json(root / package_path)
        deps = {
            **(package.get("dependencies") if isinstance(package.get("dependencies"), dict) else {}),
            **(package.get("devDependencies") if isinstance(package.get("devDependencies"), dict) else {}),
        }
        if "react" in deps:
            frameworks.add("react")
        if "vite" in deps or any(path.endswith("vite.config.ts") or path.endswith("vite.config.js") for path in paths):
            frameworks.add("vite")
        if "next" in deps:
            frameworks.add("nextjs")
    if "pyproject.toml" in paths or "requirements.txt" in paths:
        frameworks.add("python")
    if any(path == "package.json" or path.endswith("/package.json") for path in paths):
        frameworks.add("node")
    return sorted(frameworks)


def _existing_dirs(root: Path, candidates: list[str]) -> list[str]:
    return [path for path in candidates if (root / path).is_dir()]


def _important_files(paths: set[str]) -> list[str]:
    names = {
        "README.md",
        "pyproject.toml",
        "requirements.txt",
        "setup.cfg",
        "pytest.ini",
        "package.json",
        "package-lock.json",
        "tsconfig.json",
        "vite.config.ts",
        "vite.config.js",
        ".gitignore",
    }
    important = [
        path
        for path in paths
        if path in names
        or path.endswith("/package.json")
        or path.endswith("/package-lock.json")
        or path.endswith("/tsconfig.json")
        or path.endswith("/vite.config.ts")
        or path.endswith("/vite.config.js")
    ]
    return sorted(important)


def _package_managers(root: Path) -> list[str]:
    managers: set[str] = set()
    if (root / "pyproject.toml").exists() or (root / "requirements.txt").exists():
        managers.add("pip")
    if (root / "package-lock.json").exists() or any(root.glob("*/package-lock.json")):
        managers.add("npm")
    if (root / "pnpm-lock.yaml").exists():
        managers.add("pnpm")
    if (root / "yarn.lock").exists():
        managers.add("yarn")
    return sorted(managers)


def _read_pyproject(path: Path) -> dict[str, Any]:
    if not path.exists() or tomllib is None:
        return {}
    try:
        with path.open("rb") as handle:
            return tomllib.load(handle)
    except Exception:
        return {}


def _read_package_json(path: Path) -> dict[str, Any]:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}


def _read_lower_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8").lower()
    except OSError:
        return ""
