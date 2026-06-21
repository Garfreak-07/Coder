from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from coder_workbench.actions import ActionGateway, ActionSpec, RunContext
from coder_workbench.agent_graph.cache import GraphRunCache
from coder_workbench.agent_graph.schema import WorkItem
from coder_workbench.budget import BudgetBroker, BudgetLimit
from coder_workbench.skills import SkillIndex, SkillIndexEntry


class ActionGatewayTests(unittest.TestCase):
    def test_build_context_action_routes_to_context_service(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            cache = GraphRunCache(round=1)
            item = _work_item()
            result = ActionGateway().run(
                ActionSpec(action_id="ctx-1", action_type="build_context"),
                run_context=RunContext(
                    run_id="run",
                    repo_root=tmp,
                    cache=cache,
                    item=item,
                    planner_order_ref="planner_order_round_1",
                    upstream_refs=[],
                    user_request="Build context.",
                    role="worker",
                    skill_index=SkillIndex(),
                    skill_store_root=Path(tmp) / ".coder",
                    repo_intelligence={},
                ),
            )

        self.assertEqual(result.status, "ok")
        self.assertEqual(result.payload["envelope"].assigned_agent_id, "executor")
        self.assertIn("executor-work", cache.context_packets_v2)
        self.assertEqual(cache.token_ledger[0]["work_item_id"], "executor-work")

    def test_denied_context_budget_blocks_before_service_call(self) -> None:
        gateway = ActionGateway(
            budget_broker=BudgetBroker(
                BudgetLimit(max_estimated_tokens=1, max_context_tokens_per_call=1)
            )
        )

        result = gateway.run(
            ActionSpec(action_id="ctx-1", action_type="build_context", estimated_tokens=10),
            run_context=RunContext(run_id="run", repo_root=".", item=_work_item(), skill_index=SkillIndex()),
        )

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.error_code, "context_budget_exceeded")

    def test_context_budget_can_fallback_by_omitting_skills(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            gateway = ActionGateway(
                budget_broker=BudgetBroker(
                    BudgetLimit(max_estimated_tokens=100, max_context_tokens_per_call=10)
                )
            )
            cache = GraphRunCache(round=1)
            result = gateway.run(
                ActionSpec(action_id="ctx-1", action_type="build_context"),
                run_context=RunContext(
                    run_id="run",
                    repo_root=tmp,
                    cache=cache,
                    item=_work_item(),
                    planner_order_ref="planner_order_round_1",
                    upstream_refs=[],
                    user_request="small",
                    role="worker",
                    skill_index=SkillIndex(
                        skills=[
                            SkillIndexEntry(
                                id="large-skill",
                                name="Large Skill",
                                description="Large skill context.",
                                category="coding",
                                risk_level="low",
                                trust_level="official",
                                max_skill_tokens=50,
                            )
                        ]
                    ),
                    skill_store_root=Path(tmp) / ".coder",
                    repo_intelligence={},
                ),
            )

        self.assertEqual(result.status, "ok")
        self.assertTrue(result.payload["budget_compressed"])
        self.assertEqual(result.payload["skill_route"].allowed_skill_ids, [])

    def test_propose_patch_action_routes_to_patch_service(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            target = repo / "src" / "example.txt"
            target.parent.mkdir()
            target.write_text("before\n", encoding="utf-8")

            result = ActionGateway().run(
                ActionSpec(
                    action_id="patch-1",
                    action_type="propose_patch",
                    input={"changes": [{"path": "src/example.txt", "action": "update", "content": "after\n"}]},
                ),
                run_context=RunContext(run_id="run", repo_root=repo, scopes=["src"]),
            )

        self.assertEqual(result.status, "ok")
        self.assertEqual(result.payload["preview"]["status"], "proposed")
        self.assertEqual(result.payload["preview"]["change_count"], 1)

    def test_unknown_action_type_fails_cleanly(self) -> None:
        result = ActionGateway().run(
            ActionSpec(action_id="unknown", action_type="unknown"),
            run_context=RunContext(run_id="run", repo_root="."),
        )

        self.assertEqual(result.status, "failed")
        self.assertEqual(result.error_code, "unknown_action_type")


def _work_item() -> WorkItem:
    return WorkItem(
        work_item_id="executor-work",
        merge_index=1,
        assignee_agent_id="executor",
        task_summary="Do work.",
        depends_on=[],
        tester_agent_ids=[],
    )


if __name__ == "__main__":
    unittest.main()
