from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.agent_harness.action_protocol import HarnessActionRequest
from coder_workbench.agent_harness.tool_metadata import ToolMetadataRegistry


class ToolActionBatch(BaseModel):
    model_config = ConfigDict(extra="forbid")

    execution_mode: Literal["concurrent", "exclusive"]
    actions: list[HarnessActionRequest] = Field(default_factory=list)


class ToolBatcher:
    def __init__(self, metadata_registry: ToolMetadataRegistry | None = None) -> None:
        self.metadata_registry = metadata_registry or ToolMetadataRegistry()

    def partition(self, actions: list[HarnessActionRequest]) -> list[ToolActionBatch]:
        batches: list[ToolActionBatch] = []
        pending_concurrent: list[HarnessActionRequest] = []

        def flush_concurrent() -> None:
            if pending_concurrent:
                batches.append(ToolActionBatch(execution_mode="concurrent", actions=list(pending_concurrent)))
                pending_concurrent.clear()

        for action in actions:
            metadata = self.metadata_registry.require(action.action_type)
            if metadata.is_concurrency_safe:
                pending_concurrent.append(action)
                continue
            flush_concurrent()
            batches.append(ToolActionBatch(execution_mode="exclusive", actions=[action]))

        flush_concurrent()
        return batches
