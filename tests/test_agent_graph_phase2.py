from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from pydantic import ValidationError

from coder_workbench.agent_graph.cache import GraphRunCache
from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_graph.schema import ExecutionRecord, PlannerOrder, TestRecord, WorkItem
from coder_workbench.core import AgentWorkflowSpec, default_planner_led_agent_workflow
from coder_workbench.server.storage import RunStore


class AgentGraphSchemaTests(unittest.TestCase):
    def test_work_item_requires_phase2_fields(self) -> None:
        with self.assertRaises(ValidationError):
            WorkItem.model_validate(
                {
                    "work_item_id": "",
                    "merge_index": 1,
                    "assignee_agent_id": "executor",
                    "task_summary": "Do the work.",
                    "depends_on": [],
                    "tester_agent_ids": [],
                }
            )

        item = WorkItem.model_validate(
                {
                    "work_item_id": "executor-work",
                    "order_index": 1,
                    "assignee_agent_id": "executor",
                "task_summary": "Do the work.",
                "depends_on": [],
                "tester_agent_ids": ["tester"],
            }
        )

        self.assertEqual(item.merge_index, 1)
        self.assertEqual(item.order_index, 1)
        self.assertNotIn("order_index", item.model_dump(mode="json"))
        self.assertEqual(item.tester_agent_ids, ["tester"])


class AgentGraphCacheTests(unittest.TestCase):
    def test_graph_run_cache_writes_task_execution_and_test_records_by_work_item(self) -> None:
        planner_order = PlannerOrder.model_validate(
            {
                "round": 1,
                "round_goal": "Implement a focused change.",
                "plan_graph": {
                    "work_items": [
                        {
                            "work_item_id": "backend-work",
                            "merge_index": 1,
                            "assignee_agent_id": "backend",
                            "task_summary": "Backend only.",
                            "depends_on": [],
                            "tester_agent_ids": ["backend-tester"],
                        },
                        {
                            "work_item_id": "frontend-work",
                            "merge_index": 2,
                            "assignee_agent_id": "frontend",
                            "task_summary": "Frontend only.",
                            "depends_on": [],
                            "tester_agent_ids": ["frontend-tester"],
                        },
                    ]
                },
            }
        )
        cache = GraphRunCache(round=1)
        plan_cache = cache.cache_planner_order(planner_order, "planner_order_round_1")

        backend_task = cache.create_agent_task(planner_order.plan_graph.work_items[0], planner_order_ref=plan_cache.planner_order_ref)
        frontend_task = cache.create_agent_task(planner_order.plan_graph.work_items[1], planner_order_ref=plan_cache.planner_order_ref)

        self.assertEqual(backend_task.task_summary, "Backend only.")
        self.assertEqual(frontend_task.task_summary, "Frontend only.")
        self.assertNotEqual(backend_task.assigned_agent_id, frontend_task.assigned_agent_id)

        cache.record_execution(
            ExecutionRecord(
                work_item_id="backend-work",
                merge_index=1,
                agent_id="backend",
                status="completed",
                execution_summary="Backend done.",
                execution_result_ref="execution_result_backend",
            )
        )
        cache.record_test(
            TestRecord(
                work_item_id="backend-work",
                merge_index=1,
                tester_agent_id="backend-tester",
                status="pass",
                test_summary="Backend tests pass.",
                test_result_ref="test_result_backend",
            )
        )

        self.assertEqual(cache.execution_cache["backend-work"].agent_id, "backend")
        self.assertEqual(cache.test_cache["backend-work"][0].tester_agent_id, "backend-tester")
        self.assertEqual(
            cache.refs_for_work_item("backend-work"),
            ["execution_result_backend", "test_result_backend"],
        )


class AgentGraphRunnerPhase2Tests(unittest.TestCase):
    def test_runner_outputs_plan_graph_cache_and_task_envelopes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow()).run("Run Phase 2.", tmp)

        self.assertEqual(result.status, "completed")
        self.assertIn("graph_run_cache", result.data)
        cache = result.data["graph_run_cache"]
        self.assertEqual(cache["planner_order"]["plan_graph"]["work_items"][0]["work_item_id"], "executor-work")
        self.assertEqual(cache["agent_tasks"]["executor-work"]["assigned_agent_id"], "executor")
        self.assertEqual(cache["execution_cache"]["executor-work"]["status"], "completed")
        self.assertEqual(cache["test_cache"]["executor-work"][0]["tester_agent_id"], "tester")
        self.assertIn("agent_task.completed", {event.type for event in result.events})
        self.assertIn("test.local.completed", {event.type for event in result.events})

    def test_runner_records_fetchable_agent_graph_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow()).run("Record artifacts.", tmp)

            self.assertEqual(result.status, "completed")
            self.assertTrue(
                {
                    "planner_order_round_1",
                    "execution_result_executor-work",
                    "test_result_executor-work_tester",
                    "planner_input_bundle_round_1",
                    "round_summary_round_1",
                    "planner_decision_round_1",
                }.issubset(result.artifacts)
            )
            cache = result.data["graph_run_cache"]
            self.assertEqual(cache["plan_cache"]["planner_order_ref"], "planner_order_round_1")
            self.assertEqual(
                cache["execution_cache"]["executor-work"]["execution_result_ref"],
                "execution_result_executor-work",
            )
            self.assertEqual(
                cache["test_cache"]["executor-work"][0]["test_result_ref"],
                "test_result_executor-work_tester",
            )
            produced_ids = {
                event.payload["artifact_id"]
                for event in result.events
                if event.type == "artifact.produced"
            }
            self.assertIn("planner_input_bundle_round_1", produced_ids)

            store = RunStore(Path(tmp) / ".coder")
            stored = store.save("agent-graph", tmp, "Record artifacts.", result)
            loaded = store.get_artifact(stored.id, "planner_input_bundle_round_1")

        self.assertEqual(loaded["artifact_type"], "planner_input_bundle")
        self.assertEqual(loaded["items"][0]["work_item_id"], "executor-work")

    def test_runner_records_final_tester_aggregate(self) -> None:
        planner_order = {
            "artifact_type": "planner_order",
            "round": 1,
            "round_goal": "Aggregate test evidence.",
            "plan_graph": {
                "work_items": [
                    {
                        "work_item_id": "executor-work",
                        "merge_index": 1,
                        "assignee_agent_id": "executor",
                        "task_summary": "Run work.",
                        "depends_on": [],
                        "tester_agent_ids": ["tester", "tester2"],
                    }
                ],
                "final_tester_agent_id": "final_tester",
            },
        }
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(_workflow_with_final_tester()).run(
                "Aggregate test evidence.",
                tmp,
                initial_data={"planner_order": planner_order},
            )

        self.assertEqual(result.status, "completed")
        cache = result.data["graph_run_cache"]
        self.assertEqual(cache["final_test_cache"]["final_tester_agent_id"], "final_tester")
        self.assertEqual(cache["final_test_cache"]["final_test_result_ref"], "test_result_final_final_tester")
        self.assertEqual(result.data["planner_input_bundle"]["final_test_ref"], "test_result_final_final_tester")
        self.assertIn("test_result_final_final_tester", result.artifacts)
        self.assertIn("test.final.completed", {event.type for event in result.events})

def _workflow_with_final_tester() -> AgentWorkflowSpec:
    payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
    payload["agents"].extend(
        [
            {
                "id": "tester2",
                "name": "Second Tester",
                "role": "tester",
                "model_tier": "standard",
                "can_talk_to_human": False,
                "capabilities": ["model_review", "return_test_result"],
            },
            {
                "id": "final_tester",
                "name": "Final Tester",
                "role": "reviewer",
                "model_tier": "standard",
                "can_talk_to_human": False,
                "capabilities": ["aggregate_tests", "return_test_result"],
            },
        ]
    )
    payload["edges"] = [
        {"from": "planner", "to": "executor"},
        {"from": "executor", "to": "tester"},
        {"from": "executor", "to": "tester2"},
        {"from": "tester", "to": "final_tester"},
        {"from": "tester2", "to": "final_tester"},
        {"from": "final_tester", "to": "planner", "loop": True},
    ]
    return AgentWorkflowSpec.model_validate(payload)


if __name__ == "__main__":
    unittest.main()
