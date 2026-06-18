from __future__ import annotations

from copy import deepcopy
from typing import Any


BUILTIN_CAPABILITIES: list[dict[str, Any]] = [
    {
        "id": "project-planning",
        "name": "Project Planning",
        "kind": "skill_pack",
        "description": "Read the project index, reason about scope, and produce a safe implementation plan.",
        "tags": ["planning", "project", "scope", "default"],
        "skills": ["read_project_index", "reason_about_scope", "produce_plan"],
        "tools": ["read", "search"],
        "mcp_servers": [],
        "permissions": {"read_files": True},
        "risk": "low",
    },
    {
        "id": "code-review",
        "name": "Code Review",
        "kind": "skill_pack",
        "description": "Review plans or patches for scope escape, risk, and missing validation.",
        "tags": ["review", "safety", "risk", "default"],
        "skills": ["detect_scope_escape", "assess_risk", "produce_stop_reasons"],
        "tools": ["read", "search"],
        "mcp_servers": [],
        "permissions": {"read_files": True},
        "risk": "low",
    },
    {
        "id": "local-editing",
        "name": "Local Editing",
        "kind": "tool_pack",
        "description": "Allow scoped local file edits and patch generation after approval.",
        "tags": ["edit", "patch", "implementation", "coding"],
        "skills": ["apply_scoped_patch"],
        "tools": ["read", "search", "edit"],
        "mcp_servers": [],
        "permissions": {"read_files": True, "edit_files": True, "requires_approval": True},
        "risk": "medium",
    },
    {
        "id": "local-checks",
        "name": "Local Checks",
        "kind": "tool_pack",
        "description": "Run configured test, lint, or build commands with approval.",
        "tags": ["test", "lint", "build", "check", "validation"],
        "skills": ["choose_validation_checks", "interpret_check_output"],
        "tools": ["read", "search", "shell"],
        "mcp_servers": [],
        "permissions": {"read_files": True, "run_commands": True, "requires_approval": True},
        "risk": "medium",
    },
    {
        "id": "github-mcp",
        "name": "GitHub Connector",
        "kind": "mcp_bundle",
        "description": "Prepare a GitHub MCP server definition for issues, pull requests, and repository context.",
        "tags": ["github", "issue", "pull request", "pr", "mcp", "connector"],
        "skills": ["reason_about_github_context"],
        "tools": ["github"],
        "mcp_servers": [
            {
                "name": "github",
                "transport": "stdio",
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-github"],
                "env": ["GITHUB_TOKEN"],
            }
        ],
        "permissions": {"use_network": True, "requires_approval": True},
        "risk": "medium",
    },
]


def list_capabilities() -> list[dict[str, Any]]:
    return deepcopy(BUILTIN_CAPABILITIES)


def capability_by_id(capability_id: str) -> dict[str, Any] | None:
    for capability in BUILTIN_CAPABILITIES:
        if capability["id"] == capability_id:
            return deepcopy(capability)
    return None


def recommend_capabilities(
    *,
    query: str = "",
    agent: dict[str, Any] | None = None,
    modules: list[dict[str, Any]] | None = None,
) -> list[dict[str, Any]]:
    text = " ".join(
        [
            query,
            str((agent or {}).get("role", "")),
            str((agent or {}).get("goal", "")),
            " ".join(str(module.get("name", "")) for module in (modules or [])[:30]),
        ]
    ).lower()
    scored: list[tuple[int, dict[str, Any]]] = []
    for capability in BUILTIN_CAPABILITIES:
        score = 0
        for tag in capability.get("tags", []):
            if str(tag).lower() in text:
                score += 3
        for token in str(capability.get("description", "")).lower().replace(",", " ").split():
            if len(token) > 3 and token in text:
                score += 1
        if capability["id"] in {"project-planning", "code-review"}:
            score += 1
        if score > 0:
            item = deepcopy(capability)
            item["score"] = score
            scored.append((score, item))
    scored.sort(key=lambda item: (-item[0], item[1]["name"]))
    return [item for _, item in scored[:5]]


def apply_capabilities_to_agent(agent: dict[str, Any], capability_ids: list[str]) -> dict[str, Any]:
    updated = deepcopy(agent)
    runtime = updated.setdefault("runtime", {})
    permissions = runtime.setdefault("permissions", {})
    skills = set(updated.get("skills", [])) | set(runtime.get("skills", []))
    tools = set(runtime.get("tools", []))
    top_level_tools = set(updated.get("tools", []))
    mcp_servers = list(runtime.get("mcp_servers", []))
    known_mcp_names = {server.get("name") for server in mcp_servers if isinstance(server, dict)}

    enabled = []
    for capability_id in capability_ids:
        capability = capability_by_id(capability_id)
        if not capability:
            continue
        enabled.append(capability_id)
        skills.update(capability.get("skills", []))
        tools.update(capability.get("tools", []))
        top_level_tools.update(capability.get("tools", []))
        permissions.update(capability.get("permissions", {}))
        for server in capability.get("mcp_servers", []):
            name = server.get("name")
            if name and name not in known_mcp_names:
                mcp_servers.append(deepcopy(server))
                known_mcp_names.add(name)

    updated["skills"] = sorted(skills)
    updated["tools"] = sorted(top_level_tools | {"claude_code"})
    runtime["skills"] = sorted(skills)
    runtime["tools"] = sorted(tools or {"read", "search"})
    runtime["mcp_servers"] = mcp_servers
    runtime["permissions"] = permissions
    runtime["enabled_capabilities"] = enabled
    return updated
