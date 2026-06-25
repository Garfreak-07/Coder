from __future__ import annotations

import unittest

from coder_workbench.agent_harness import RecoveryPolicy


class CodeWorkerRecoveryPolicyTests(unittest.TestCase):
    def test_invalid_json_recovers_once(self) -> None:
        policy = RecoveryPolicy()

        first = policy.decide("invalid_json", attempts=[])
        second = policy.decide("invalid_json", attempts=[{"error_code": "invalid_json"}])

        self.assertTrue(first.recoverable)
        self.assertFalse(second.recoverable)
        self.assertEqual(first.max_attempts, 1)
        self.assertIn("JSON", first.next_instruction)

    def test_command_failed_recovers_twice(self) -> None:
        policy = RecoveryPolicy()

        first = policy.decide("command_failed", attempts=[])
        second = policy.decide("command_failed", attempts=[{"error_code": "command_failed"}])
        third = policy.decide(
            "command_failed",
            attempts=[{"error_code": "command_failed"}, {"error_code": "command_failed"}],
        )

        self.assertTrue(first.recoverable)
        self.assertTrue(second.recoverable)
        self.assertFalse(third.recoverable)

    def test_permission_boundary_is_not_recoverable(self) -> None:
        decision = RecoveryPolicy().decide("permission_boundary", attempts=[])

        self.assertFalse(decision.recoverable)
        self.assertEqual(decision.max_attempts, 0)


if __name__ == "__main__":
    unittest.main()
