from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Literal

from coder_workbench.project_index import annotate_recommendations, build_project_modules, recommend_modules
from coder_workbench.tools.mcp import call_mcp_tool
from coder_workbench.tools.filesystem import summarize_project


ToolFn = Callable[[dict[str, Any], dict[str, Any]], dict[str, Any]]
RiskLevel = Literal["low", "medium", "high"]
PermissionKey = Literal["read_files", "edit_files", "run_commands", "use_network"]


@dataclass(frozen=True)
class ToolCapability:
    id: str
    display_name: str
    description: str = ""
    risk_level: RiskLevel = "low"
    permissions: tuple[PermissionKey, ...] = ()
    requires_approval: bool = False

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "display_name": self.display_name,
            "description": self.description,
            "risk_level": self.risk_level,
            "permissions": list(self.permissions),
            "requires_approval": self.requires_approval,
        }


class ToolRegistry:
    def __init__(self) -> None:
        self._tools: dict[str, ToolFn] = {}
        self._capabilities: dict[str, ToolCapability] = {}

    def register(self, name: str, fn: ToolFn, capability: ToolCapability | None = None) -> None:
        self._tools[name] = fn
        self._capabilities[name] = capability or ToolCapability(id=name, display_name=name)

    def run(self, name: str, args: dict[str, Any], runtime_context: dict[str, Any]) -> dict[str, Any]:
        if name not in self._tools:
            raise ValueError(f"Unknown tool: {name}")
        return self._tools[name](args, runtime_context)

    def names(self) -> list[str]:
        return sorted(self._tools)

    def capability(self, name: str) -> ToolCapability | None:
        return self._capabilities.get(name)

    def capabilities(self) -> dict[str, dict[str, Any]]:
        return {name: capability.to_dict() for name, capability in sorted(self._capabilities.items())}


def default_tool_registry() -> ToolRegistry:
    registry = ToolRegistry()
    registry.register(
        "project_index",
        _project_index,
        ToolCapability(
            id="project_index",
            display_name="Project index",
            description="Read scoped project files and summarize the local project.",
            risk_level="low",
            permissions=("read_files",),
        ),
    )
    registry.register(
        "recommend_modules",
        _recommend_modules,
        ToolCapability(
            id="recommend_modules",
            display_name="Recommend modules",
            description="Rank indexed project modules for a request.",
            risk_level="low",
        ),
    )
    registry.register(
        "dry_run_patch",
        _dry_run_patch,
        ToolCapability(
            id="dry_run_patch",
            display_name="Patch dry run",
            description="Preview a scoped patch without writing files.",
            risk_level="medium",
            permissions=("read_files",),
            requires_approval=True,
        ),
    )
    registry.register(
        "propose_patch",
        _propose_patch,
        ToolCapability(
            id="propose_patch",
            display_name="Propose patch",
            description="Build a scoped patch preview before file mutation.",
            risk_level="medium",
            permissions=("read_files", "edit_files"),
            requires_approval=True,
        ),
    )
    registry.register(
        "apply_patch",
        _apply_patch,
        ToolCapability(
            id="apply_patch",
            display_name="Apply patch",
            description="Apply an approved scoped patch to local files.",
            risk_level="high",
            permissions=("edit_files",),
            requires_approval=True,
        ),
    )
    registry.register(
        "rollback_patch",
        _rollback_patch,
        ToolCapability(
            id="rollback_patch",
            display_name="Rollback patch",
            description="Restore files from a saved patch snapshot.",
            risk_level="high",
            permissions=("edit_files",),
            requires_approval=True,
        ),
    )
    registry.register(
        "run_check",
        _run_check,
        ToolCapability(
            id="run_check",
            display_name="Run check",
            description="Run an approved local command in a scoped working directory.",
            risk_level="high",
            permissions=("run_commands",),
            requires_approval=True,
        ),
    )
    registry.register(
        "mcp_call",
        call_mcp_tool,
        ToolCapability(
            id="mcp_call",
            display_name="MCP stdio call",
            description="Launch or call an MCP stdio server tool after approval.",
            risk_level="high",
            permissions=("run_commands",),
            requires_approval=True,
        ),
    )
    return registry


def _project_index(args: dict[str, Any], runtime_context: dict[str, Any]) -> dict[str, Any]:
    repo_root = Path(runtime_context["repo_root"]).resolve()
    scope = _list_value(args.get("scope")) or _list_value(runtime_context.get("scopes"))
    files = summarize_project(repo_root, scope, max_files=int(args.get("max_files", 800)))
    modules = build_project_modules(files)
    summary = _project_summary(repo_root, files, scope)
    return {
        "files": files,
        "modules": modules,
        "file_count": len(files),
        "scopes": scope,
        "summary": summary,
        "important_files": summary["important_files"],
        "detected_frameworks": summary["detected_frameworks"],
        "candidate_check_commands": summary["candidate_check_commands"],
    }


def _recommend_modules(args: dict[str, Any], runtime_context: dict[str, Any]) -> dict[str, Any]:
    query = str(args.get("query") or runtime_context.get("request") or "")
    files = args.get("files") or runtime_context.get("data", {}).get("project_index", {}).get("files", [])
    modules = args.get("modules") or runtime_context.get("data", {}).get("project_index", {}).get("modules", [])
    recommendations = recommend_modules(query, modules, files) if query else []
    annotated = annotate_recommendations(modules, recommendations) if recommendations else modules
    return {"query": query, "recommendations": recommendations, "modules": annotated}


def _dry_run_patch(args: dict[str, Any], runtime_context: dict[str, Any]) -> dict[str, Any]:
    preview = _patch_service(runtime_context).preview(args)
    if preview.get("status") == "blocked":
        return preview
    return preview | {
        "status": "dry_run",
        "message": "Patch preview generated without applying files.",
    }


def _propose_patch(args: dict[str, Any], runtime_context: dict[str, Any]) -> dict[str, Any]:
    return _patch_service(runtime_context).preview(args)


def _apply_patch(args: dict[str, Any], runtime_context: dict[str, Any]) -> dict[str, Any]:
    patch = args.get("patch") or runtime_context.get("data", {}).get(str(args.get("patch_key") or "patch_preview")) or args
    return _patch_service(runtime_context).apply(
        patch,
        approved=bool(args.get("approved") or args.get("patch_approved")),
    )


def _rollback_patch(args: dict[str, Any], runtime_context: dict[str, Any]) -> dict[str, Any]:
    snapshot_id = str(args.get("snapshot_id") or "").strip()
    if not snapshot_id:
        patch_result = args.get("patch_result") or runtime_context.get("data", {}).get("patch_apply")
        if isinstance(patch_result, dict):
            snapshot_id = str(patch_result.get("snapshot_id") or "").strip()
    return _patch_service(runtime_context).rollback(snapshot_id)


def _patch_service(runtime_context: dict[str, Any]):
    from coder_workbench.coding.patch_service import PatchService

    return PatchService(
        runtime_context["repo_root"],
        scopes=_list_value(runtime_context.get("scopes")),
        data=runtime_context.get("data") if isinstance(runtime_context.get("data"), dict) else {},
    )


def _run_check(args: dict[str, Any], runtime_context: dict[str, Any]) -> dict[str, Any]:
    from coder_workbench.coding.command_service import CommandService

    command = str(args.get("command") or "").strip()
    argv_input = args.get("argv")
    argv = [str(item) for item in argv_input] if isinstance(argv_input, list) else None
    if not command and not argv:
        return {"passed": True, "output": "No check command configured.", "skipped": True}
    repo_root = Path(runtime_context["repo_root"]).resolve()
    scopes = _list_value(runtime_context.get("scopes"))
    default_cwd = scopes[0] if scopes else "."
    return CommandService(
        repo_root,
        scopes=scopes,
        data=runtime_context.get("data") if isinstance(runtime_context.get("data"), dict) else {},
    ).run_check(
        command,
        argv=argv,
        cwd=str(args.get("cwd") or default_cwd),
        timeout_seconds=int(args.get("timeout_seconds", 120)),
        shell=args.get("shell"),
        source=str(args.get("source") or "model"),
        sandbox=bool(args.get("sandbox")),
    )


def _list_value(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        return [value] if value.strip() else []
    if isinstance(value, list):
        return [str(item) for item in value if str(item).strip()]
    return [str(value)]


def _project_summary(repo_root: Path, files: list[dict[str, Any]], scopes: list[str]) -> dict[str, Any]:
    paths = {str(file.get("path", "")) for file in files}
    important_names = {
        "README.md",
        "pyproject.toml",
        "requirements.txt",
        "package.json",
        "frontend/package.json",
        "Cargo.toml",
        "go.mod",
        ".github/workflows/ci.yml",
    }
    important_files = sorted(path for path in paths if path in important_names or path.endswith(("/package.json", "/pyproject.toml")))
    frameworks = _detect_frameworks(repo_root, paths)
    candidate_checks = _candidate_check_commands(repo_root, paths)
    return {
        "file_count": len(files),
        "scopes": scopes,
        "important_files": important_files[:20],
        "detected_frameworks": frameworks,
        "candidate_check_commands": candidate_checks,
    }


def _detect_frameworks(repo_root: Path, paths: set[str]) -> list[str]:
    frameworks: list[str] = []
    if "pyproject.toml" in paths or "requirements.txt" in paths:
        frameworks.append("python")
    package_paths = [path for path in paths if path == "package.json" or path.endswith("/package.json")]
    if package_paths:
        frameworks.append("node")
    if any(path.endswith("vite.config.ts") or path.endswith("vite.config.js") for path in paths):
        frameworks.append("vite")
    if any(path.endswith("next.config.js") or path.endswith("next.config.mjs") or path.endswith("next.config.ts") for path in paths):
        frameworks.append("nextjs")
    if "Cargo.toml" in paths:
        frameworks.append("rust")
    if "go.mod" in paths:
        frameworks.append("go")
    if ".github/workflows/ci.yml" in paths or any(path.startswith(".github/workflows/") for path in paths):
        frameworks.append("github_actions")
    return frameworks


def _candidate_check_commands(repo_root: Path, paths: set[str]) -> list[str]:
    commands: list[str] = []
    if "tests" in {Path(path).parts[0] for path in paths if Path(path).parts}:
        commands.append("python -m unittest discover -s tests")
    if "src" in {Path(path).parts[0] for path in paths if Path(path).parts}:
        commands.append("python -m compileall src tests")
    for package_path in sorted(path for path in paths if path == "package.json" or path.endswith("/package.json")):
        package_dir = "." if package_path == "package.json" else str(Path(package_path).parent).replace("\\", "/")
        scripts = _package_scripts(repo_root / package_path)
        prefix = "" if package_dir == "." else f"--prefix {package_dir} "
        if "test" in scripts:
            commands.append(f"npm {prefix}run test".strip())
        if "build" in scripts:
            commands.append(f"npm {prefix}run build".strip())
    if "Cargo.toml" in paths:
        commands.append("cargo test")
    if "go.mod" in paths:
        commands.append("go test ./...")
    return _dedupe(commands)


def _package_scripts(path: Path) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    scripts = payload.get("scripts")
    return scripts if isinstance(scripts, dict) else {}


def _dedupe(values: list[str]) -> list[str]:
    seen: set[str] = set()
    result: list[str] = []
    for value in values:
        if value in seen:
            continue
        seen.add(value)
        result.append(value)
    return result
