from __future__ import annotations

from typing import Any

from pydantic import BaseModel, ConfigDict


class RecoveryDecision(BaseModel):
    model_config = ConfigDict(extra="forbid")

    recoverable: bool
    reason: str
    next_instruction: str
    max_attempts: int
    error_code: str


class RecoveryPolicy:
    def decide(self, error_code: str, *, attempts: list[dict[str, Any]] | None = None) -> RecoveryDecision:
        attempts = attempts or []
        policy = _POLICIES.get(error_code) or _POLICIES["unknown_error"]
        used = sum(1 for attempt in attempts if attempt.get("error_code") == error_code)
        recoverable = bool(policy["recoverable"]) and used < int(policy["max_attempts"])
        return RecoveryDecision(
            recoverable=recoverable,
            reason=str(policy["reason"]),
            next_instruction=str(policy["next_instruction"]),
            max_attempts=int(policy["max_attempts"]),
            error_code=error_code,
        )


_POLICIES: dict[str, dict[str, Any]] = {
    "invalid_json": {
        "recoverable": True,
        "max_attempts": 1,
        "reason": "Model output was not valid JSON.",
        "next_instruction": "Return exactly one valid JSON object.",
    },
    "invalid_action_schema": {
        "recoverable": True,
        "max_attempts": 1,
        "reason": "Model output did not match harness_action schema.",
        "next_instruction": "Return a valid harness_action JSON object.",
    },
    "invalid_artifact_type": {
        "recoverable": True,
        "max_attempts": 1,
        "reason": "Model returned an unsupported artifact type.",
        "next_instruction": "Return harness_action or execution_result only.",
    },
    "unknown_action": {
        "recoverable": True,
        "max_attempts": 1,
        "reason": "Model requested an unknown action.",
        "next_instruction": "Choose one allowed CodeWorker action.",
    },
    "permission_boundary": {
        "recoverable": False,
        "max_attempts": 0,
        "reason": "Model attempted an action outside executor authority.",
        "next_instruction": "Return a blocked execution_result.",
    },
    "patch_failed": {
        "recoverable": True,
        "max_attempts": 2,
        "reason": "Patch action failed.",
        "next_instruction": "Reread the affected file and produce a smaller patch or return blocked.",
    },
    "command_failed": {
        "recoverable": True,
        "max_attempts": 2,
        "reason": "Command action failed.",
        "next_instruction": "Inspect the command output, repair, rerun checks, or return blocked.",
    },
    "stop_gate_failed": {
        "recoverable": True,
        "max_attempts": 2,
        "reason": "Candidate execution_result failed stop gate checks.",
        "next_instruction": "Obtain runtime-backed evidence, correct the final artifact, or return blocked.",
    },
    "context_too_large": {
        "recoverable": True,
        "max_attempts": 1,
        "reason": "Context exceeded the budget.",
        "next_instruction": "Compact observations and retry.",
    },
    "unknown_error": {
        "recoverable": False,
        "max_attempts": 0,
        "reason": "Recovery policy has no safe retry path.",
        "next_instruction": "Return blocked.",
    },
}
