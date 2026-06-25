from __future__ import annotations

import tempfile
import unittest

from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.runtime_state import (
    SharedRunState,
    StateUpdate,
    apply_state_update,
    build_debug_state_view,
    build_executor_state_view,
    build_final_report_state_view,
    build_planner_state_view,
)


class RuntimeStateTests(unittest.TestCase):
    def test_reducers_apply_control_work_item_message_and_artifact_refs(self) -> None:
        state = SharedRunState(run_id="run", workflow_id="workflow", user_request="Do work.")
        state = apply_state_update(
            state,
            StateUpdate(
                update_id="u1",
                run_id="run",
                source="test",
                channel="control",
                payload={"status": "running", "round": 1, "blocked_recovery_used": True},
            ),
        )
        state = apply_state_update(
            state,
            StateUpdate(
                update_id="u2",
                run_id="run",
                source="test",
                channel="work_items",
                payload={
                    "work_item_id": "work",
                    "agent_id": "executor",
                    "status": "blocked",
                    "summary": "Blocked.",
                    "execution_result_ref": "execution_result_work",
                    "blocked_reason": "Missing dependency.",
                },
            ),
        )
        state = apply_state_update(
            state,
            StateUpdate(
                update_id="u3",
                run_id="run",
                source="test",
                channel="messages",
                payload={
                    "message_id": "m1",
                    "source_agent_id": "executor",
                    "target": "planner",
                    "kind": "blocked_fact",
                    "summary": "work blocked",
                    "artifact_refs": ["execution_result_work"],
                },
            ),
        )
        state = apply_state_update(
            state,
            StateUpdate(
                update_id="u4",
                run_id="run",
                source="test",
                channel="artifacts",
                payload={
                    "artifact_id": "execution_result_work",
                    "artifact_type": "execution_result",
                    "summary": "Blocked.",
                },
            ),
        )

        self.assertEqual(state.control.round, 1)
        self.assertTrue(state.control.blocked_recovery_used)
        self.assertEqual(state.work_items["work"].status, "blocked")
        self.assertEqual(state.messages[0].kind, "blocked_fact")
        self.assertEqual(state.artifacts["execution_result_work"].artifact_type, "execution_result")

    def test_state_update_rejects_large_inline_payload_keys(self) -> None:
        with self.assertRaises(Exception):
            StateUpdate(
                update_id="u1",
                run_id="run",
                source="test",
                channel="artifacts",
                payload={"artifact_id": "a", "artifact_type": "x", "graph_run_cache": {}},
            )

    def test_state_views_hide_raw_debug_payloads(self) -> None:
        state = SharedRunState(run_id="run", workflow_id="workflow", user_request="Do work.")
        state = apply_state_update(
            state,
            StateUpdate(
                update_id="u1",
                run_id="run",
                source="test",
                channel="planner",
                payload={"planner_order_ref": "planner_order_1", "planner_decision_ref": "planner_decision_1"},
            ),
        )
        state = apply_state_update(
            state,
            StateUpdate(
                update_id="u2",
                run_id="run",
                source="test",
                channel="work_items",
                payload={
                    "work_item_id": "work",
                    "agent_id": "executor",
                    "status": "completed",
                    "summary": "Done.",
                    "execution_result_ref": "execution_result_work",
                },
            ),
        )

        planner_view = build_planner_state_view(state)
        executor_view = build_executor_state_view(state, "work")
        final_report_view = build_final_report_state_view(state)
        debug_view = build_debug_state_view(state)

        self.assertEqual(planner_view["planner"]["planner_order_ref"], "planner_order_1")
        self.assertEqual(executor_view["assigned_work_item"]["work_item_id"], "work")
        self.assertEqual(final_report_view["planner_decision_ref"], "planner_decision_1")
        self.assertIn("refs", debug_view)
        self.assertNotIn("graph_run_cache", str(planner_view))

    def test_runner_records_compact_shared_run_state(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow()).run(
                "Record shared state.",
                tmp,
            )

        state = result.data["shared_run_state"]

        self.assertEqual(state["control"]["status"], "completed")
        self.assertEqual(state["planner"]["planner_order_ref"], "planner_order_round_1")
        self.assertEqual(state["planner"]["planner_decision_ref"], "planner_decision_round_1")
        self.assertEqual(state["planner"]["round_summary_ref"], "round_summary_round_1")
        self.assertEqual(state["final_report_ref"], "final_report")
        self.assertEqual(state["work_items"]["executor-work"]["status"], "completed")
        self.assertIn("execution_result_executor-work", state["artifacts"])
        self.assertNotIn("graph_run_cache", str(state))
        self.assertNotIn("token_ledger", str(state))

    def test_blocked_resume_checkpoint_contains_shared_run_state(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow()).run(
                "Force a controller checkpoint.",
                tmp,
                initial_data={
                    "max_auto_rounds": 1,
                    "planner_decision": {
                        "artifact_type": "planner_decision",
                        "round": 1,
                        "task_done": False,
                        "next_action": "continue",
                        "reason": "Need another round.",
                        "next_round_goal": "Continue.",
                    },
                },
            )

        self.assertEqual(result.status, "blocked")
        self.assertIsNotNone(result.resume_checkpoint)
        checkpoint_data = result.resume_checkpoint["data"]  # type: ignore[index]
        self.assertIn("shared_run_state", checkpoint_data)
        self.assertEqual(checkpoint_data["shared_run_state"]["final_report_ref"], "final_report")
        self.assertEqual(
            checkpoint_data["shared_run_state"]["work_items"]["executor-work"]["execution_result_ref"],
            "execution_result_executor-work",
        )


if __name__ == "__main__":
    unittest.main()
