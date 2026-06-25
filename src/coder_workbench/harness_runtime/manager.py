from __future__ import annotations

import os
import uuid
from typing import Any

from .contracts import CONVERSATION_HARNESS_ID, TASK_EXECUTION_HARNESS_ID, HarnessMode, harness_contract_for_id
from .fallback_provider import InternalFallbackProvider
from .openhands_provider import OpenHandsRuntimeProvider
from .profiles import (
    INTERNAL_FALLBACK_PROVIDER_ID,
    OPENHANDS_PROVIDER_ID,
    HarnessRuntimeProfile,
    default_harness_runtime_profiles,
)
from .providers import HarnessProvider
from .runtime_context import HarnessRunRequest, HarnessRunResult, HarnessRuntimeContext
from .safety import enforce_harness_safety
from .sandbox import enforce_sandbox_policy


class HarnessRuntimeManager:
    """Central entrypoint for running canonical harness modes through providers."""

    def __init__(
        self,
        *,
        profiles: dict[str, HarnessRuntimeProfile] | None = None,
        providers: list[HarnessProvider] | None = None,
    ) -> None:
        self.profiles = profiles or default_harness_runtime_profiles()
        installed = providers or [OpenHandsRuntimeProvider(), InternalFallbackProvider()]
        self.providers = {provider.provider_id: provider for provider in installed}

    def run(self, request: HarnessRunRequest, *, emit: Any | None = None) -> HarnessRunResult:
        self._validate_request(request)
        provider = self._provider_for_profile(request.profile)
        return provider.run(request, emit=emit)

    def run_planning_chat(
        self,
        *,
        context: HarnessRuntimeContext,
        input_artifacts: dict[str, Any] | None = None,
        request_id: str | None = None,
        profile_id: str = "openhands-planning-chat-default",
        emit: Any | None = None,
    ) -> HarnessRunResult:
        return self.run(
            self._request(
                request_id=request_id,
                contract_id=CONVERSATION_HARNESS_ID,
                mode="planning_chat",
                profile_id=profile_id,
                context=context,
                input_artifacts=input_artifacts,
            ),
            emit=emit,
        )

    def run_workflow_supervisor(
        self,
        *,
        context: HarnessRuntimeContext,
        input_artifacts: dict[str, Any] | None = None,
        request_id: str | None = None,
        profile_id: str = "openhands-workflow-supervisor-default",
        emit: Any | None = None,
    ) -> HarnessRunResult:
        return self.run(
            self._request(
                request_id=request_id,
                contract_id=CONVERSATION_HARNESS_ID,
                mode="workflow_supervisor",
                profile_id=profile_id,
                context=context,
                input_artifacts=input_artifacts,
            ),
            emit=emit,
        )

    def run_task_execution(
        self,
        *,
        context: HarnessRuntimeContext,
        input_artifacts: dict[str, Any] | None = None,
        request_id: str | None = None,
        profile_id: str = "openhands-task-executor-default",
        emit: Any | None = None,
    ) -> HarnessRunResult:
        return self.run(
            self._request(
                request_id=request_id,
                contract_id=TASK_EXECUTION_HARNESS_ID,
                mode="task_execution",
                profile_id=profile_id,
                context=context,
                input_artifacts=input_artifacts,
            ),
            emit=emit,
        )

    def profile_for_id(self, profile_id: str) -> HarnessRuntimeProfile:
        try:
            return self.profiles[profile_id]
        except KeyError as exc:
            raise ValueError(f"unknown harness runtime profile {profile_id!r}") from exc

    def _request(
        self,
        *,
        request_id: str | None,
        contract_id: str,
        mode: HarnessMode,
        profile_id: str,
        context: HarnessRuntimeContext,
        input_artifacts: dict[str, Any] | None,
    ) -> HarnessRunRequest:
        profile = self.profile_for_id(profile_id)
        return HarnessRunRequest(
            request_id=request_id or str(uuid.uuid4()),
            contract_id=contract_id,
            mode=mode,
            profile=profile,
            context=context,
            input_artifacts=input_artifacts or {},
        )

    def _validate_request(self, request: HarnessRunRequest) -> None:
        contract = harness_contract_for_id(request.contract_id)
        if request.profile.harness_id != contract.harness_id:
            raise ValueError(
                f"profile {request.profile.id!r} targets {request.profile.harness_id!r}, "
                f"but request contract is {contract.harness_id!r}"
            )
        if request.mode not in contract.modes:
            raise ValueError(f"mode {request.mode!r} is not valid for harness {contract.harness_id!r}")
        if request.profile.mode != request.mode:
            raise ValueError(f"profile {request.profile.id!r} mode does not match request mode {request.mode!r}")
        enforce_harness_safety(contract, request.profile)
        enforce_sandbox_policy(contract, request.profile)

    def _provider_for_profile(self, profile: HarnessRuntimeProfile) -> HarnessProvider:
        if profile.provider_id == OPENHANDS_PROVIDER_ID and not _openhands_enabled():
            return self._fallback_provider(profile)

        provider = self.providers.get(profile.provider_id)
        if provider is not None and provider.is_available():
            return provider

        return self._fallback_provider(profile)

    def _fallback_provider(self, profile: HarnessRuntimeProfile) -> HarnessProvider:
        fallback = self.providers.get(INTERNAL_FALLBACK_PROVIDER_ID)
        if fallback is not None and fallback.is_available():
            return fallback

        raise RuntimeError(f"no available harness provider for profile {profile.id!r}")


def _openhands_enabled() -> bool:
    return os.getenv("CODER_ENABLE_OPENHANDS_RUNTIME") == "1"


__all__ = ["HarnessRuntimeManager"]
