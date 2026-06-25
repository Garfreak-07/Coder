from __future__ import annotations

from pathlib import Path
from typing import Any

from pydantic import BaseModel, ConfigDict

from coder_workbench.actions import ActionSpec, RunContext
from coder_workbench.agent_harness.action_protocol import HarnessActionRequest, HarnessObservation
from coder_workbench.agent_harness.tool_metadata import DEFAULT_CODE_WORKER_TOOL_METADATA
from coder_workbench.coding.command_policy import evaluate_command_policy
from coder_workbench.coding.risk_map import build_risk_map, is_risk_path
from coder_workbench.runtime_capabilities.registries import code_worker_tool_capabilities
from coder_workbench.tools.filesystem import resolve_scoped_path


ALLOWED_CODE_WORKER_ACTIONS = set(DEFAULT_CODE_WORKER_TOOL_METADATA)

DENIED_CODE_WORKER_ACTIONS = {
    "ask_user",
    "final_report",
    "planner_decision",
    "write_memory",
    "direct_memory_write",
    "external_publish",
    "push",
    "deploy",
    "install_plugin",
    "enable_mcp",
    "network_request",
    "secret_read",
    "run_command",
}


class ToolGateDecision(BaseModel):
    model_config = ConfigDict(arbitrary_types_allowed=True)

    allowed: bool
    reason: str = ""
    error_code: str | None = None
    action_spec: ActionSpec | None = None
    observation: HarnessObservation | None = None


class ToolGate:
    """Deny-by-default permission and scope gate for CodeWorker tool actions."""

    def __init__(
        self,
        *,
        run_context: RunContext,
        capability_set: dict[str, Any] | None = None,
    ) -> None:
        self.run_context = run_context
        self.capability_set = capability_set or {}
        self.repo_root = Path(run_context.repo_root).resolve()
        self.scopes = run_context.active_scopes
        self._risk_map: dict[str, Any] | None = None

    def decide(self, request: HarnessActionRequest) -> ToolGateDecision:
        action_type = request.action_type
        if action_type in DENIED_CODE_WORKER_ACTIONS:
            return self._blocked(
                request,
                "Action is outside CodeWorker authority.",
                "permission_boundary",
            )
        if action_type not in ALLOWED_CODE_WORKER_ACTIONS:
            return self._blocked(
                request,
                f"Unknown or unsupported CodeWorker action: {action_type}",
                "unknown_action_type",
            )
        if action_type not in self._capability_names():
            return self._blocked(
                request,
                f"Action {action_type} is not present in the current CapabilitySet.",
                "capability_denied",
            )

        blocked = self._scope_or_policy_blocker(request)
        if blocked is not None:
            reason, code = blocked
            return self._blocked(request, reason, code)

        if action_type == "return_execution_result":
            return ToolGateDecision(allowed=True, reason="Loop-control action accepted.")
        return ToolGateDecision(
            allowed=True,
            reason="Action accepted.",
            action_spec=ActionSpec(
                action_id=request.action_id,
                action_type=action_type,
                input=dict(request.payload),
                risk_level=request.risk_level,
                requires_permission=request.risk_level != "low",
            ),
        )

    def _scope_or_policy_blocker(self, request: HarnessActionRequest) -> tuple[str, str] | None:
        action_type = request.action_type
        payload = request.payload
        if action_type == "read_file":
            path = str(payload.get("path") or "").strip()
            if not path:
                return "read_file requires path.", "invalid_action_payload"
            return self._validate_path(path)
        if action_type == "search_files":
            for path in _string_list(payload.get("paths")):
                blocked = self._validate_path(path)
                if blocked is not None:
                    return blocked
            return None
        if action_type == "inspect_git_diff":
            for path in _string_list(payload.get("paths")):
                blocked = self._validate_path(path)
                if blocked is not None:
                    return blocked
            return None
        if action_type in {"propose_patch", "apply_patch_sandbox"}:
            for path in _patch_paths(payload):
                blocked = self._validate_path(path)
                if blocked is not None:
                    return blocked
                if is_risk_path(path, self._active_risk_map()):
                    return f"Patch action targets risk path: {path}", "risk_path_blocked"
            return None
        if action_type == "run_command_sandbox":
            return self._validate_command(payload, request.risk_level)
        return None

    def _validate_path(self, path: str) -> tuple[str, str] | None:
        try:
            resolve_scoped_path(self.repo_root, path, self.scopes)
        except ValueError as exc:
            message = str(exc)
            if "outside allowed scopes" in message or "escapes repo root" in message:
                return message, "scope_violation"
            return message, "invalid_path"
        except (FileNotFoundError, NotADirectoryError) as exc:
            return str(exc), "invalid_scope"
        return None

    def _validate_command(self, payload: dict[str, Any], risk_level: str) -> tuple[str, str] | None:
        if risk_level == "high":
            return "High-risk sandbox commands are blocked for CodeWorker.", "risk_path_blocked"
        command = str(payload.get("command") or "")
        argv = _string_list(payload.get("argv"))
        text = command or " ".join(argv)
        lower = text.lower()
        if any(token in lower for token in ("read-host", "pause", "input(", "--interactive")):
            return "Interactive command execution is blocked.", "permission_boundary"
        shell = bool(payload.get("shell")) if "shell" in payload else bool(command and not argv)
        policy = evaluate_command_policy(
            command=command,
            argv=argv,
            shell=shell,
            source="model",
            sandbox=True,
        )
        if policy.risk == "high":
            return policy.reason or "High-risk command is blocked.", "risk_path_blocked"
        cwd = str(payload.get("cwd") or ".")
        return self._validate_path(cwd)

    def _active_risk_map(self) -> dict[str, Any]:
        if self._risk_map is None:
            self._risk_map = build_risk_map(self.repo_root).model_dump(mode="json")
        return self._risk_map

    def _capability_names(self) -> set[str]:
        tools = self.capability_set.get("tools")
        if tools is None:
            return {tool.name for tool in code_worker_tool_capabilities()}
        if not isinstance(tools, list):
            return set()
        names: set[str] = set()
        for item in tools:
            if isinstance(item, dict):
                name = str(item.get("name") or "").strip()
            else:
                name = str(getattr(item, "name", "") or "").strip()
            if name:
                names.add(name)
        return names

    def _blocked(self, request: HarnessActionRequest, reason: str, error_code: str) -> ToolGateDecision:
        observation = HarnessObservation(
            action_id=request.action_id,
            action_type=request.action_type,
            status="blocked",
            summary=reason,
            evidence_refs=[f"harness_observation:{request.action_id}"],
            error_code=error_code,
        )
        return ToolGateDecision(
            allowed=False,
            reason=reason,
            error_code=error_code,
            observation=observation,
        )


def _string_list(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        return [value] if value.strip() else []
    if isinstance(value, list):
        return [str(item) for item in value if str(item).strip()]
    return [str(value)]


def _patch_paths(payload: dict[str, Any]) -> list[str]:
    paths: list[str] = []
    for item in _patch_items(payload):
        path = str(item.get("path") or item.get("file") or "").strip()
        if path:
            paths.append(path)
    return paths


def _patch_items(value: Any) -> list[dict[str, Any]]:
    if isinstance(value, dict):
        for key in ("changes", "proposed_changes", "files", "patches"):
            if isinstance(value.get(key), list):
                return [dict(item) for item in value[key] if isinstance(item, dict)]
        patch = value.get("patch")
        if isinstance(patch, dict):
            return _patch_items(patch)
        if "path" in value or "file" in value:
            return [dict(value)]
        return []
    if isinstance(value, list):
        return [dict(item) for item in value if isinstance(item, dict)]
    return []
