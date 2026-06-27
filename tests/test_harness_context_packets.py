from __future__ import annotations

import unittest

from coder_workbench.agent_graph.agent_run import AgentRun
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, WorkItem
from coder_workbench.context import build_harness_context_packet
from coder_workbench.core import default_planner_led_agent_workflow


class HarnessContextPacketTests(unittest.TestCase):
    def test_planning_chat_packet_uses_hot_warm_cold_shape(self) -> None:
        packet = build_harness_context_packet(
            mode="planning_chat",
            user_goal="Build feature.",
            workflow_id="workflow",
            agent_id="planner",
            planner_agent_id="planner",
            workflow_summary={"workflow_id": "workflow", "workflow_name": "Default"},
            user_constraints=["Do not execute."],
            selected_knowledge_pack_ids=["kb"],
            selected_skill_pack_ids=["skill"],
            selected_memory_pack_ids=["memory"],
            selected_skill_pack_summaries=[{"skill_id": "skill", "summary": "Use narrowly."}],
            knowledge_refs=["knowledge-ref"],
            memory_refs=["memory-ref"],
            repo_intelligence_refs=["repo-ref"],
        )

        self.assertEqual(packet["schema_version"], "harness-context-packet/v1")
        self.assertEqual(packet["hot"]["user_goal"], "Build feature.")
        self.assertEqual(packet["hot"]["selected_workflow"]["workflow_name"], "Default")
        self.assertEqual(packet["hot"]["planner_agent_id"], "planner")
        self.assertEqual(packet["hot"]["user_constraints"], ["Do not execute."])
        self.assertEqual(packet["hot"]["selected_skill_pack_ids"], ["skill"])
        self.assertEqual(packet["warm"]["workflow_summary"]["workflow_id"], "workflow")
        self.assertEqual(packet["warm"]["selected_skill_pack_summaries"][0]["skill_id"], "skill")
        self.assertEqual(
            packet["cold_refs"],
            [
                {"ref_type": "knowledge", "refs": ["knowledge-ref"]},
                {"ref_type": "memory", "refs": ["memory-ref"]},
                {"ref_type": "repo_intelligence", "refs": ["repo-ref"]},
            ],
        )
        self.assertNotIn("raw_events", str(packet))
        self.assertNotIn("terminal_log", str(packet))

    def test_workflow_supervisor_packet_keeps_runtime_facts_as_refs(self) -> None:
        packet = build_harness_context_packet(
            mode="workflow_supervisor",
            user_goal="Finish run.",
            workflow_id="workflow",
            agent_id="planner",
            state_view={"status": "running", "round": 1},
            capability_set={"tools": [{"name": "inspect_artifact"}], "denied": [{"name": "push"}]},
            execution_results=[
                {
                    "artifact_id": "execution-result-ref",
                    "status": "blocked",
                    "summary": "Tests failed.",
                    "verification": {"status": "blocked", "evidence_refs": ["check-ref"]},
                    "raw_runtime_json": {"large": "must not inline"},
                }
            ],
            blocked_reasons=["Tests failed."],
            changed_files_summary={"modified": ["src/app.py"]},
            evidence_refs=["execution-result-ref"],
            native_event_refs=["native-event-ref"],
            diff_refs=["diff-ref"],
            log_refs=["log-ref"],
        )

        self.assertEqual(packet["hot"]["confirmed_goal"], "Finish run.")
        self.assertEqual(packet["hot"]["current_decision_needed"], "decide_continue_or_finish")
        self.assertEqual(packet["warm"]["run_state_summary"], {"status": "running", "round": 1})
        self.assertEqual(packet["warm"]["execution_result_summaries"][0]["artifact_id"], "execution-result-ref")
        self.assertNotIn("raw_runtime_json", str(packet))
        self.assertEqual(
            packet["cold_refs"],
            [
                {"ref_type": "evidence", "refs": ["execution-result-ref"]},
                {"ref_type": "native_runtime", "refs": ["native-event-ref"]},
                {"ref_type": "diff", "refs": ["diff-ref"]},
                {"ref_type": "log", "refs": ["log-ref"]},
            ],
        )

    def test_task_execution_packet_includes_execution_contract_without_large_blobs(self) -> None:
        large_text = "x" * 5000
        packet = build_harness_context_packet(
            mode="task_execution",
            user_goal="Do work.",
            workflow_id="workflow",
            agent_id="executor",
            work_item={"work_item_id": "work-1", "task_summary": "Do work.", "full_text": large_text},
            task_envelope={
                "round": 1,
                "work_item_id": "work-1",
                "task_summary": "Do work.",
                "constraints": ["Stay scoped."],
                "planner_order_ref": "planner-order-ref",
                "upstream_refs": ["upstream-ref"],
            },
            success_criteria=["Tests pass."],
            sandbox_policy={"workspace": "temp_worktree"},
            relevant_file_summaries=[{"path": "src/app.py", "summary": large_text}],
            file_refs=["file-ref"],
            evidence_refs=["evidence-ref"],
        )

        self.assertEqual(packet["hot"]["work_item"]["work_item_id"], "work-1")
        self.assertEqual(packet["hot"]["constraints"], ["Stay scoped."])
        self.assertEqual(packet["hot"]["success_criteria"], ["Tests pass."])
        self.assertEqual(packet["hot"]["sandbox_policy"], {"workspace": "temp_worktree"})
        self.assertNotIn(large_text, str(packet))
        self.assertEqual(
            packet["cold_refs"],
            [
                {"ref_type": "upstream", "refs": ["upstream-ref"]},
                {"ref_type": "planner_order", "refs": ["planner-order-ref"]},
                {"ref_type": "file", "refs": ["file-ref"]},
                {"ref_type": "evidence", "refs": ["evidence-ref"]},
            ],
        )

    def test_packet_separates_repo_evidence_from_knowledge_hints(self) -> None:
        packet = build_harness_context_packet(
            mode="task_execution",
            user_goal="Use repo context.",
            workflow_id="workflow",
            agent_id="executor",
            task_envelope={"round": 1, "work_item_id": "work", "task_summary": "Use repo context.", "planner_order_ref": "order"},
            repo_evidence=[
                {
                    "ref_id": "repo-read:1",
                    "kind": "repo_read",
                    "evidence_kind": "repo_evidence",
                    "path": "src/app.py",
                    "start_line": 1,
                    "end_line": 2,
                    "text": "x" * 5000,
                }
            ],
            knowledge_hints=[
                {
                    "id": "hint-1",
                    "source": "hybrid_rag",
                    "evidence_kind": "knowledge_hint",
                    "summary": "Old design note.",
                    "source_refs": ["knowledge-source-1"],
                    "requires_repo_verification": True,
                }
            ],
            repo_evidence_refs=["repo-read:1"],
        )

        self.assertEqual(packet["warm"]["repo_evidence"][0]["evidence_kind"], "repo_evidence")
        self.assertEqual(packet["warm"]["knowledge_hints"][0]["evidence_kind"], "knowledge_hint")
        self.assertNotIn("x" * 5000, str(packet))
        self.assertIn({"ref_type": "repo_evidence", "refs": ["repo-read:1"]}, packet["cold_refs"])
        self.assertIn({"ref_type": "knowledge", "refs": ["knowledge-source-1", "hint-1"]}, packet["cold_refs"])

    def test_agent_run_harness_context_carries_task_execution_packet(self) -> None:
        workflow = default_planner_led_agent_workflow()
        item = WorkItem(
            work_item_id="executor-work",
            merge_index=1,
            assignee_agent_id="executor",
            task_summary="Do work.",
            depends_on=[],
        )
        envelope = AgentTaskEnvelope(
            round=1,
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            assigned_agent_id=item.assignee_agent_id,
            task_summary=item.task_summary,
            planner_order_ref="planner-order-ref",
            upstream_refs=["upstream-ref"],
        )
        context = AgentRun(workflow, initial_data={"request": "Do work."})._harness_context(
            agent_id="executor",
            harness_id="task-execution-harness",
            mode="task_execution",
            profile_id="internal-fallback-task-executor",
            round_number=1,
            state_view={},
            capability_set={"tools": [{"name": "return_execution_result"}]},
            work_item=item,
            task_envelope=envelope,
        )

        packet = context.context_packet or {}
        self.assertEqual(packet["mode"], "task_execution")
        self.assertEqual(packet["hot"]["work_item"]["work_item_id"], "executor-work")
        self.assertEqual(packet["cold_refs"], [{"ref_type": "upstream", "refs": ["upstream-ref"]}, {"ref_type": "planner_order", "refs": ["planner-order-ref"]}])


if __name__ == "__main__":
    unittest.main()
