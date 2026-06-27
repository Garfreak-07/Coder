from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from coder_workbench.memory.run_memory import RunMemoryStore, build_run_memory_snapshot
from coder_workbench.runtime import RunResult
from coder_workbench.server.storage import RunStore


class RunMemoryTests(unittest.TestCase):
    def test_snapshot_includes_planner_task_state_and_execution_summary(self) -> None:
        snapshot = build_run_memory_snapshot(
            run_id="run-1",
            workflow_id="workflow",
            status="completed",
            data=_data(),
            artifacts=_artifacts(),
        )

        self.assertEqual(snapshot.planner_task_state["goal"], "Ship memory.")
        self.assertEqual(snapshot.planner_order_ref, "planner_order_round_1")
        self.assertEqual(snapshot.planner_decision_ref, "planner_decision_round_1")
        self.assertEqual(snapshot.work_items[0].work_item_id, "work-1")
        self.assertEqual(snapshot.execution_result_summaries[0]["artifact_id"], "execution_result_work-1")
        self.assertEqual(snapshot.verification_summaries[0]["status"], "skipped")

    def test_run_memory_store_writes_latest_snapshot_and_checkpoints(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / ".coder"
            snapshot = build_run_memory_snapshot(
                run_id="run-1",
                workflow_id="workflow",
                status="completed",
                data=_data(),
                artifacts=_artifacts(),
            )

            RunMemoryStore(root).write_result_checkpoints(snapshot)

            latest = root / "runs" / "run-1" / "memory" / "latest_snapshot.json"
            checkpoints = root / "runs" / "run-1" / "memory" / "checkpoints.jsonl"
            self.assertTrue(latest.exists())
            self.assertTrue(checkpoints.exists())
            phases = [json.loads(line)["phase"] for line in checkpoints.read_text(encoding="utf-8").splitlines()]
            self.assertIn("planner_order", phases)
            self.assertIn("execution_result", phases)
            self.assertIn("planner_decision", phases)
            self.assertIn("final_report", phases)
            self.assertEqual(RunMemoryStore(root).latest_snapshot("run-1").run_id, "run-1")

    def test_run_store_save_creates_run_memory_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / ".coder"
            store = RunStore(root)
            stored = store.save(
                "workflow",
                "/repo",
                "Ship memory.",
                RunResult(
                    status="completed",
                    data=_data(),
                    summaries={},
                    artifacts=_artifacts(),
                    events=[],
                    estimated_tokens_used=1,
                    agent_calls=1,
                    tool_calls=0,
                ),
            )

            latest = root / "runs" / stored.id / "memory" / "latest_snapshot.json"
            self.assertTrue(latest.exists())
            snapshot = json.loads(latest.read_text(encoding="utf-8"))
            self.assertEqual(snapshot["planner_task_state"]["goal"], "Ship memory.")
            self.assertEqual(snapshot["status"], "completed")

    def test_blocked_reason_is_stored(self) -> None:
        artifacts = _artifacts(status="blocked", blocker_reason="Tests failed.")
        snapshot = build_run_memory_snapshot(
            run_id="run-1",
            workflow_id="workflow",
            status="blocked",
            data=_data(work_status="blocked"),
            artifacts=artifacts,
        )

        self.assertEqual(snapshot.status, "blocked")
        self.assertEqual(snapshot.blocked_reasons, ["Tests failed."])
        self.assertEqual(snapshot.work_items[0].status, "blocked")

    def test_snapshot_omits_raw_logs_diffs_prompts_and_model_outputs(self) -> None:
        data = _data()
        data["planner_task_state"]["raw_prompt"] = "do not store"
        artifacts = _artifacts()
        artifacts["execution_result_work-1"]["raw_runtime_json"] = {"full": True}
        artifacts["execution_result_work-1"]["full_diff"] = "diff --git ..."

        snapshot = build_run_memory_snapshot(
            run_id="run-1",
            workflow_id="workflow",
            status="completed",
            data=data,
            artifacts=artifacts,
        )

        dumped = json.dumps(snapshot.model_dump(mode="json"), ensure_ascii=False)
        self.assertNotIn("raw_prompt", dumped)
        self.assertNotIn("raw_runtime_json", dumped)
        self.assertNotIn("full_diff", dumped)
        self.assertNotIn("do not store", dumped)
        self.assertNotIn("diff --git", dumped)


def _data(*, work_status: str = "completed") -> dict:
    return {
        "planner_chat_session_id": "session-1",
        "planner_task_state": {"goal": "Ship memory.", "readiness": "ready_to_execute"},
        "shared_run_state": {
            "run_id": "run-1",
            "workflow_id": "workflow",
            "user_request": "Ship memory.",
            "control": {"status": work_status, "round": 1, "blocked_recovery_used": False},
            "planner": {
                "planner_order_ref": "planner_order_round_1",
                "planner_decision_ref": "planner_decision_round_1",
                "round_summary_ref": "round_summary_round_1",
            },
            "work_items": {
                "work-1": {
                    "work_item_id": "work-1",
                    "agent_id": "executor",
                    "status": work_status,
                    "summary": "Implemented.",
                    "execution_result_ref": "execution_result_work-1",
                }
            },
            "messages": [],
            "artifacts": {},
            "tool_results": {},
            "blobs": {},
            "memory_refs": [],
            "final_report_ref": "final_report",
            "debug_refs": [],
        },
        "rounds": [
            {
                "round": 1,
                "planner_order": "planner_order_round_1",
                "planner_decision": "planner_decision_round_1",
            }
        ],
        "graph_run_cache": {
            "native_runtime_refs": {"work-1": ["native-1"]},
            "diff_refs": {"work-1": ["diff-1"]},
            "log_refs": {"work-1": ["log-1"]},
        },
        "final_report": {
            "artifact_type": "final_report",
            "status": work_status,
            "summary": "Done.",
            "next_steps": [],
            "evidence_refs": ["final-evidence"],
        },
    }


def _artifacts(*, status: str = "completed", blocker_reason: str | None = None) -> dict:
    return {
        "execution_result_work-1": {
            "artifact_id": "execution_result_work-1",
            "artifact_type": "execution_result",
            "round": 1,
            "merge_index": 1,
            "work_item_id": "work-1",
            "agent_id": "executor",
            "status": status,
            "summary": "Implemented." if status == "completed" else blocker_reason,
            "changed_files": ["src/app.py"],
            "created_files": [],
            "deleted_files": [],
            "patch_refs": ["diff-1"],
            "evidence_refs": ["execution-evidence"],
            "blocker_reason": blocker_reason,
            "verification": {
                "status": "skipped",
                "checks_run": [],
                "evidence_refs": ["verification-evidence"],
                "remaining_work": [],
                "no_check_rationale": "No checks configured.",
            },
        },
        "final_report": {
            "artifact_type": "final_report",
            "status": status,
            "summary": "Done.",
            "next_steps": [],
            "evidence_refs": ["final-evidence"],
        },
    }


if __name__ == "__main__":
    unittest.main()
