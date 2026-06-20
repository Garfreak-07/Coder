from __future__ import annotations

import tempfile
import unittest

from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.agent_graph.scheduler import AgentGraphScheduler
from coder_workbench.agent_graph.schema import WorkItem
from coder_workbench.core import default_planner_led_agent_workflow


def item(
    work_item_id: str,
    order_index: int,
    depends_on: list[str] | None = None,
) -> WorkItem:
    return WorkItem(
        work_item_id=work_item_id,
        order_index=order_index,
        assignee_agent_id=f"{work_item_id}-agent",
        task_summary=f"Task {work_item_id}",
        depends_on=depends_on or [],
        tester_agent_ids=[],
    )


class AgentGraphSchedulerTests(unittest.TestCase):
    def test_depends_on_waits_for_all_upstreams(self) -> None:
        scheduler = AgentGraphScheduler(
            [
                item("a", 1),
                item("b", 2, ["a", "c"]),
                item("c", 3),
            ],
            max_concurrency=3,
        )

        self.assertEqual([ready.work_item_id for ready in scheduler.ready_items()], ["a", "c"])
        scheduler.mark_completed("a")
        self.assertEqual([ready.work_item_id for ready in scheduler.ready_items()], ["c"])
        scheduler.mark_completed("c")
        self.assertEqual([ready.work_item_id for ready in scheduler.ready_items()], ["b"])

    def test_ready_queue_respects_max_concurrency_and_order_index(self) -> None:
        scheduler = AgentGraphScheduler(
            [
                item("third", 3),
                item("first", 1),
                item("second", 2),
            ],
            max_concurrency=2,
        )

        self.assertEqual([ready.work_item_id for ready in scheduler.ready_items()], ["first", "second"])

    def test_failed_upstream_blocks_downstream(self) -> None:
        scheduler = AgentGraphScheduler([item("a", 1), item("b", 2, ["a"])])

        scheduler.mark_failed("a")
        blocked = scheduler.block_items_with_failed_upstreams()

        self.assertEqual([record.work_item.work_item_id for record in blocked], ["b"])
        self.assertEqual(blocked[0].blocked_by, ["a"])
        self.assertEqual(scheduler.status_by_id["b"], "blocked")


class AgentGraphRunnerSchedulerTests(unittest.TestCase):
    def test_runner_waits_for_dependency_and_passes_upstream_refs(self) -> None:
        planner_order = {
            "artifact_type": "planner_order",
            "round": 1,
            "round_goal": "Run ordered work.",
            "plan_graph": {
                "work_items": [
                    {
                        "work_item_id": "first",
                        "order_index": 1,
                        "assignee_agent_id": "executor",
                        "task_summary": "Run first.",
                        "depends_on": [],
                        "tester_agent_ids": ["tester"],
                    },
                    {
                        "work_item_id": "second",
                        "order_index": 2,
                        "assignee_agent_id": "executor",
                        "task_summary": "Run second.",
                        "depends_on": ["first"],
                        "tester_agent_ids": ["tester"],
                    },
                    {
                        "work_item_id": "third",
                        "order_index": 3,
                        "assignee_agent_id": "executor",
                        "task_summary": "Run third.",
                        "depends_on": [],
                        "tester_agent_ids": [],
                    },
                ]
            },
        }
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow()).run(
                "Run ordered work.",
                tmp,
                initial_data={"planner_order": planner_order, "max_concurrency": 2},
            )

        self.assertEqual(result.status, "completed")
        self.assertEqual(
            result.data["scheduler_status"],
            {"first": "completed", "second": "completed", "third": "completed"},
        )
        events = [event.type for event in result.events]
        self.assertIn("join.completed", events)
        first_completed = next(
            index
            for index, event in enumerate(result.events)
            if event.type == "agent_task.completed" and event.payload["work_item_id"] == "first"
        )
        second_started = next(
            index
            for index, event in enumerate(result.events)
            if event.type == "agent_task.started" and event.payload["work_item_id"] == "second"
        )
        self.assertGreater(second_started, first_completed)
        second_task = result.data["graph_run_cache"]["agent_tasks"]["second"]
        self.assertEqual(
            second_task["upstream_refs"],
            ["memory:execution_result:first", "memory:test_result:first:tester"],
        )


if __name__ == "__main__":
    unittest.main()
