from __future__ import annotations

import tempfile
import unittest
from typing import Any

from coder_workbench.core import WorkflowSpec
from coder_workbench.runtime import run_workflow
from coder_workbench.runtime.runner import WorkflowRunner


class CapturingExecutor:
    def __init__(self) -> None:
        self.contexts: list[dict[str, Any]] = []

    def run(self, agent, context: dict[str, Any]) -> dict[str, Any]:
        self.contexts.append(context)
        return {
            "status": "completed",
            "state_keys": sorted(context.get("state", {}).keys()),
            "summary_keys": sorted(context.get("state_summaries", {}).keys()),
        }


class LoopAndContextPacketTests(unittest.TestCase):
    def test_loop_node_stops_at_max_iterations(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workflow = WorkflowSpec.model_validate(
                {
                    "id": "loop-test",
                    "name": "Loop test",
                    "max_steps": 10,
                    "nodes": [
                        {"id": "start", "type": "start"},
                        {
                            "id": "retry",
                            "type": "loop",
                            "loop_mode": "retry_until",
                            "condition": "review.status == 'done'",
                            "max_iterations": 2,
                            "output_key": "retry_state",
                        },
                        {"id": "end", "type": "end"},
                    ],
                    "edges": [
                        {"from": "start", "to": "retry"},
                        {"from": "retry", "to": "retry", "when": "retry_state.should_continue == True", "max_traversals": 3},
                        {"from": "retry", "to": "end", "when": "retry_state.should_continue == False"},
                    ],
                }
            )

            result = run_workflow(
                workflow,
                "retry until done",
                tmp,
                initial_data={"review": {"status": "needs_changes"}},
            )

            self.assertEqual(result.status, "completed")
            self.assertEqual(result.data["retry_state"]["iteration"], 2)
            self.assertFalse(result.data["retry_state"]["should_continue"])
            self.assertEqual(result.data["retry_state"]["break_reason"], "max_iterations")
            self.assertTrue(any(event.type == "loop.iteration.started" for event in result.events))
            self.assertTrue(any(event.type == "loop.completed" for event in result.events))

    def test_agent_context_packet_includes_loop_state(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workflow = WorkflowSpec.model_validate(
                {
                    "id": "context-packet-test",
                    "name": "Context packet test",
                    "nodes": [
                        {"id": "start", "type": "start"},
                        {
                            "id": "retry",
                            "type": "loop",
                            "loop_mode": "retry_until",
                            "condition": "review.status == 'done'",
                            "max_iterations": 2,
                            "output_key": "retry_state",
                        },
                        {"id": "agent", "type": "agent", "agent_id": "worker", "output_key": "worker_result"},
                        {"id": "end", "type": "end"},
                    ],
                    "edges": [
                        {"from": "start", "to": "retry"},
                        {"from": "retry", "to": "agent", "when": "retry_state.should_continue == True"},
                        {"from": "agent", "to": "end"},
                    ],
                    "agents": [
                        {
                            "id": "worker",
                            "role": "Worker",
                            "goal": "Use compact context.",
                            "context": {"input_keys": ["retry_state"], "summary_keys": []},
                        }
                    ],
                }
            )

            result = run_workflow(
                workflow,
                "inspect context",
                tmp,
                initial_data={"review": {"status": "needs_changes"}, "scopes": ["src"]},
            )

            self.assertEqual(result.status, "completed")
            event_types = [event.type for event in result.events]
            packet_index = event_types.index("agent.context_packet")
            called_index = event_types.index("agent.called")
            self.assertLess(packet_index, called_index)
            packet = result.events[packet_index].payload["packet"]
            self.assertEqual(packet["agent"]["id"], "worker")
            self.assertEqual(packet["loop"]["node_id"], "retry")
            self.assertEqual(packet["loop"]["iteration"], 1)
            self.assertEqual(packet["project_context"]["scopes"], ["src"])
            self.assertIn("retry_state", packet["selected_state_keys"])

    def test_empty_input_keys_do_not_send_all_state_by_default(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workflow = WorkflowSpec.model_validate(
                {
                    "id": "default-context-test",
                    "name": "Default context test",
                    "nodes": [
                        {"id": "start", "type": "start"},
                        {"id": "agent", "type": "agent", "agent_id": "worker", "output_key": "worker_result"},
                        {"id": "end", "type": "end"},
                    ],
                    "edges": [
                        {"from": "start", "to": "agent"},
                        {"from": "agent", "to": "end"},
                    ],
                    "agents": [{"id": "worker", "role": "Worker", "goal": "Use compact context."}],
                }
            )
            executor = CapturingExecutor()

            result = WorkflowRunner(workflow, agent_executor=executor).run(
                "inspect context",
                tmp,
                initial_data={"large_previous_output": "x" * 2000, "scopes": ["src"]},
            )

            self.assertEqual(result.status, "completed")
            self.assertEqual(executor.contexts[0]["state"], {})
            self.assertEqual(executor.contexts[0]["state_summaries"], {})
            self.assertEqual(result.data["worker_result"]["state_keys"], [])
            packet = next(event.payload["packet"] for event in result.events if event.type == "agent.context_packet")
            self.assertEqual(packet["selected_state_keys"], [])

    def test_include_all_state_is_explicit_and_lists_are_compacted_recursively(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workflow = WorkflowSpec.model_validate(
                {
                    "id": "explicit-context-test",
                    "name": "Explicit context test",
                    "nodes": [
                        {"id": "start", "type": "start"},
                        {"id": "agent", "type": "agent", "agent_id": "worker", "output_key": "worker_result"},
                        {"id": "end", "type": "end"},
                    ],
                    "edges": [
                        {"from": "start", "to": "agent"},
                        {"from": "agent", "to": "end"},
                    ],
                    "agents": [
                        {
                            "id": "worker",
                            "role": "Worker",
                            "goal": "Use explicit full state.",
                            "context": {
                                "include_all_state": True,
                                "max_items_per_key": 1,
                                "max_chars_per_value": 500,
                            },
                        }
                    ],
                }
            )
            executor = CapturingExecutor()

            result = WorkflowRunner(workflow, agent_executor=executor).run(
                "inspect context",
                tmp,
                initial_data={"items": [["x" * 700, "y" * 700], ["z" * 700]]},
            )

            self.assertEqual(result.status, "completed")
            self.assertEqual(result.data["worker_result"]["state_keys"], ["items"])
            compacted = executor.contexts[0]["state"]["items"]
            self.assertEqual(len(compacted), 1)
            self.assertEqual(len(compacted[0]), 1)
            self.assertEqual(len(compacted[0][0]), 500)

    def test_token_budget_overflow_compacts_context_before_agent_call(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workflow = WorkflowSpec.model_validate(
                {
                    "id": "budget-test",
                    "name": "Budget test",
                    "token_budget": 1000,
                    "nodes": [
                        {"id": "start", "type": "start"},
                        {"id": "agent", "type": "agent", "agent_id": "worker", "output_key": "worker_result"},
                        {"id": "end", "type": "end"},
                    ],
                    "edges": [
                        {"from": "start", "to": "agent"},
                        {"from": "agent", "to": "end"},
                    ],
                    "agents": [
                        {
                            "id": "worker",
                            "role": "Worker",
                            "goal": "Use too much context.",
                            "context": {"include_all_state": True, "max_chars_per_value": 50000},
                        }
                    ],
                }
            )
            executor = CapturingExecutor()

            result = WorkflowRunner(workflow, agent_executor=executor).run(
                "inspect context",
                tmp,
                initial_data={"large": "x" * 8000},
            )

            self.assertEqual(result.status, "completed")
            self.assertEqual(result.agent_calls, 1)
            self.assertEqual(len(executor.contexts), 1)
            self.assertIsInstance(executor.contexts[0]["state"]["large"], dict)
            budget_events = [event for event in result.events if event.type == "budget.warning"]
            self.assertTrue(budget_events)
            self.assertEqual(budget_events[0].payload["context_reductions"][0]["action"], "summarize_state")
            packet = next(event.payload["packet"] for event in result.events if event.type == "agent.context_packet")
            self.assertLess(packet["token_estimate"]["packet"], packet["token_estimate"]["original_packet"])
            self.assertTrue(packet["token_estimate"]["context_reductions"])
            self.assertTrue(any(event.type == "agent.called" for event in result.events))

    def test_token_budget_excess_blocks_after_compaction_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workflow = WorkflowSpec.model_validate(
                {
                    "id": "budget-block-test",
                    "name": "Budget block test",
                    "token_budget": 1000,
                    "nodes": [
                        {"id": "start", "type": "start"},
                        {"id": "agent", "type": "agent", "agent_id": "worker", "output_key": "worker_result"},
                        {"id": "end", "type": "end"},
                    ],
                    "edges": [
                        {"from": "start", "to": "agent"},
                        {"from": "agent", "to": "end"},
                    ],
                    "agents": [{"id": "worker", "role": "Worker", "goal": "Use compact context."}],
                }
            )
            executor = CapturingExecutor()

            result = WorkflowRunner(workflow, agent_executor=executor).run(
                "x" * 8000,
                tmp,
                initial_data={},
            )

            self.assertEqual(result.status, "blocked")
            self.assertEqual(result.blocked_node_id, "agent")
            self.assertEqual(result.agent_calls, 0)
            self.assertEqual(executor.contexts, [])
            self.assertTrue(any(event.type == "budget.warning" for event in result.events))
            self.assertFalse(any(event.type == "agent.called" for event in result.events))

    def test_node_completed_events_keep_summary_not_full_result(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workflow = WorkflowSpec.model_validate(
                {
                    "id": "event-summary-test",
                    "name": "Event summary test",
                    "nodes": [
                        {"id": "start", "type": "start"},
                        {"id": "agent", "type": "agent", "agent_id": "worker", "output_key": "worker_result"},
                        {"id": "end", "type": "end"},
                    ],
                    "edges": [
                        {"from": "start", "to": "agent"},
                        {"from": "agent", "to": "end"},
                    ],
                    "agents": [
                        {
                            "id": "worker",
                            "role": "Worker",
                            "goal": "Return a result.",
                            "context": {"input_keys": ["request_context"]},
                        }
                    ],
                }
            )

            result = WorkflowRunner(workflow, agent_executor=CapturingExecutor()).run(
                "inspect context",
                tmp,
                initial_data={"request_context": {"note": "small"}},
            )

            completed_events = [event for event in result.events if event.type == "node.completed"]
            self.assertTrue(completed_events)
            for event in completed_events:
                self.assertNotIn("result", event.payload)
                self.assertIn("result_summary", event.payload)


if __name__ == "__main__":
    unittest.main()
