from __future__ import annotations

from collections.abc import Iterable
from typing import Literal

from pydantic import BaseModel, ConfigDict


class ToolCapabilityMetadata(BaseModel):
    model_config = ConfigDict(extra="forbid")

    name: str
    is_read_only: bool
    is_concurrency_safe: bool
    is_destructive: bool = False
    interrupt_behavior: Literal["cancel", "block"] = "block"
    max_result_size_chars: int = 12000
    requires_scope: bool = True
    risk_level: Literal["low", "medium", "high"] = "low"
    phase: Literal["inspect", "edit", "verify", "finalize", "utility"]


DEFAULT_CODE_WORKER_TOOL_METADATA: dict[str, ToolCapabilityMetadata] = {
    "read_file": ToolCapabilityMetadata(
        name="read_file",
        is_read_only=True,
        is_concurrency_safe=True,
        interrupt_behavior="cancel",
        phase="inspect",
    ),
    "search_files": ToolCapabilityMetadata(
        name="search_files",
        is_read_only=True,
        is_concurrency_safe=True,
        interrupt_behavior="cancel",
        phase="inspect",
    ),
    "inspect_git_diff": ToolCapabilityMetadata(
        name="inspect_git_diff",
        is_read_only=True,
        is_concurrency_safe=True,
        interrupt_behavior="cancel",
        phase="inspect",
    ),
    "propose_patch": ToolCapabilityMetadata(
        name="propose_patch",
        is_read_only=False,
        is_concurrency_safe=False,
        interrupt_behavior="block",
        phase="edit",
    ),
    "apply_patch_sandbox": ToolCapabilityMetadata(
        name="apply_patch_sandbox",
        is_read_only=False,
        is_concurrency_safe=False,
        interrupt_behavior="block",
        phase="edit",
    ),
    "run_command_sandbox": ToolCapabilityMetadata(
        name="run_command_sandbox",
        is_read_only=False,
        is_concurrency_safe=False,
        interrupt_behavior="cancel",
        phase="verify",
        risk_level="medium",
    ),
    "read_tool_output": ToolCapabilityMetadata(
        name="read_tool_output",
        is_read_only=True,
        is_concurrency_safe=True,
        interrupt_behavior="cancel",
        phase="utility",
    ),
    "return_execution_result": ToolCapabilityMetadata(
        name="return_execution_result",
        is_read_only=False,
        is_concurrency_safe=False,
        interrupt_behavior="block",
        phase="finalize",
    ),
}


class ToolMetadataRegistry:
    def __init__(self, metadata: Iterable[ToolCapabilityMetadata] | None = None) -> None:
        records = list(metadata) if metadata is not None else list(DEFAULT_CODE_WORKER_TOOL_METADATA.values())
        self._metadata = {record.name: record for record in records}

    def get(self, name: str) -> ToolCapabilityMetadata | None:
        return self._metadata.get(name)

    def require(self, name: str) -> ToolCapabilityMetadata:
        metadata = self.get(name)
        if metadata is None:
            raise KeyError(f"Unknown CodeWorker tool metadata: {name}")
        return metadata

    def names(self) -> set[str]:
        return set(self._metadata)

    def all(self) -> list[ToolCapabilityMetadata]:
        return list(self._metadata.values())
