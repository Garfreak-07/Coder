from __future__ import annotations

import tempfile
import unittest

from fastapi.testclient import TestClient

from coder_workbench.core import (
    AgentWorkflowSpec,
    compile_runtime_profiles,
    default_planner_led_agent_workflow,
    role_card_catalog,
    validate_agent_workflow_payload,
)
from coder_workbench.server.app import create_app


class AgentArchetypeTests(unittest.TestCase):
    def test_role_card_catalog_contains_ordinary_user_choices(self) -> None:
        labels = {card["label"] for card in role_card_catalog()}

        self.assertEqual(
            labels,
            {"Do work", "Check result", "Organize information", "Research sources", "Write draft"},
        )

    def test_role_card_payload_compiles_default_role_and_capabilities(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload["agents"][1] = {
            "id": "executor",
            "name": "Worker",
            "role_card": "do_work",
            "model_tier": "standard",
            "can_talk_to_human": False,
        }

        spec = AgentWorkflowSpec.model_validate(payload)
        validation = validate_agent_workflow_payload(payload)

        self.assertEqual(validation.status, "pass")
        self.assertEqual(spec.agents[1].role, "worker")
        self.assertEqual(spec.agents[1].capabilities, ["follow_planner_order", "modify_files", "return_execution_result"])

    def test_runtime_profile_compiles_runtime_managed_internals(self) -> None:
        payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
        payload["agents"][1]["role_card"] = "do_work"
        workflow = AgentWorkflowSpec.model_validate(payload)

        profiles = compile_runtime_profiles(workflow)
        worker = next(profile for profile in profiles if profile.agent_id == "executor")

        self.assertEqual(worker.agent_archetype, "worker")
        self.assertEqual(worker.authority.authority, "worker")
        self.assertTrue(worker.tool_policy["edit_files"])
        self.assertTrue(worker.token_budget["managed_by_runtime"])
        self.assertEqual(worker.internal_loops["schema_repair_attempts"], 1)


class AgentArchetypeApiTests(unittest.TestCase):
    def test_role_cards_and_runtime_profiles_api(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            role_cards = client.get("/api/v2/agent-role-cards")

            self.assertEqual(role_cards.status_code, 200)
            self.assertEqual(len(role_cards.json()["role_cards"]), 5)

            payload = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)
            payload["agents"][1]["role_card"] = "do_work"
            profiles = client.post("/api/v2/agent-workflows/runtime-profiles", json=payload)

        self.assertEqual(profiles.status_code, 200)
        executor = next(profile for profile in profiles.json()["profiles"] if profile["agent_id"] == "executor")
        self.assertEqual(executor["agent_archetype"], "worker")
        self.assertEqual(executor["tool_policy"]["connector_operations"], "deny_by_default")


if __name__ == "__main__":
    unittest.main()
