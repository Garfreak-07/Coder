from __future__ import annotations

import hashlib
import json
from typing import Any

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.agent_model.compiler import RuntimeProfileCompiler
from coder_workbench.agent_model.profile import AgentRuntimeProfile


class RuntimeProfileCacheResult(BaseModel):
    model_config = ConfigDict(extra="forbid")

    profile_hash: str
    profiles: list[AgentRuntimeProfile]
    cache_hit: bool = False


class RuntimeProfileCache:
    def __init__(self) -> None:
        self._profiles_by_hash: dict[str, list[AgentRuntimeProfile]] = {}

    def get(self, profile_hash: str) -> list[AgentRuntimeProfile] | None:
        profiles = self._profiles_by_hash.get(profile_hash)
        if profiles is None:
            return None
        return [profile.model_copy(deep=True) for profile in profiles]

    def put(self, profile_hash: str, profiles: list[AgentRuntimeProfile]) -> None:
        self._profiles_by_hash[profile_hash] = [profile.model_copy(deep=True) for profile in profiles]

    def compile_or_get(
        self,
        workflow: Any,
        *,
        compiler: RuntimeProfileCompiler | None = None,
        installed_extensions: list[dict[str, Any]] | None = None,
        planner_setting: dict[str, Any] | None = None,
    ) -> RuntimeProfileCacheResult:
        profile_hash = runtime_profile_hash(
            workflow,
            installed_extensions=installed_extensions,
            planner_setting=planner_setting,
        )
        cached = self.get(profile_hash)
        if cached is not None:
            return RuntimeProfileCacheResult(profile_hash=profile_hash, profiles=cached, cache_hit=True)
        active_compiler = compiler or RuntimeProfileCompiler(installed_extensions=installed_extensions)
        profiles = active_compiler.compile_workflow(workflow)
        self.put(profile_hash, profiles)
        return RuntimeProfileCacheResult(profile_hash=profile_hash, profiles=profiles, cache_hit=False)


def runtime_profile_hash(
    workflow: Any,
    *,
    installed_extensions: list[dict[str, Any]] | None = None,
    planner_setting: dict[str, Any] | None = None,
) -> str:
    payload = {
        "workflow": workflow.model_dump(mode="json", by_alias=True, exclude_none=True)
        if hasattr(workflow, "model_dump")
        else workflow,
        "installed_extensions": _extension_versions(installed_extensions or []),
        "planner_setting": planner_setting or {},
    }
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _extension_versions(extensions: list[dict[str, Any]]) -> list[dict[str, str]]:
    versions: list[dict[str, str]] = []
    for extension in extensions:
        extension_id = str(extension.get("id") or extension.get("name") or "").strip()
        version = str(extension.get("version") or extension.get("package_sha256") or "").strip()
        if extension_id:
            versions.append({"id": extension_id, "version": version})
    return sorted(versions, key=lambda item: (item["id"], item["version"]))
