from __future__ import annotations

import unittest

from coder_workbench.core.artifacts import ArtifactValidationError, validate_artifact
from coder_workbench.core.planner_chat_artifacts import PlannerChatTurn


class PlannerChatArtifactTests(unittest.TestCase):
    def test_valid_discuss_continue_chat(self) -> None:
        turn = PlannerChatTurn.model_validate(
            {
                "assistant_message": "I need one more detail before planning.",
                "interaction_mode": "discuss",
                "decision": "continue_chat",
                "visible_thinking": {"phase": "clarifying", "summary": "Clarifying scope."},
                "task_state": {
                    "goal": "Improve planner chat.",
                    "readiness": "needs_clarification",
                    "open_questions": ["Which repo area should be included?"],
                },
            }
        )

        self.assertEqual(turn.artifact_type, "planner_chat_turn")
        self.assertEqual(turn.task_state.readiness, "needs_clarification")

    def test_valid_discuss_produce_plan(self) -> None:
        artifact = validate_artifact(
            {
                "artifact_type": "planner_chat_turn",
                "assistant_message": "Here is the draft plan.",
                "interaction_mode": "discuss",
                "decision": "produce_plan",
                "visible_thinking": {"phase": "planning", "summary": "Drafting a user-visible plan."},
                "task_state": {
                    "goal": "Add planner sessions.",
                    "success_criteria": ["Session state persists."],
                    "plan_steps": [{"id": "step-1", "summary": "Add contracts.", "status": "ready"}],
                    "readiness": "ready_to_plan",
                },
            },
            expected_type="planner_chat_turn",
        )

        self.assertEqual(artifact["decision"], "produce_plan")

    def test_discuss_start_workflow_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "Discuss mode"):
            PlannerChatTurn.model_validate(_ready_start_turn(interaction_mode="discuss"))

    def test_work_start_workflow_ready_to_execute_succeeds(self) -> None:
        turn = PlannerChatTurn.model_validate(_ready_start_turn())

        self.assertEqual(turn.decision, "start_workflow")
        self.assertEqual(turn.handoff.workflow_request, "Implement the ready task.")

    def test_work_start_workflow_with_open_questions_is_rejected(self) -> None:
        payload = _ready_start_turn()
        payload["task_state"]["open_questions"] = ["Still unclear."]

        with self.assertRaisesRegex(ValueError, "open_questions"):
            PlannerChatTurn.model_validate(payload)

    def test_unknown_extra_field_rejected(self) -> None:
        payload = _ready_start_turn()
        payload["private_chain_of_thought"] = "hidden"

        with self.assertRaises(ArtifactValidationError):
            validate_artifact(payload, expected_type="planner_chat_turn")

    def test_visible_thinking_exists_and_is_not_empty(self) -> None:
        payload = _ready_start_turn()
        payload["visible_thinking"]["summary"] = ""

        with self.assertRaisesRegex(ValueError, "String should have at least 1 character"):
            PlannerChatTurn.model_validate(payload)

    def test_needs_clarification_requires_open_questions(self) -> None:
        payload = _ready_start_turn(decision="continue_chat")
        payload["task_state"]["readiness"] = "needs_clarification"
        payload["task_state"]["open_questions"] = []
        payload["handoff"] = None

        with self.assertRaisesRegex(ValueError, "needs_clarification"):
            PlannerChatTurn.model_validate(payload)

    def test_workflow_activity_update_validates(self) -> None:
        artifact = validate_artifact(
            {
                "artifact_type": "workflow_activity_update",
                "visible_phase": "executing",
                "user_message": "Executor work is in progress.",
                "steps": [
                    {"id": "plan", "label": "Plan", "status": "done"},
                    {"id": "execute", "label": "Execute", "status": "active"},
                ],
                "safety": [{"policy": "readonly_supervisor", "status": "enforced"}],
                "technical_refs": {"evidence_refs": ["event-1"]},
            },
            expected_type="workflow_activity_update",
        )

        self.assertEqual(artifact["visible_phase"], "executing")

    def test_workflow_activity_update_rejects_full_technical_payloads(self) -> None:
        with self.assertRaises(ArtifactValidationError):
            validate_artifact(
                {
                    "artifact_type": "workflow_activity_update",
                    "visible_phase": "checking",
                    "user_message": "Checking the result.",
                    "steps": [{"id": "check", "label": "Check", "status": "active"}],
                    "technical_refs": {"full_diff": "diff --git ..."},
                },
                expected_type="workflow_activity_update",
            )


def _ready_start_turn(*, interaction_mode: str = "work", decision: str = "start_workflow") -> dict:
    return {
        "artifact_type": "planner_chat_turn",
        "assistant_message": "I have enough detail and will start the workflow.",
        "interaction_mode": interaction_mode,
        "decision": decision,
        "visible_thinking": {"phase": "ready_to_start", "summary": "Ready to start the workflow."},
        "task_state": {
            "goal": "Implement the ready task.",
            "scope": ["src"],
            "success_criteria": ["The implementation is complete and tested."],
            "open_questions": [],
            "readiness": "ready_to_execute",
        },
        "handoff": {
            "workflow_request": "Implement the ready task.",
            "scope": ["src"],
            "success_criteria": ["The implementation is complete and tested."],
            "risks": ["Tests may need local dependencies."],
        },
    }


if __name__ == "__main__":
    unittest.main()
