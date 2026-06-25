from __future__ import annotations

import unittest

from coder_workbench.harness_runtime import ArtifactProjector, HarnessRunResult


class ArtifactProjectorTests(unittest.TestCase):
    def test_projector_validates_provider_execution_result_and_merges_refs(self) -> None:
        artifact = ArtifactProjector().project(
            HarnessRunResult(
                status="completed",
                artifact_type="execution_result",
                artifact={
                    "artifact_type": "execution_result",
                    "round": 1,
                    "work_item_id": "work-1",
                    "agent_id": "executor",
                    "status": "completed",
                    "summary": "Changed one file.",
                    "changed_files": ["src/app.py"],
                    "verification": {
                        "status": "pass",
                        "checks_run": [],
                        "evidence_refs": ["check-ref"],
                        "confidence": "medium",
                    },
                },
                native_event_refs=["native-ref"],
                diff_refs=["diff-ref"],
                log_refs=["log-ref"],
            ),
            artifact_id="execution_result_work_1",
        )

        self.assertEqual(artifact["artifact_id"], "execution_result_work_1")
        self.assertEqual(artifact["status"], "completed")
        self.assertEqual(artifact["patch_refs"], ["diff-ref"])
        self.assertEqual(artifact["evidence_refs"], ["native-ref", "diff-ref", "log-ref"])
        self.assertEqual(artifact["verification"]["evidence_refs"], ["check-ref", "native-ref", "diff-ref", "log-ref"])

    def test_projector_synthesizes_blocked_execution_result_from_runtime_status(self) -> None:
        artifact = ArtifactProjector().project(
            HarnessRunResult(
                status="failed",
                artifact_type="execution_result",
                error={"code": "provider_error", "message": "Provider failed."},
                native_event_refs=["native-ref"],
                log_refs=["log-ref"],
            )
        )

        self.assertEqual(artifact["status"], "blocked")
        self.assertEqual(artifact["blocker_type"], "unknown_error")
        self.assertTrue(artifact["executor_recovery_exhausted"])
        self.assertEqual(artifact["verification"]["status"], "blocked")
        self.assertIn("Provider failed.", artifact["unexpected_issues"])

    def test_projector_synthesizes_planner_artifacts(self) -> None:
        projector = ArtifactProjector()
        order = projector.project(HarnessRunResult(status="completed", artifact_type="planner_order"))
        decision = projector.project(HarnessRunResult(status="completed", artifact_type="planner_decision"))
        report = projector.project(
            HarnessRunResult(status="completed", artifact_type="final_report", evidence_refs=["evidence-ref"])
        )

        self.assertEqual(order["artifact_type"], "planner_order")
        self.assertEqual(order["plan_graph"]["work_items"], [])
        self.assertEqual(decision["next_action"], "finish")
        self.assertEqual(report["status"], "completed")
        self.assertEqual(report["evidence_refs"], ["evidence-ref"])


if __name__ == "__main__":
    unittest.main()
