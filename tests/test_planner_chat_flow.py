from __future__ import annotations

import tempfile
import unittest

from fastapi.testclient import TestClient

from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.core.artifacts import validate_artifact
from coder_workbench.server.app import create_app


class PlannerChatFlowTests(unittest.TestCase):
    def test_project_plan_and_run_contract_drafts_validate(self) -> None:
        plan = validate_artifact(
            {
                "artifact_type": "project_plan_draft",
                "draft_id": "draft-1",
                "summary": "Plan the work.",
                "proposed_scope": ["src"],
                "success_criteria": ["Confirm before execution."],
                "risks": ["Missing dependency."],
                "requires_confirmation": True,
            },
            expected_type="project_plan_draft",
        )
        contract = validate_artifact(
            {
                "artifact_type": "run_contract_draft",
                "draft_id": "draft-1",
                "user_goal": "Do work.",
                "workflow_id": "default-planner-led",
                "planner_agent_id": "planner",
                "success_criteria": ["Confirm before execution."],
                "constraints": ["No execution before confirmation."],
                "requires_confirmation": True,
            },
            expected_type="run_contract_draft",
        )

        self.assertEqual(plan["artifact_type"], "project_plan_draft")
        self.assertEqual(contract["artifact_type"], "run_contract_draft")

    def test_draft_does_not_start_run_and_confirm_controls_execution(self) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as store, tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as repo:
            client = TestClient(create_app(store_root=store, frontend_dist=store))

            draft_response = client.post(
                "/api/v2/planner-chat/draft",
                json={
                    "request": "Update docs.",
                    "workflow_id": "default-planner-led",
                    "planner_agent_id": "planner",
                    "repo": repo,
                    "scopes": [],
                },
            )
            draft = draft_response.json()

            self.assertEqual(draft_response.status_code, 200)
            self.assertEqual(draft["artifact_type"], "project_plan_draft")
            self.assertTrue(draft["requires_confirmation"])
            self.assertNotIn("run_id", draft)

            cancel_response = client.post(
                "/api/v2/planner-chat/confirm",
                json={"draft_id": draft["draft_id"], "approved": False},
            )
            self.assertEqual(cancel_response.status_code, 200)
            self.assertEqual(cancel_response.json()["status"], "cancelled")

            second_draft = client.post(
                "/api/v2/planner-chat/draft",
                json={
                    "request": "Update docs.",
                    "workflow_id": "default-planner-led",
                    "planner_agent_id": "planner",
                    "repo": repo,
                },
            ).json()
            confirm_response = client.post(
                "/api/v2/planner-chat/confirm",
                json={"draft_id": second_draft["draft_id"], "approved": True},
            )
            confirm = confirm_response.json()

            self.assertEqual(confirm_response.status_code, 200)
            self.assertIn("run_id", confirm)
            self.assertIn(confirm["status"], {"queued", "running", "completed", "blocked", "failed"})

    def test_draft_can_use_supplied_workflow_payload(self) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as store, tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as repo:
            client = TestClient(create_app(store_root=store, frontend_dist=store))
            workflow = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True, exclude_none=True)
            workflow["id"] = "unsaved-workflow"
            workflow["name"] = "Unsaved Workflow"

            response = client.post(
                "/api/v2/planner-chat/draft",
                json={
                    "request": "Use the current edited workflow.",
                    "workflow_id": "missing-from-library",
                    "planner_agent_id": "planner",
                    "repo": repo,
                    "agent_workflow": workflow,
                },
            )
            draft = response.json()

            self.assertEqual(response.status_code, 200)
            self.assertIn("Unsaved Workflow", draft["summary"])


if __name__ == "__main__":
    unittest.main()
