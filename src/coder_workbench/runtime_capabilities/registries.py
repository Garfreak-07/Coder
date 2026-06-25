from __future__ import annotations

from pathlib import Path
from typing import Any

from coder_workbench.skills import InstalledSkillStore

from .schema import (
    DeniedCapability,
    McpManifestOperation,
    McpManifestValidation,
    McpServerManifest,
    MemoryScope,
    ToolCapability,
    ToolRegistryEntry,
)


PLANNER_TOOL_CAPABILITIES = [
    ToolCapability(name="inspect_workflow", toolset="workflow", side_effect="read", risk="low"),
    ToolCapability(name="inspect_project_summary", toolset="context", side_effect="read", risk="low"),
    ToolCapability(name="inspect_artifact", toolset="runtime_state", side_effect="read", risk="low"),
    ToolCapability(name="inspect_run_state", toolset="runtime_state", side_effect="read", risk="low"),
    ToolCapability(name="inspect_round_summary", toolset="runtime_state", side_effect="read", risk="low"),
    ToolCapability(name="inspect_evidence", toolset="evidence", side_effect="read", risk="low"),
    ToolCapability(name="inspect_skill_index", toolset="skills", side_effect="read", risk="low"),
    ToolCapability(name="inspect_memory", toolset="memory", side_effect="read", risk="low"),
    ToolCapability(name="read_skill_index", toolset="skills", side_effect="read", risk="low"),
    ToolCapability(name="search_workflow_memory", toolset="memory", side_effect="read", risk="low"),
    ToolCapability(name="search_project_memory", toolset="memory", side_effect="read", risk="low"),
    ToolCapability(name="validate_run_contract_draft", toolset="artifacts", side_effect="none", risk="low"),
    ToolCapability(name="validate_planner_order", toolset="artifacts", side_effect="none", risk="low"),
    ToolCapability(name="validate_planner_decision", toolset="artifacts", side_effect="none", risk="low"),
    ToolCapability(name="build_final_report", toolset="artifacts", side_effect="none", risk="low"),
    ToolCapability(name="estimate_risk", toolset="runtime_policy", side_effect="none", risk="low"),
    ToolCapability(name="estimate_budget", toolset="runtime_policy", side_effect="none", risk="low"),
]

CODE_WORKER_TOOL_CAPABILITIES = [
    ToolCapability(name="read_file", toolset="filesystem", side_effect="read", risk="low"),
    ToolCapability(name="search_files", toolset="filesystem", side_effect="read", risk="low"),
    ToolCapability(name="inspect_git_diff", toolset="git", side_effect="read", risk="low"),
    ToolCapability(name="propose_patch", toolset="filesystem", side_effect="write", risk="medium"),
    ToolCapability(name="apply_patch_sandbox", toolset="filesystem", side_effect="write", risk="medium"),
    ToolCapability(name="run_command_sandbox", toolset="commands", side_effect="external", risk="medium"),
    ToolCapability(name="read_tool_output", toolset="runtime_state", side_effect="read", risk="low"),
    ToolCapability(name="return_execution_result", toolset="artifacts", side_effect="none", risk="low"),
]

PLANNER_DENIED_CAPABILITIES = [
    DeniedCapability(name="apply_patch", reason="Planner does not perform file side effects."),
    DeniedCapability(name="write_file", reason="Planner delegates file changes to executor harnesses."),
    DeniedCapability(name="run_command", reason="Planner does not run shell commands directly."),
    DeniedCapability(name="push", reason="External publishing must stay outside model harnesses."),
    DeniedCapability(name="deploy", reason="Deployment is not a planner harness capability."),
    DeniedCapability(name="write_long_term_memory_direct", reason="Memory writes must go through MemoryService or ActionGateway."),
    DeniedCapability(name="install_plugin", reason="Plugin installation requires an explicit user-controlled flow."),
    DeniedCapability(name="enable_mcp", reason="MCP enablement is not performed by harnesses."),
]

CODE_WORKER_DENIED_CAPABILITIES = [
    DeniedCapability(name="ask_user", reason="Executor cannot talk to the user directly."),
    DeniedCapability(name="final_report", reason="Only Planner final-report harness produces final reports."),
    DeniedCapability(name="write_long_term_memory_direct", reason="Executor cannot write long-term memory directly."),
    DeniedCapability(name="install_plugin", reason="Plugin installation requires an explicit user-controlled flow."),
    DeniedCapability(name="enable_mcp", reason="MCP enablement is not performed by harnesses."),
    DeniedCapability(name="push", reason="External publishing must stay outside model harnesses."),
    DeniedCapability(name="deploy", reason="Deployment is not an executor harness capability."),
]

PLANNER_MEMORY_SCOPES = [
    MemoryScope(scope="workflow", access="search"),
    MemoryScope(scope="project", access="search"),
]

CODE_WORKER_MEMORY_SCOPES = [
    MemoryScope(scope="project", access="read"),
]

TOOL_REGISTRY_ENTRIES = [
    *[
        ToolRegistryEntry(
            capability=tool,
            description=f"Planner harness tool: {tool.name}.",
            harness_ids=["conversation-harness", "planner-order-harness", "planner-decision-harness", "final-report-harness"],
            requires_approval=tool.risk != "low" or tool.side_effect in {"write", "external"},
        )
        for tool in PLANNER_TOOL_CAPABILITIES
    ],
    *[
        ToolRegistryEntry(
            capability=tool,
            description=f"Code worker harness tool: {tool.name}.",
            harness_ids=["task-execution-harness", "code-worker-harness"],
            requires_approval=tool.risk != "low" or tool.side_effect in {"write", "external"},
        )
        for tool in CODE_WORKER_TOOL_CAPABILITIES
    ],
]


class ToolRegistry:
    def __init__(self, entries: list[ToolRegistryEntry] | None = None) -> None:
        self._entries = entries or tool_registry_entries()

    def list_tools(self, *, harness_id: str | None = None) -> list[ToolRegistryEntry]:
        if harness_id is None:
            return [entry.model_copy(deep=True) for entry in self._entries]
        return [
            entry.model_copy(deep=True)
            for entry in self._entries
            if harness_id in entry.harness_ids
        ]

    def get_tool(self, name: str) -> ToolRegistryEntry:
        for entry in self._entries:
            if entry.capability.name == name:
                return entry.model_copy(deep=True)
        raise KeyError(name)


class ProgressiveSkillRegistry:
    def __init__(self, store: InstalledSkillStore) -> None:
        self.store = store

    def list_skill_summaries(self) -> list[dict[str, object]]:
        return self.store.list_summaries()

    def load_skill_full(self, skill_id: str) -> dict[str, object]:
        record = self.store.get_skill(skill_id)
        return {
            "skill_id": skill_id,
            "manifest": record.manifest.model_dump(mode="json"),
            "body": self.store.read_skill_body(skill_id),
        }

    def load_skill_reference(self, skill_id: str, reference_path: str) -> dict[str, object]:
        root = self.store.skill_root(skill_id).resolve()
        refs_root = (root / "references").resolve()
        target = (refs_root / reference_path).resolve()
        if refs_root not in target.parents and target != refs_root:
            raise KeyError(reference_path)
        if not target.exists() or not target.is_file():
            raise KeyError(reference_path)
        return {
            "skill_id": skill_id,
            "reference_path": str(target.relative_to(root)).replace("\\", "/"),
            "content": target.read_text(encoding="utf-8"),
        }


def planner_tool_capabilities() -> list[ToolCapability]:
    return [tool.model_copy(deep=True) for tool in PLANNER_TOOL_CAPABILITIES]


def code_worker_tool_capabilities() -> list[ToolCapability]:
    return [tool.model_copy(deep=True) for tool in CODE_WORKER_TOOL_CAPABILITIES]


def planner_denied_capabilities() -> list[DeniedCapability]:
    return [capability.model_copy(deep=True) for capability in PLANNER_DENIED_CAPABILITIES]


def code_worker_denied_capabilities() -> list[DeniedCapability]:
    return [capability.model_copy(deep=True) for capability in CODE_WORKER_DENIED_CAPABILITIES]


def planner_memory_scopes() -> list[MemoryScope]:
    return [scope.model_copy(deep=True) for scope in PLANNER_MEMORY_SCOPES]


def code_worker_memory_scopes() -> list[MemoryScope]:
    return [scope.model_copy(deep=True) for scope in CODE_WORKER_MEMORY_SCOPES]


def tool_registry_entries() -> list[ToolRegistryEntry]:
    return [entry.model_copy(deep=True) for entry in TOOL_REGISTRY_ENTRIES]


def parse_mcp_manifest(raw: dict[str, Any]) -> McpServerManifest:
    server_id = str(raw.get("server_id") or raw.get("id") or "").strip()
    name = str(raw.get("name") or server_id).strip()
    raw_operations = raw.get("operations", raw.get("tools", []))
    operations: list[McpManifestOperation] = []
    if isinstance(raw_operations, list):
        for item in raw_operations:
            if not isinstance(item, dict):
                continue
            operation_name = str(item.get("name") or item.get("operation") or item.get("id") or "").strip()
            if not operation_name:
                continue
            operations.append(
                McpManifestOperation(
                    name=operation_name,
                    description=str(item.get("description") or ""),
                    risk=str(item.get("risk") or item.get("risk_level") or "medium"),  # type: ignore[arg-type]
                    side_effect=str(item.get("side_effect") or "external"),  # type: ignore[arg-type]
                    enabled_by_default=bool(item.get("enabled_by_default", False)),
                )
            )
    return McpServerManifest(
        server_id=server_id,
        name=name,
        operations=operations,
        enabled_by_default=bool(raw.get("enabled_by_default", False)),
    )


def validate_mcp_manifest(raw: dict[str, Any]) -> McpManifestValidation:
    errors: list[str] = []
    warnings: list[str] = []
    try:
        manifest = parse_mcp_manifest(raw)
    except Exception as exc:
        return McpManifestValidation(ok=False, errors=[str(exc)])

    if not manifest.server_id:
        errors.append("server_id is required")
    if not manifest.operations:
        errors.append("at least one operation is required")
    if manifest.enabled_by_default:
        warnings.append("MCP servers are not enabled by default; explicit user approval is required")
        manifest = manifest.model_copy(update={"enabled_by_default": False})
    operations: list[McpManifestOperation] = []
    for operation in manifest.operations:
        if operation.enabled_by_default:
            warnings.append(f"operation {operation.name} default enablement was disabled")
            operation = operation.model_copy(update={"enabled_by_default": False})
        operations.append(operation)
    manifest = manifest.model_copy(update={"operations": operations})
    return McpManifestValidation(ok=not errors, errors=errors, warnings=warnings, manifest=manifest)


def progressive_skill_registry(root: str | Path) -> ProgressiveSkillRegistry:
    return ProgressiveSkillRegistry(InstalledSkillStore(root))


__all__ = [
    "ProgressiveSkillRegistry",
    "ToolRegistry",
    "code_worker_denied_capabilities",
    "code_worker_memory_scopes",
    "code_worker_tool_capabilities",
    "parse_mcp_manifest",
    "planner_denied_capabilities",
    "planner_memory_scopes",
    "planner_tool_capabilities",
    "progressive_skill_registry",
    "tool_registry_entries",
    "validate_mcp_manifest",
]
