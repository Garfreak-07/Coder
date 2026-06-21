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
from coder_workbench.skills import InstalledSkillStore, build_skill_index
from coder_workbench.skills.schema import SkillPackageManifest


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
        self.assertIn("agent_evaluation_reports", result.data)
        runtime_profiles = result.data["runtime_profiles"]
        executor_profile = next(profile for profile in runtime_profiles if profile["agent_id"] == "executor")
        self.assertEqual(executor_profile["authority"]["authority"], "worker")
        executor_report = next(report for report in result.data["agent_evaluation_reports"] if report["agent_id"] == "executor")
        self.assertEqual(executor_report["calls"], 1)
        self.assertEqual(executor_report["schema_valid_rate"], 1.0)
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
            ledger_entries = store.partitions.ledgers.list(stored.id)

        self.assertEqual(loaded["artifact_type"], "planner_input_bundle")
        self.assertEqual(loaded["items"][0]["work_item_id"], "executor-work")
        self.assertEqual(ledger_entries[0]["work_item_id"], "executor-work")

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

    def test_runner_events_carry_trace_and_span_hierarchy(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow()).run("Trace the run.", tmp)

        trace_ids = {event.payload.get("trace_id") for event in result.events}
        self.assertEqual(len(trace_ids), 1)
        self.assertIn("trace_id", result.data)
        self.assertEqual(trace_ids, {result.data["trace_id"]})

        spans = {span["span_id"]: span for span in result.data["trace_spans"]}
        wave_event = next(event for event in result.events if event.type == "agent_graph.wave.started")
        agent_started = next(event for event in result.events if event.type == "agent_task.started")
        action_completed = next(event for event in result.events if event.type == "action.completed")

        self.assertEqual(agent_started.payload["parent_span_id"], wave_event.payload["span_id"])
        self.assertEqual(
            action_completed.payload["parent_span_id"],
            agent_started.payload["span_id"],
        )
        self.assertEqual(spans[action_completed.payload["span_id"]]["kind"], "action")

    def test_runner_routes_installed_skills_into_task_envelope_and_ledger(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            skill_store = _install_skill(Path(tmp) / ".coder")
            result = AgentGraphRunner(default_planner_led_agent_workflow()).run(
                "Research GitHub repositories for comparable tools.",
                tmp,
                initial_data={"skill_index": build_skill_index(skill_store.list_installed()).model_dump(mode="json")},
            )

        self.assertEqual(result.status, "completed")
        cache = result.data["graph_run_cache"]
        task = cache["agent_tasks"]["executor-work"]
        self.assertEqual(task["allowed_skill_ids"], ["github-research"])
        self.assertEqual(task["loaded_skill_refs"], ["skill:github-research:SKILL.md"])
        self.assertEqual(task["selected_skill_context"][0]["skill_id"], "github-research")
        self.assertIn("# GitHub Research", task["selected_skill_context"][0]["content"])
        self.assertEqual(cache["skill_routes"]["executor-work"]["allowed_skill_ids"], ["github-research"])
        self.assertEqual(
            cache["context_packets_v2"]["executor-work"]["included_skill_ids"],
            ["github-research"],
        )
        self.assertGreater(cache["token_ledger"][0]["skill_tokens_loaded"], 0)
        self.assertLess(cache["token_ledger"][0]["skill_tokens_loaded"], 1200)
        self.assertEqual(result.data["token_ledger"][0]["work_item_id"], "executor-work")
        skill_report = result.data["skill_evaluation_reports"][0]
        self.assertEqual(skill_report["skill_id"], "github-research")
        self.assertEqual(skill_report["success_when_used"], 1)
        self.assertGreater(skill_report["average_token_cost"], 0)
        self.assertGreater(result.estimated_tokens_used, 0)
        self.assertIn("skill.route.selected", {event.type for event in result.events})
        self.assertIn("agent.context_packet_v2", {event.type for event in result.events})
        self.assertIn("token.ledger.entry", {event.type for event in result.events})

    def test_organizer_role_card_records_synthesis_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(_workflow_with_organizer()).run(
                "Organize the available project facts.",
                tmp,
            )

        self.assertEqual(result.status, "completed")
        cache = result.data["graph_run_cache"]
        execution = cache["execution_cache"]["executor-work"]
        self.assertEqual(execution["artifact_type"], "synthesis_artifact")
        self.assertEqual(execution["execution_result_ref"], "synthesis_artifact_executor-work")
        self.assertEqual(cache["context_packets_v2"]["executor-work"]["artifact_type"], "synthesis_artifact")
        self.assertEqual(cache["token_ledger"][0]["artifact_type"], "synthesis_artifact")
        self.assertIn("synthesis_artifact_executor-work", result.artifacts)
        artifact = result.artifacts["synthesis_artifact_executor-work"]
        self.assertEqual(artifact["artifact_type"], "synthesis_artifact")
        self.assertEqual(artifact["status"], "completed")
        self.assertGreaterEqual(len(artifact["sources"]), 1)
        self.assertGreaterEqual(len(artifact["ranked_items"]), 1)
        self.assertIn("synthesis_artifact_executor-work", result.data["planner_input_bundle"]["items"][0]["refs"])
        executor_report = next(report for report in result.data["agent_evaluation_reports"] if report["agent_id"] == "executor")
        self.assertEqual(executor_report["agent_archetype"], "synthesizer")


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


def _workflow_with_organizer() -> AgentWorkflowSpec:
    payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
    payload["agents"][1] = {
        "id": "executor",
        "name": "Organizer",
        "role_card": "organize_information",
        "model_tier": "standard",
        "can_talk_to_human": False,
    }
    return AgentWorkflowSpec.model_validate(payload)


def _install_skill(root: Path) -> InstalledSkillStore:
    package_dir = root.parent / "skill-package"
    package_dir.mkdir(parents=True, exist_ok=True)
    manifest = SkillPackageManifest.model_validate(
        {
            "id": "github-research",
            "name": "GitHub Research",
            "version": "0.1.0",
            "description": "Search and compare open-source GitHub repositories.",
            "category": "research",
            "skill_type": "procedure",
            "risk_level": "low",
            "publisher": "coder-official",
            "allowed_authorities": ["planner", "worker", "tester", "synthesizer"],
            "requires": ["search_query"],
            "produces": ["source_collection", "execution_result"],
            "connectors": ["github_readonly"],
            "external_effect": False,
            "requires_preview": False,
            "requires_human_approval": False,
            "context_policy": {
                "load_mode": "on_demand",
                "max_skill_tokens": 1200,
            },
            "compatibility": {
                "coder_min_version": "0.7.0",
                "agent_graph_runtime": True,
            },
            "trigger_hints": ["github", "repository", "research"],
        }
    )
    (package_dir / "skill.json").write_text(manifest.model_dump_json(indent=2), encoding="utf-8")
    (package_dir / "SKILL.md").write_text(
        "# GitHub Research\n\nUse for GitHub source research.\n",
        encoding="utf-8",
    )
    store = InstalledSkillStore(root)
    store.install_from_directory(
        package_dir,
        manifest=manifest,
        package_sha256="0" * 64,
        trust_level="official",
    )
    return store


if __name__ == "__main__":
    unittest.main()
