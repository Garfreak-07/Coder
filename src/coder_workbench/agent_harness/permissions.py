from __future__ import annotations

from .actions import HarnessAction
from .policies import HarnessPolicy


class HarnessPermissionPolicy:
    def allow(self, action: HarnessAction, policy: HarnessPolicy) -> bool:
        action_type = action.type
        if action_type in {"finish", "context_request", "interrupt_planner", "observe"}:
            return True
        if action_type == "ask_human":
            return policy.can_ask_human
        if action_type in {"propose_changes", "modify_files", "apply_patch"}:
            return policy.can_modify_files
        if action_type in {"run_command", "run_check"}:
            return policy.can_run_commands
        if action_type == "write_memory":
            return policy.can_write_memory
        return True
