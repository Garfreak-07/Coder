from __future__ import annotations

import tempfile
import unittest

from coder_workbench.agent_graph.schema import (
    PlannerInputBundle,
    PlannerInputBundleItem,
    PlanRunSummary,
    RoundSummaryItem,
)
from coder_workbench.memory import MemoryDelta, MemoryService


class MemoryServiceTests(unittest.TestCase):
    def test_executor_long_term_memory_write_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = MemoryService(tmp)
            staged = service.stage_delta(
                MemoryDelta(
                    workflow_id="workflow",
                    collection="planner_notes",
                    actor_id="executor",
                    actor_role="executor",
                    evidence_refs=["execution_result_work"],
                    entry={"note": "executor learned something"},
                )
            )

            self.assertEqual(staged.status, "rejected")
            self.assertEqual(staged.reason, "executor_cannot_write_long_term_memory")
            self.assertEqual(service.load_workflow_memory("workflow").planner_notes, [])

    def test_memory_delta_requires_evidence_refs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = MemoryService(tmp)
            staged = service.stage_delta(
                MemoryDelta(
                    workflow_id="workflow",
                    collection="planner_notes",
                    actor_id="planner",
                    actor_role="planner",
                    evidence_refs=[],
                    entry={"note": "unsupported memory"},
                )
            )

            self.assertEqual(staged.status, "rejected")
            self.assertEqual(staged.reason, "memory_delta_requires_evidence_refs")

    def test_planner_write_is_staged_until_committed(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = MemoryService(tmp)
            staged = service.stage_delta(
                MemoryDelta(
                    workflow_id="workflow",
                    collection="planner_notes",
                    actor_id="planner",
                    actor_role="planner",
                    evidence_refs=["round_summary_1"],
                    entry={"round": 1, "reason": "remember this"},
                )
            )

            self.assertEqual(staged.status, "staged")
            self.assertEqual(service.load_workflow_memory("workflow").planner_notes, [])

            committed = service.commit_staged(staged.write_id, approved_by="planner")
            memory = service.load_workflow_memory("workflow")

            self.assertEqual(committed.status, "committed")
            self.assertEqual(memory.planner_notes[0]["reason"], "remember this")
            self.assertEqual(memory.planner_notes[0]["evidence_refs"], ["round_summary_1"])

    def test_non_workflow_scope_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = MemoryService(tmp)
            staged = service.stage_delta(
                MemoryDelta(
                    scope="project",
                    workflow_id="workflow",
                    collection="planner_notes",
                    actor_id="planner",
                    actor_role="planner",
                    evidence_refs=["round_summary_1"],
                    entry={"note": "project memory is out of scope"},
                )
            )

            self.assertEqual(staged.status, "rejected")
            self.assertEqual(staged.reason, "memory_scope_not_supported")

    def test_staged_delta_cannot_commit_without_approval(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = MemoryService(tmp)
            staged = service.stage_delta(
                MemoryDelta(
                    workflow_id="workflow",
                    collection="planner_notes",
                    actor_id="planner",
                    actor_role="planner",
                    evidence_refs=["round_summary_1"],
                    entry={"note": "needs approval"},
                )
            )

            committed = service.commit_staged(staged.write_id, approved_by="")

            self.assertEqual(committed.status, "rejected")
            self.assertEqual(committed.reason, "memory_write_requires_approval")
            self.assertEqual(service.load_workflow_memory("workflow").planner_notes, [])

    def test_record_planner_round_wraps_existing_workflow_memory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = MemoryService(tmp)
            bundle = PlannerInputBundle(
                round=1,
                planner_order_ref="planner_order_round_1",
                plan_status="completed",
                items=[
                    PlannerInputBundleItem(
                        merge_index=1,
                        work_item_id="work",
                        task_summary="Do work.",
                        execution_status="completed",
                        execution_summary="Done.",
                        verification_status="pass",
                        verification_summary="Passed.",
                        refs=["execution_result_work"],
                    )
                ],
            )
            summary = PlanRunSummary(
                round=1,
                planner_order_ref="planner_order_round_1",
                plan_status="completed",
                completed_count=1,
                ordered_state=[
                    RoundSummaryItem(
                        merge_index=1,
                        work_item_id="work",
                        status="completed",
                        summary="Done.",
                        refs=["execution_result_work"],
                    )
                ],
            )

            memory = service.record_planner_round(
                workflow_id="workflow",
                bundle=bundle,
                round_summary=summary,
                planner_decision={"next_action": "finish", "reason": "Complete."},
            )

            self.assertEqual(len(memory.successful_assignments), 1)
            self.assertEqual(len(memory.planner_notes), 1)
            self.assertEqual(memory.successful_assignments[0]["evidence_refs"], ["execution_result_work"])


if __name__ == "__main__":
    unittest.main()
