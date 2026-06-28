from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
from typing import Any, Literal, get_args
from urllib.parse import urlparse

from pydantic import BaseModel, ConfigDict, Field

from .contracts import harness_contract_for_id
from .loops import HarnessLoopPhase, HarnessLoopTrace
from .openhands_provider import artifact_output_contract_available
from .permissions import evaluate_harness_permission
from .profiles import (
    LLMProviderProfile,
    OPENHANDS_PROVIDER_ID,
    default_harness_runtime_profiles,
    normalize_llm_model,
    resolve_llm_provider_profile,
)
from .runtime_context import HarnessRunRequest
from .sandbox import sandbox_policy_for_profile


DryRunStatus = Literal["ready", "warning", "blocked"]

_OPENHANDS_SDK_MODULES = (
    "openhands.sdk",
    "openhands.tools.file_editor",
    "openhands.tools.task_tracker",
    "openhands.tools.terminal",
)
_ARTIFACTS_BY_MODE: dict[str, set[str]] = {
    "planning_chat": {"project_plan_draft"},
    "workflow_supervisor": {"planner_order", "planner_decision", "final_report"},
    "task_execution": {"execution_result"},
}
_SECRET_ENV_NAMES = ("LLM_API_KEY", "DEEPSEEK_API_KEY", "OPENAI_API_KEY")
_SENSITIVE_KEY_MARKERS = (
    "api_key",
    "apikey",
    "secret",
    "token",
    "password",
    "credential_value",
    "private_key",
    "prompt",
    "model_output",
    "full_log",
    "full_diff",
)


class HarnessDryRunCheck(BaseModel):
    model_config = ConfigDict(extra="forbid")

    name: str
    status: DryRunStatus
    summary: str
    next_actions: list[str] = Field(default_factory=list)
    metadata: dict[str, Any] = Field(default_factory=dict)


class HarnessDryRunReport(BaseModel):
    model_config = ConfigDict(extra="forbid")

    status: DryRunStatus
    mode: str
    artifact_target: str | None = None
    provider_id: str | None = None
    profile_id: str | None = None
    checks: list[HarnessDryRunCheck] = Field(default_factory=list)
    next_actions: list[str] = Field(default_factory=list)


def run_harness_dry_run(request: HarnessRunRequest) -> HarnessDryRunReport:
    """Build a static readiness preview for a harness request.

    This function intentionally avoids provider construction, model calls, tool
    execution, sandbox preparation, and workspace copying.
    """

    artifact_target = _artifact_target_for_request(request)
    checks = [
        _artifact_target_check(request, artifact_target),
        _profile_binding_check(request, artifact_target),
        _openhands_sdk_check(request),
        _llm_provider_profile_check(request),
        _llm_credentials_check(request),
        _llm_model_check(request),
        _prompt_contract_check(),
        _sandbox_readiness_check(request),
        _permission_readiness_check(request),
        _trace_readiness_check(),
        _license_metadata_check(request),
    ]
    status = summarize_dry_run_status(checks)
    return HarnessDryRunReport(
        status=status,
        mode=request.mode,
        artifact_target=artifact_target,
        provider_id=request.profile.provider_id,
        profile_id=request.profile.id,
        checks=checks,
        next_actions=_dedupe([action for check in checks for action in check.next_actions]),
    )


def dry_run_harness_request(request: HarnessRunRequest) -> HarnessDryRunReport:
    return run_harness_dry_run(request)


def summarize_dry_run_status(checks: list[HarnessDryRunCheck]) -> DryRunStatus:
    if any(check.status == "blocked" for check in checks):
        return "blocked"
    if any(check.status == "warning" for check in checks):
        return "warning"
    return "ready"


def sanitize_dry_run_metadata(metadata: dict[str, Any]) -> dict[str, Any]:
    return _sanitize_dict(metadata, secret_values=_secret_values())


def _artifact_target_for_request(request: HarnessRunRequest) -> str | None:
    requested = request.input_artifacts.get("requested_artifact_type")
    if requested:
        return str(requested)
    legacy_operation = str(request.input_artifacts.get("legacy_operation") or "")
    mapped = {
        "planner_order": "planner_order",
        "planner_decision": "planner_decision",
        "final_report": "final_report",
        "task_execution": "execution_result",
        "planning_chat": "project_plan_draft",
    }.get(legacy_operation)
    if mapped:
        return mapped
    if request.mode == "task_execution":
        return "execution_result"
    if request.mode == "planning_chat":
        return "project_plan_draft"
    return "final_report"


def _artifact_target_check(request: HarnessRunRequest, artifact_target: str | None) -> HarnessDryRunCheck:
    allowed = _ARTIFACTS_BY_MODE.get(request.mode, set())
    if artifact_target in allowed:
        return _check(
            "artifact_target",
            "ready",
            "Requested artifact target is valid for the harness mode.",
            metadata={"allowed_artifacts": sorted(allowed), "artifact_target": artifact_target},
        )
    return _check(
        "artifact_target",
        "blocked",
        "Requested artifact target is not valid for the harness mode.",
        next_actions=["Use a valid artifact target for this harness mode."],
        metadata={"allowed_artifacts": sorted(allowed), "artifact_target": artifact_target},
    )


def _profile_binding_check(request: HarnessRunRequest, artifact_target: str | None) -> HarnessDryRunCheck:
    profiles = default_harness_runtime_profiles()
    profile = profiles.get(request.profile.id)
    if profile is None:
        return _check(
            "harness_profile_binding",
            "blocked",
            "Harness runtime profile is not registered.",
            next_actions=["Use a registered harness runtime profile."],
            metadata={"profile_id": request.profile.id},
        )
    issues: list[str] = []
    if request.context.profile_id and request.context.profile_id != request.profile.id:
        issues.append("context profile_id does not match request profile")
    if profile.provider_id != request.profile.provider_id:
        issues.append("provider_id does not match registered profile")
    if profile.harness_id != request.profile.harness_id:
        issues.append("harness_id does not match registered profile")
    if profile.mode != request.mode or request.profile.mode != request.mode:
        issues.append("profile mode does not match request mode")
    if artifact_target and artifact_target not in request.profile.allowed_artifacts:
        issues.append("artifact target is outside profile allowed_artifacts")
    try:
        contract = harness_contract_for_id(request.contract_id)
    except ValueError as exc:
        issues.append(str(exc))
        contract = None
    if contract is not None and request.profile.harness_id != contract.harness_id:
        issues.append("request contract does not match profile harness_id")

    if issues:
        return _check(
            "harness_profile_binding",
            "blocked",
            "Harness runtime profile does not match the request.",
            next_actions=["Use a profile whose provider, harness, mode, and allowed artifacts match the request."],
            metadata={
                "profile_id": request.profile.id,
                "provider_id": request.profile.provider_id,
                "harness_id": request.profile.harness_id,
                "mode": request.profile.mode,
                "allowed_artifacts": list(request.profile.allowed_artifacts),
                "issues": issues,
            },
        )
    return _check(
        "harness_profile_binding",
        "ready",
        "Harness runtime profile is registered and matches the request.",
        metadata={
            "profile_id": request.profile.id,
            "provider_id": request.profile.provider_id,
            "harness_id": request.profile.harness_id,
            "mode": request.profile.mode,
            "allowed_artifacts": list(request.profile.allowed_artifacts),
        },
    )


def _openhands_sdk_check(request: HarnessRunRequest) -> HarnessDryRunCheck:
    missing = [name for name in _OPENHANDS_SDK_MODULES if not _module_importable(name)]
    if not missing:
        return _check(
            "openhands_sdk_imports",
            "ready",
            "OpenHands SDK modules are importable.",
            metadata={"modules_checked": list(_OPENHANDS_SDK_MODULES), "missing_modules": []},
        )
    status: DryRunStatus = "blocked" if request.profile.provider_id == OPENHANDS_PROVIDER_ID else "warning"
    return _check(
        "openhands_sdk_imports",
        status,
        "OpenHands SDK modules are not fully importable.",
        next_actions=["Install openhands-sdk and required OpenHands tools packages."]
        if status == "blocked"
        else ["OpenHands SDK is unavailable; fallback provider readiness should be checked for execution."],
        metadata={"modules_checked": list(_OPENHANDS_SDK_MODULES), "missing_modules": missing},
    )


def _module_importable(name: str) -> bool:
    try:
        return importlib.util.find_spec(name) is not None
    except (ImportError, ModuleNotFoundError, ValueError):
        return False


def _llm_credentials_check(request: HarnessRunRequest) -> HarnessDryRunCheck:
    profile, error = _llm_profile()
    if error is not None or profile is None:
        return _check(
            "llm_credentials",
            "blocked",
            "LLM provider profile could not be resolved.",
            next_actions=["Set CODER_LLM_PROVIDER_PROFILE to a known provider profile."],
            metadata={"profile_error": error},
        )
    credential_source = _credential_source(profile.auth_env_candidates)
    if credential_source or request.profile.provider_id != OPENHANDS_PROVIDER_ID:
        return _check(
            "llm_credentials",
            "ready",
            "OpenHands LLM credential presence was checked.",
            metadata={
                "llm_profile_id": profile.id,
                "provider": profile.provider,
                "api_format": profile.api_format,
                "credential_env_candidates": list(profile.auth_env_candidates),
                "credential_source": credential_source,
                "credential_present": credential_source is not None,
            },
        )
    return _check(
        "llm_credentials",
        "blocked",
        "OpenHands LLM credentials are missing.",
        next_actions=[f"Set one of {', '.join(profile.auth_env_candidates)} in the current shell."],
        metadata={
            "llm_profile_id": profile.id,
            "provider": profile.provider,
            "api_format": profile.api_format,
            "credential_env_candidates": list(profile.auth_env_candidates),
            "credential_source": None,
            "credential_present": False,
        },
    )


def _llm_model_check(request: HarnessRunRequest) -> HarnessDryRunCheck:
    profile, error = _llm_profile()
    if error is not None or profile is None:
        return _check(
            "llm_model_base_url",
            "blocked",
            "LLM provider profile could not be resolved.",
            next_actions=["Set CODER_LLM_PROVIDER_PROFILE to a known provider profile."],
            metadata={"profile_error": error},
        )
    base_url = os.getenv("LLM_BASE_URL") or profile.base_url
    model = normalize_llm_model(os.getenv("LLM_MODEL") or profile.default_model, profile=profile, base_url=base_url)
    return _check(
        "llm_model_base_url",
        "ready",
        "OpenHands LLM model and base URL configuration were resolved without exposing credentials.",
        metadata={
            "llm_profile_id": profile.id,
            "provider": profile.provider,
            "api_format": profile.api_format,
            "model_configured": bool(os.getenv("LLM_MODEL")),
            "model": model,
            "base_url_configured": bool(os.getenv("LLM_BASE_URL")),
            "base_url_host": _url_host(base_url),
            "provider_id": request.profile.provider_id,
        },
    )


def _llm_provider_profile_check(request: HarnessRunRequest) -> HarnessDryRunCheck:
    profile, error = _llm_profile()
    if error is not None or profile is None:
        return _check(
            "llm_provider_profile",
            "blocked",
            "LLM provider profile could not be resolved.",
            next_actions=["Set CODER_LLM_PROVIDER_PROFILE to a known provider profile."],
            metadata={"profile_error": error, "provider_id": request.profile.provider_id},
        )
    return _check(
        "llm_provider_profile",
        "ready",
        "LLM provider profile resolved.",
        metadata={
            "llm_profile_id": profile.id,
            "provider": profile.provider,
            "api_format": profile.api_format,
            "auth_env_candidates": list(profile.auth_env_candidates),
            "default_model": profile.default_model,
            "base_url_host": _url_host(profile.base_url),
            "capabilities": dict(profile.capabilities),
        },
    )


def _prompt_contract_check() -> HarnessDryRunCheck:
    planner_order = artifact_output_contract_available("planner_order")
    execution_result = artifact_output_contract_available("execution_result")
    if planner_order and execution_result:
        return _check(
            "prompt_contracts",
            "ready",
            "Required structured output contracts are available.",
            metadata={"planner_order": True, "execution_result": True},
        )
    return _check(
        "prompt_contracts",
        "blocked",
        "Required structured output contracts are not available.",
        next_actions=["Restore planner_order and execution_result output contracts."],
        metadata={"planner_order": planner_order, "execution_result": execution_result},
    )


def _sandbox_readiness_check(request: HarnessRunRequest) -> HarnessDryRunCheck:
    policy = sandbox_policy_for_profile(request.profile)
    metadata: dict[str, Any] = {
        "workspace_mode": policy.workspace_mode,
        "repo_root_provided": bool(request.context.repo_root),
        "sandbox_root_provided": bool(request.context.sandbox_root),
    }
    if request.context.repo_root:
        repo_root = Path(request.context.repo_root)
        metadata["repo_root_exists"] = repo_root.exists() and repo_root.is_dir()
        if not metadata["repo_root_exists"]:
            return _check(
                "sandbox_readiness",
                "blocked",
                "Configured repo_root does not exist.",
                next_actions=["Provide an existing repo_root for harness execution."],
                metadata=metadata,
            )
    if request.context.sandbox_root:
        sandbox_root = Path(request.context.sandbox_root)
        metadata["sandbox_root_exists"] = sandbox_root.exists()
        metadata["sandbox_root_parent_exists"] = sandbox_root.parent.exists()

    if request.mode != "task_execution":
        return _check(
            "sandbox_readiness",
            "ready",
            "Conversation harness workspace readiness was checked without mutation.",
            metadata=metadata,
        )
    if policy.workspace_mode != "temp_worktree":
        return _check(
            "sandbox_readiness",
            "blocked",
            "Task Execution Harness requires a temp_worktree workspace policy.",
            next_actions=["Use a task execution profile with sandbox_policy.workspace=temp_worktree."],
            metadata=metadata,
        )
    if not request.context.repo_root and not request.context.sandbox_root:
        return _check(
            "sandbox_readiness",
            "blocked",
            "Task Execution Harness lacks repo_root or sandbox_root for workspace preparation.",
            next_actions=["Provide repo_root or sandbox_root for task execution."],
            metadata=metadata,
        )
    return _check(
        "sandbox_readiness",
        "ready",
        "Task execution sandbox requirements can be represented without preparing a workspace.",
        metadata=metadata,
    )


def _permission_readiness_check(request: HarnessRunRequest) -> HarnessDryRunCheck:
    sandbox_root = _permission_sandbox_root(request)
    planner_write = evaluate_harness_permission(
        mode="workflow_supervisor",
        harness_id="conversation-harness",
        role="planner",
        action_type="write_file",
        file_path="src/app.py",
        sandbox_root=sandbox_root,
    )
    executor_safe_path = evaluate_harness_permission(
        mode="task_execution",
        harness_id="task-execution-harness",
        role="executor",
        tool_name="file_editor",
        action_type="write_file",
        file_path="src/app.py",
        sandbox_root=sandbox_root,
    )
    executor_sensitive_path = evaluate_harness_permission(
        mode="task_execution",
        harness_id="task-execution-harness",
        role="executor",
        tool_name="file_editor",
        action_type="write_file",
        file_path=".env",
        sandbox_root=sandbox_root,
    )
    publish_command = evaluate_harness_permission(
        mode="task_execution",
        harness_id="task-execution-harness",
        role="executor",
        tool_name="terminal",
        action_type="run_command",
        command="git push origin main",
        sandbox_root=sandbox_root,
    )
    safe_command = evaluate_harness_permission(
        mode="task_execution",
        harness_id="task-execution-harness",
        role="executor",
        tool_name="terminal",
        action_type="run_command",
        command="python -m unittest discover tests",
        sandbox_root=sandbox_root,
    )
    expectations = {
        "planner_write_denied": not planner_write.allowed,
        "executor_safe_path_allowed": executor_safe_path.allowed,
        "executor_sensitive_path_denied": not executor_sensitive_path.allowed,
        "commit_push_deploy_denied": not publish_command.allowed,
        "safe_test_command_allowed": safe_command.allowed,
    }
    metadata = {
        "sandbox_root_available": sandbox_root is not None,
        "checks": {
            "planner_write": planner_write.code,
            "executor_safe_path": executor_safe_path.code,
            "executor_sensitive_path": executor_sensitive_path.code,
            "publish_command": publish_command.code,
            "safe_command": safe_command.code,
        },
        "expectations": expectations,
    }
    if all(expectations.values()):
        return _check(
            "permission_readiness",
            "ready",
            "Harness permission policy readiness checks passed in memory.",
            metadata=metadata,
        )
    return _check(
        "permission_readiness",
        "blocked",
        "Harness permission policy readiness checks did not match expected safety boundaries.",
        next_actions=["Restore HarnessPermissionChecker planner, executor, sensitive path, and publish command rules."],
        metadata=metadata,
    )


def _trace_readiness_check() -> HarnessDryRunCheck:
    phases = set(get_args(HarnessLoopPhase))
    required = {"started", "prompt_contract", "completed", "blocked"}
    metadata = {
        "trace_model_importable": HarnessLoopTrace is not None,
        "required_phases_present": sorted(required & phases),
        "missing_phases": sorted(required - phases),
    }
    if required <= phases:
        return _check("trace_readiness", "ready", "HarnessLoopTrace models and required phases are available.", metadata=metadata)
    return _check(
        "trace_readiness",
        "blocked",
        "HarnessLoopTrace required phases are missing.",
        next_actions=["Restore HarnessLoopTrace phase names."],
        metadata=metadata,
    )


def _license_metadata_check(request: HarnessRunRequest) -> HarnessDryRunCheck:
    root = _metadata_root(request)
    repo_root = _repo_metadata_root(root)
    legacy_root = _legacy_python_metadata_root(repo_root, root)
    license_text = _read_text(repo_root / "LICENSE")
    pyproject_text = _read_text(legacy_root / "pyproject.toml")
    frontend_package = _read_json(repo_root / "frontend" / "package.json")
    checks = {
        "license_mit": "mit license" in license_text.lower(),
        "pyproject_mit": 'license = "MIT"' in pyproject_text,
        "frontend_mit": frontend_package.get("license") == "MIT",
    }
    if all(checks.values()):
        return _check("license_metadata", "ready", "License metadata is consistent with MIT.", metadata=checks)
    return _check(
        "license_metadata",
        "warning",
        "License metadata is missing or inconsistent.",
        next_actions=["Check LICENSE, pyproject.toml, and frontend/package.json license metadata."],
        metadata=checks,
    )


def _check(
    name: str,
    status: DryRunStatus,
    summary: str,
    *,
    next_actions: list[str] | None = None,
    metadata: dict[str, Any] | None = None,
) -> HarnessDryRunCheck:
    return HarnessDryRunCheck(
        name=name,
        status=status,
        summary=summary,
        next_actions=next_actions or [],
        metadata=sanitize_dry_run_metadata(metadata or {}),
    )


def _credential_source(candidates: tuple[str, ...] | list[str]) -> str | None:
    for name in candidates:
        if os.getenv(name):
            return name
    return None


def _url_host(value: str | None) -> str | None:
    if not value:
        return None
    parsed = urlparse(value)
    if parsed.hostname:
        return parsed.hostname
    return None


def _permission_sandbox_root(request: HarnessRunRequest) -> str | None:
    if request.context.sandbox_root:
        return request.context.sandbox_root
    if request.context.repo_root:
        return str(Path(request.context.repo_root) / ".coder-dry-run-sandbox")
    return str(Path.cwd() / ".coder-dry-run-sandbox")


def _metadata_root(request: HarnessRunRequest) -> Path:
    if request.context.repo_root and Path(request.context.repo_root).exists():
        return Path(request.context.repo_root)
    return Path.cwd()


def _repo_metadata_root(root: Path) -> Path:
    root = root.resolve()
    if (root / "LICENSE").exists():
        return root
    if root.name == "legacy-python" and (root.parent / "LICENSE").exists():
        return root.parent
    return root


def _legacy_python_metadata_root(repo_root: Path, original_root: Path) -> Path:
    legacy_root = repo_root / "legacy-python"
    if (legacy_root / "pyproject.toml").exists():
        return legacy_root
    return original_root


def _read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except OSError:
        return ""


def _read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return value if isinstance(value, dict) else {}


def _secret_values() -> set[str]:
    return {value for name in _SECRET_ENV_NAMES if (value := os.getenv(name))}


def _llm_profile() -> tuple[LLMProviderProfile | None, str | None]:
    try:
        return resolve_llm_provider_profile(), None
    except ValueError as exc:
        return None, str(exc)


def _sanitize_dict(value: dict[str, Any], *, secret_values: set[str]) -> dict[str, Any]:
    clean: dict[str, Any] = {}
    for key, item in value.items():
        key_text = str(key)
        if _is_sensitive_metadata_key(key_text):
            clean[key_text] = "[redacted]"
            continue
        clean[key_text] = _sanitize_value(item, secret_values=secret_values)
    return clean


def _sanitize_value(value: Any, *, secret_values: set[str]) -> Any:
    if isinstance(value, dict):
        return _sanitize_dict(value, secret_values=secret_values)
    if isinstance(value, list):
        return [_sanitize_value(item, secret_values=secret_values) for item in value]
    if isinstance(value, tuple):
        return [_sanitize_value(item, secret_values=secret_values) for item in value]
    if isinstance(value, str):
        text = value
        for secret in secret_values:
            if secret and secret in text:
                text = text.replace(secret, "[redacted]")
        return text
    return value


def _is_sensitive_metadata_key(key: str) -> bool:
    normalized = key.strip().lower()
    return any(marker in normalized for marker in _SENSITIVE_KEY_MARKERS)


def _dedupe(values: list[str]) -> list[str]:
    seen: set[str] = set()
    output: list[str] = []
    for value in values:
        if value in seen:
            continue
        seen.add(value)
        output.append(value)
    return output


__all__ = [
    "DryRunStatus",
    "HarnessDryRunCheck",
    "HarnessDryRunReport",
    "dry_run_harness_request",
    "run_harness_dry_run",
    "sanitize_dry_run_metadata",
    "summarize_dry_run_status",
]
