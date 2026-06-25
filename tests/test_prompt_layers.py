from __future__ import annotations

import unittest

from coder_workbench.agent_graph.prompts import (
    build_planner_decision_prompt,
    build_planner_order_prompt,
    build_worker_execution_prompt,
)
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, PlannerInputBundle, WorkItem
from coder_workbench.agent_harness.contracts import CODE_WORKER_HARNESS, PLANNER_ORDER_HARNESS
from coder_workbench.agent_harness.prompt_layers import default_prompt_layer_config
from coder_workbench.agent_model import AgentRecipe, RuntimeProfileCompiler
from coder_workbench.core import compile_runtime_profiles, default_planner_led_agent_workflow


class PromptLayerTests(unittest.TestCase):
    def test_planner_order_prompt_has_ordered_contract_state_and_capability_layers(self) -> None:
        workflow = default_planner_led_agent_workflow()
        prompt = build_planner_order_prompt(
            request="Plan layered prompt work.",
            agent_workflow=workflow,
            state_view={"planner": {"planner_order_ref": None}},
            capability_set={"harness_id": PLANNER_ORDER_HARNESS.harness_id, "tools": []},
        )

        self.assertLess(prompt.index("Output Contract:"), prompt.index("Planner Order Rules:"))
        self.assertLess(prompt.index("Planner Order Rules:"), prompt.index("Harness Contract JSON:"))
        self.assertLess(prompt.index("Harness Contract JSON:"), prompt.index("AgentWorkflow JSON:"))
        self.assertIn(PLANNER_ORDER_HARNESS.harness_id, prompt)
        self.assertIn("PlannerStateView JSON:", prompt)
        self.assertIn("Resolved CapabilitySet JSON:", prompt)

    def test_worker_prompt_has_code_worker_harness_and_execution_layers(self) -> None:
        workflow = default_planner_led_agent_workflow()
        agent = workflow.agents[1]
        item = WorkItem(
            work_item_id="executor-work",
            merge_index=1,
            assignee_agent_id=agent.id,
            task_summary="Do the work.",
            depends_on=[],
        )
        envelope = AgentTaskEnvelope(
            round=1,
            work_item_id=item.work_item_id,
            assigned_agent_id=agent.id,
            merge_index=1,
            task_summary=item.task_summary,
            planner_order_ref="planner_order_round_1",
            upstream_refs=[],
            coding_context_packet={"artifact_type": "coding_context_packet", "work_item_id": item.work_item_id},
            capability_set={"harness_id": CODE_WORKER_HARNESS.harness_id},
        )

        prompt = build_worker_execution_prompt(agent=agent, item=item, envelope=envelope)

        self.assertLess(prompt.index("Output Contract:"), prompt.index("Executor Rules:"))
        self.assertLess(prompt.index("Executor Rules:"), prompt.index("Harness Contract JSON:"))
        self.assertIn(CODE_WORKER_HARNESS.harness_id, prompt)
        self.assertIn("AgentTaskEnvelope JSON:", prompt)
        self.assertIn("CodingContextPacket JSON:", prompt)
        self.assertIn("Resolved CapabilitySet JSON:", prompt)

    def test_planner_prompts_do_not_include_legacy_human_response_layer(self) -> None:
        workflow = default_planner_led_agent_workflow()
        order_prompt = build_planner_order_prompt(
            request="Plan without legacy response state.",
            agent_workflow=workflow,
        )
        decision_prompt = build_planner_decision_prompt(
            planner=workflow.agents[0],
            bundle=PlannerInputBundle(
                round=1,
                planner_order_ref="planner_order_round_1",
                plan_status="completed",
                items=[],
            ),
        )

        for prompt in [order_prompt, decision_prompt]:
            with self.subTest(prompt=prompt[:40]):
                self.assertNotIn("planner_human_response", prompt)
                self.assertNotIn("Planner human response JSON", prompt)

    def test_runtime_profiles_record_prompt_layer_policy(self) -> None:
        compiler = RuntimeProfileCompiler()
        planner = compiler.compile(AgentRecipe(id="planner", name="Planner", role="planner"))
        executor = compiler.compile(AgentRecipe(id="executor", name="Executor", role="executor"))

        self.assertEqual(planner.prompt_layers, default_prompt_layer_config("planner"))
        self.assertEqual(executor.prompt_layers, default_prompt_layer_config("executor"))

        core_profiles = compile_runtime_profiles(default_planner_led_agent_workflow())
        self.assertTrue(all(profile.prompt_layers for profile in core_profiles))


if __name__ == "__main__":
    unittest.main()
