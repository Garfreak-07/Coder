from __future__ import annotations

import unittest

from coder_workbench.budget import BudgetBroker, BudgetLimit


class BudgetBrokerTests(unittest.TestCase):
    def test_reserve_within_budget_succeeds(self) -> None:
        broker = BudgetBroker(BudgetLimit(max_estimated_tokens=100, max_model_calls=1))

        reservation = broker.reserve_model_call(run_id="run", agent_id="planner", estimated_tokens=50)

        self.assertTrue(reservation.approved)
        self.assertEqual(broker.usage("run").estimated_tokens_reserved, 50)
        self.assertEqual(broker.usage("run").model_calls_reserved, 1)

    def test_reserve_over_total_budget_is_denied(self) -> None:
        broker = BudgetBroker(BudgetLimit(max_estimated_tokens=10))

        reservation = broker.reserve_context(run_id="run", estimated_tokens=20)

        self.assertFalse(reservation.approved)
        self.assertEqual(reservation.reason, "estimated_token_budget_exceeded")

    def test_reserve_over_context_call_budget_is_denied(self) -> None:
        broker = BudgetBroker(BudgetLimit(max_estimated_tokens=100, max_context_tokens_per_call=10))

        reservation = broker.reserve_context(run_id="run", estimated_tokens=20)

        self.assertFalse(reservation.approved)
        self.assertEqual(reservation.reason, "context_budget_exceeded")

    def test_commit_updates_actual_usage(self) -> None:
        broker = BudgetBroker(BudgetLimit(max_estimated_tokens=100, max_tool_calls=2))
        reservation = broker.reserve_tool_call(run_id="run", action_type="run_command", estimated_tokens=5)

        committed = broker.commit(reservation.reservation_id, actual_tokens=3)

        self.assertTrue(committed.committed)
        self.assertEqual(broker.usage("run").actual_tokens_committed, 3)
        self.assertEqual(broker.usage("run").tool_calls_committed, 1)


if __name__ == "__main__":
    unittest.main()
