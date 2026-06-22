from __future__ import annotations

import sys
import tempfile
import time
import unittest
from pathlib import Path

from coder_workbench.core.schema import WorkflowSpec
from coder_workbench.runtime import run_workflow
from coder_workbench.server.manager import RunManager
from coder_workbench.server.storage import RunStore


def _command_workflow(command: str) -> WorkflowSpec:
    return WorkflowSpec.model_validate(
        {
            "id": "command-approval-test",
            "name": "Command approval test",
            "max_steps": 10,
            "nodes": [
                {"id": "start", "type": "start"},
                {"id": "check", "type": "tool", "tool": "run_check", "input": {"command": command}, "output_key": "check_result"},
                {"id": "end", "type": "end"},
            ],
            "edges": [
                {"from": "start", "to": "check"},
                {"from": "check", "to": "end"},
            ],
        }
    )


class CommandApprovalTests(unittest.TestCase):
    def test_run_check_blocks_until_matching_command_is_approved(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            command = f'"{sys.executable}" -c "print(123)"'
            workflow = _command_workflow(command)

            blocked = run_workflow(workflow, "run check", tmp, initial_data={"scopes": []})

            self.assertEqual(blocked.status, "blocked")
            self.assertEqual(blocked.blocked_node_id, "check")
            check_result = blocked.data["check_result"]
            self.assertTrue(check_result["blocked"])
            approval_key = check_result["approval_key"]

            checkpoint = dict(blocked.resume_checkpoint)
            checkpoint["data"] = dict(checkpoint["data"])
            checkpoint["data"]["command_approvals"] = {approval_key: True}
            resumed = run_workflow(
                workflow,
                "run check",
                tmp,
                resume_checkpoint=checkpoint,
                prior_events=blocked.events,
                resume_after_node=blocked.blocked_node_id,
            )

            self.assertEqual(resumed.status, "completed")
            self.assertTrue(resumed.data["check_result"]["passed"])
            self.assertIn("123", resumed.data["check_result"]["output"])

    def test_live_run_approval_records_command_audit(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store_root = Path(tmp) / ".coder"
            repo = Path(tmp) / "repo"
            repo.mkdir()
            command = f'"{sys.executable}" -c "print(456)"'
            manager = RunManager(RunStore(store_root))
            run = manager.start(_command_workflow(command), str(repo), "run check", {"scopes": []})

            _wait_for_status(run, "blocked")
            manager.approve(run.id, approved=True)
            _wait_for_status(run, "completed")

            self.assertIsNotNone(run.result)
            assert run.result is not None
            audit = run.result.data["approval_audit"]
            self.assertEqual(len(audit), 1)
            self.assertEqual(audit[0]["approval_type"], "command")
            self.assertEqual(audit[0]["command"], command)
            self.assertTrue(audit[0]["approved"])
            self.assertTrue(run.result.data["check_result"]["passed"])

            self.assertIsNotNone(run.stored_run_id)
            assert run.stored_run_id is not None
            stored = manager.store.get(run.stored_run_id)
            self.assertEqual(stored.result.data["approval_audit"][0]["approval_type"], "command")

    def test_live_run_reject_records_audit_and_stops(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store_root = Path(tmp) / ".coder"
            repo = Path(tmp) / "repo"
            repo.mkdir()
            command = f'"{sys.executable}" -c "print(789)"'
            manager = RunManager(RunStore(store_root))
            run = manager.start(_command_workflow(command), str(repo), "run check", {"scopes": []})

            _wait_for_status(run, "blocked")
            manager.approve(run.id, approved=False, reason="not safe")
            _wait_for_status(run, "failed")

            self.assertEqual(run.error, "not safe")
            audit = run.initial_data["approval_audit"]
            self.assertEqual(audit[0]["approval_type"], "command")
            self.assertFalse(audit[0]["approved"])
            self.assertEqual(audit[0]["reason"], "not safe")

    def test_live_run_snapshots_can_be_loaded_by_new_manager(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store = RunStore(Path(tmp) / ".coder")
            repo = Path(tmp) / "repo"
            repo.mkdir()
            command = f'"{sys.executable}" -c "print(999)"'
            manager = RunManager(store)
            run = manager.start(_command_workflow(command), str(repo), "run check", {"scopes": []})
            _wait_for_status(run, "blocked")

            restored = RunManager(store).get(run.id)

            self.assertEqual(restored.status, "blocked")
            self.assertEqual(restored.workflow.id, "command-approval-test")
            self.assertGreaterEqual(len(restored.events), 1)

    def test_restored_blocked_live_run_can_be_approved_after_manager_restart(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store = RunStore(Path(tmp) / ".coder")
            repo = Path(tmp) / "repo"
            repo.mkdir()
            command = f'"{sys.executable}" -c "print(321)"'
            manager = RunManager(store)
            run = manager.start(_command_workflow(command), str(repo), "run check", {"scopes": []})
            _wait_for_status(run, "blocked")

            restarted = RunManager(store)
            restored = restarted.get(run.id)
            restarted.approve(restored.id, approved=True)
            _wait_for_status(restored, "completed")

            self.assertIsNotNone(restored.result)
            assert restored.result is not None
            self.assertEqual(restored.result.status, "completed")
            self.assertTrue(restored.result.data["check_result"]["passed"])
            self.assertIn("321", restored.result.data["check_result"]["output"])
            self.assertIsNotNone(restored.stored_run_id)


def _wait_for_status(run, status: str, timeout: float = 5.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if run.status == status:
            return
        time.sleep(0.02)
    raise AssertionError(f"timed out waiting for {status}, last status={run.status}")


if __name__ == "__main__":
    unittest.main()
