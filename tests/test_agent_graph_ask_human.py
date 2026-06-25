from __future__ import annotations

import tempfile
import time
import unittest

from fastapi.testclient import TestClient

from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.core import default_planner_led_agent_workflow
from coder_workbench.server.app import create_app


class AgentGraphAskHumanTests(unittest.TestCase):
    def test_legacy_planner_ask_human_becomes_blocked_final_report(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow()).run(
                "Ask the user.",
                tmp,
                initial_data={
                    "planner_decision": {
                        "artifact_type": "planner_decision",
                        "round": 1,
                        "task_done": False,
                        "next_action": "ask_human",
                        "reason": "Need confirmation.",
                        "human_message": "Confirm the next step?",
                    }
                },
            )

        self.assertEqual(result.status, "blocked")
        self.assertEqual(result.status_code, "planner_blocked")
        self.assertEqual(result.blocked_node_id, "planner")
        self.assertNotIn("planner.human_prompt", {event.type for event in result.events})
        self.assertNotIn("approval.required", {event.type for event in result.events})
        self.assertIsNone(result.resume_checkpoint)
        self.assertEqual(result.data["planner_decision"]["next_action"], "finish")
        self.assertEqual(result.data["planner_decision"]["final_status"], "blocked")
        self.assertEqual(result.data["final_report"]["status"], "blocked")

    def test_live_agent_run_rejects_legacy_planner_response_resume(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            client = TestClient(create_app(store_root=tmp, frontend_dist=tmp))
            agent_workflow = default_planner_led_agent_workflow().model_dump(mode="json", by_alias=True)

            response = client.post(
                "/api/v2/live-agent-runs",
                json={
                    "repo": tmp,
                    "request": "Ask before finishing.",
                    "agent_workflow": agent_workflow,
                    "approved": True,
                    "scopes": [],
                    "initial_data": {
                        "planner_decision": {
                            "artifact_type": "planner_decision",
                            "round": 1,
                            "task_done": False,
                            "next_action": "ask_human",
                            "reason": "Need confirmation.",
                            "human_message": "Proceed?",
                        }
                    },
                },
            )
            self.assertEqual(response.status_code, 200)
            run_id = response.json()["run_id"]
            detail = _wait_for_status(client, run_id, "blocked")
            self.assertEqual(detail["status"], "blocked")
            self.assertEqual(detail["result"]["status_code"], "planner_blocked")
            self.assertEqual(detail["result"]["data"]["final_report"]["status"], "blocked")

            resume = client.post(
                f"/api/v2/live-agent-runs/{run_id}/planner-response",
                json={"response": "Proceed.", "data": {"confirmed": True}},
            )
            self.assertEqual(resume.status_code, 405)


def _wait_for_status(client: TestClient, run_id: str, expected: str) -> dict:
    detail = client.get(f"/api/v2/live-agent-runs/{run_id}").json()
    for _ in range(50):
        if detail["status"] == expected:
            return detail
        time.sleep(0.02)
        detail = client.get(f"/api/v2/live-agent-runs/{run_id}").json()
    return detail


if __name__ == "__main__":
    unittest.main()
