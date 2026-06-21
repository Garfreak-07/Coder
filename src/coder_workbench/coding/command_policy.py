from __future__ import annotations

from dataclasses import asdict
from dataclasses import dataclass
from typing import Literal


CommandRisk = Literal["low", "medium", "high"]


@dataclass(frozen=True)
class CommandPolicyDecision:
    allowed: bool
    requires_approval: bool
    risk: CommandRisk
    reason: str = ""

    def as_dict(self) -> dict[str, object]:
        return asdict(self)


SHELL_META_CHARS = {"&&", "||", "|", ";", ">", "<", "$(", "`"}
HIGH_RISK_TOKENS = {
    "rm ",
    "rm -",
    "del ",
    "rmdir ",
    "format ",
    "sudo ",
    "chmod ",
    "chown ",
    "curl ",
    "wget ",
    "ssh ",
    "scp ",
}


def evaluate_command_policy(
    *,
    command: str,
    argv: list[str] | None,
    shell: bool,
    source: str = "model",
    sandbox: bool = False,
) -> CommandPolicyDecision:
    text = command.strip() if command else " ".join(argv or [])
    if not text:
        return CommandPolicyDecision(allowed=True, requires_approval=False, risk="low")

    lower = text.lower()
    if any(token in lower for token in HIGH_RISK_TOKENS):
        return CommandPolicyDecision(
            allowed=True,
            requires_approval=True,
            risk="high",
            reason="Command contains high-risk token.",
        )

    if shell or any(meta in text for meta in SHELL_META_CHARS):
        return CommandPolicyDecision(
            allowed=True,
            requires_approval=not sandbox,
            risk="medium",
            reason="Shell command requires approval outside sandbox.",
        )

    if source == "model" and not sandbox:
        return CommandPolicyDecision(
            allowed=True,
            requires_approval=True,
            risk="medium",
            reason="Model-generated command requires approval outside sandbox.",
        )

    return CommandPolicyDecision(allowed=True, requires_approval=False, risk="low")
