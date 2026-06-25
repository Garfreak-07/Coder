from __future__ import annotations

from typing import Any

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.agent_harness.action_protocol import HarnessObservation


class HarnessSession(BaseModel):
    model_config = ConfigDict(extra="forbid")

    run_id: str
    round: int
    work_item_id: str
    agent_id: str
    merge_index: int
    task_summary: str
    capability_set: dict[str, Any] = Field(default_factory=dict)
    coding_context_packet: dict[str, Any] = Field(default_factory=dict)

    observations: list[HarnessObservation] = Field(default_factory=list)
    evidence_refs: list[str] = Field(default_factory=list)

    opened_files: list[str] = Field(default_factory=list)
    searched_patterns: list[str] = Field(default_factory=list)

    changed_files: list[str] = Field(default_factory=list)
    created_files: list[str] = Field(default_factory=list)
    deleted_files: list[str] = Field(default_factory=list)
    patch_refs: list[str] = Field(default_factory=list)

    command_checks: list[dict[str, Any]] = Field(default_factory=list)
    blocked_reasons: list[str] = Field(default_factory=list)
    stop_gate_failures: list[dict[str, Any]] = Field(default_factory=list)
    recovery_attempts: list[dict[str, Any]] = Field(default_factory=list)


class CodeWorkerLoopState(BaseModel):
    model_config = ConfigDict(extra="forbid")

    session: HarnessSession
    messages: list[dict[str, Any]] = Field(default_factory=list)
    turn_count: int = 0
    max_turns: int = 16

    transition: dict[str, Any] | None = None

    max_output_recovery_count: int = 0
    has_attempted_reactive_compact: bool = False
    stop_gate_active: bool = False

    pending_tool_summary_ref: str | None = None
    last_model_output: str | None = None
