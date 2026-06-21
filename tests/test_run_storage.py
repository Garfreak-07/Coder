from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from coder_workbench.runtime import RunEvent, RunResult
from coder_workbench.server.storage import RunStore, StoredRun


class RunStoreTests(unittest.TestCase):
    def test_split_run_layout_reconstructs_events_and_lists_from_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / ".coder"
            store = RunStore(root)
            result = _result()

            stored = store.save("workflow-1", "/repo", "do work", result)

            run_dir = root / "runs" / stored.id
            self.assertTrue((run_dir / "metadata.json").exists())
            self.assertTrue((run_dir / "result.json").exists())
            self.assertTrue((run_dir / "events.jsonl").exists())
            self.assertTrue((root / "runs" / "index.sqlite").exists())
            self.assertFalse((root / "runs" / f"{stored.id}.json").exists())
            self.assertEqual(store.partitions.events.read(stored.id)[0].type, "run.started")
            self.assertEqual(
                store.partitions.artifacts.read(stored.id, "execution_result_1")["summary"],
                "done",
            )
            ledgers = store.partitions.ledgers.list(stored.id)
            self.assertTrue(any(entry.get("work_item_id") == "worker" for entry in ledgers))
            self.assertTrue(any(entry.get("ledger_kind") == "trace_span" for entry in ledgers))

            loaded = store.get(stored.id)
            self.assertEqual(loaded.result.status, "completed")
            self.assertEqual([event.type for event in loaded.result.events], ["run.started", "node.started", "run.completed"])

            without_events = store.get(stored.id, include_events=False)
            self.assertEqual(without_events.result.events, [])

            event_page = store.get_events(stored.id, cursor=1, limit=1)
            self.assertEqual(event_page["cursor"], 1)
            self.assertEqual(event_page["next_cursor"], 2)
            self.assertTrue(event_page["has_more"])
            self.assertEqual(event_page["events"][0]["type"], "node.started")

            (run_dir / "result.json").write_text("not valid json", encoding="utf-8")
            listed = store.list()
            self.assertEqual(listed[0]["id"], stored.id)
            self.assertEqual(listed[0]["events"], 3)

    def test_legacy_single_file_runs_still_load(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / ".coder"
            store = RunStore(root)
            legacy = StoredRun(
                id="legacy-run",
                workflow_id="workflow-legacy",
                repo_root="/repo",
                request="legacy work",
                result=_result(),
            )
            (root / "runs" / "legacy-run.json").write_text(legacy.model_dump_json(indent=2), encoding="utf-8")

            loaded = store.get("legacy-run")
            self.assertEqual(loaded.workflow_id, "workflow-legacy")
            self.assertEqual(len(loaded.result.events), 3)

            without_events = store.get("legacy-run", include_events=False)
            self.assertEqual(without_events.result.events, [])

            event_page = store.get_events("legacy-run", cursor=0, limit=2)
            self.assertEqual([event["type"] for event in event_page["events"]], ["run.started", "node.started"])
            self.assertTrue(event_page["has_more"])

            listed = store.list()
            self.assertEqual(listed[0]["id"], "legacy-run")
            self.assertEqual(listed[0]["events"], 3)

    def test_context_packets_are_externalized_from_event_log(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / ".coder"
            store = RunStore(root)
            packet = {
                "task": "inspect context",
                "agent": {"id": "worker", "name": "Worker"},
                "node_id": "agent",
                "selected_state_keys": ["review"],
                "state_summaries": {"review": "needs changes"},
                "selected_state": {"review": {"status": "needs_changes"}},
                "allowed_tools": ["project_index"],
                "token_estimate": {"packet": 123, "budget": 5000},
            }
            result = RunResult(
                status="completed",
                data={},
                summaries={},
                events=[
                    RunEvent(type="run.started", message="started"),
                    RunEvent(type="agent.context_packet", node_id="agent", message="context", payload={"packet": packet}),
                    RunEvent(type="run.completed", message="completed"),
                ],
                estimated_tokens_used=123,
                agent_calls=1,
                tool_calls=0,
            )

            stored = store.save("workflow-1", "/repo", "inspect", result)
            event_page = store.get_events(stored.id)
            context_event = event_page["events"][1]
            packet_id = context_event["payload"]["packet_id"]

            self.assertEqual(context_event["type"], "agent.context_packet")
            self.assertNotIn("packet", context_event["payload"])
            self.assertEqual(context_event["payload"]["summary"]["agent_id"], "worker")
            self.assertEqual(context_event["payload"]["summary"]["selected_state_keys"], ["review"])
            self.assertGreater(context_event["payload"]["size_chars"], 0)

            packet_path = root / "runs" / stored.id / "contexts" / f"{packet_id}.json"
            self.assertTrue(packet_path.exists())
            self.assertEqual(json.loads(packet_path.read_text(encoding="utf-8")), packet)
            self.assertEqual(store.get_context_packet(stored.id, packet_id), packet)

    def test_legacy_embedded_context_packets_still_load(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / ".coder"
            store = RunStore(root)
            packet = {"task": "legacy context", "agent": {"id": "worker"}}
            event = RunEvent(type="agent.context_packet", message="context", payload={"packet": packet})
            legacy = StoredRun(
                id="legacy-context-run",
                workflow_id="workflow-legacy",
                repo_root="/repo",
                request="legacy context",
                result=RunResult(
                    status="completed",
                    data={},
                    summaries={},
                    events=[event],
                    estimated_tokens_used=1,
                    agent_calls=1,
                    tool_calls=0,
                ),
            )
            (root / "runs" / "legacy-context-run.json").write_text(legacy.model_dump_json(indent=2), encoding="utf-8")

            self.assertEqual(store.get_context_packet("legacy-context-run", event.id), packet)

    def test_unsafe_context_packet_ids_fall_back_to_event_id(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / ".coder"
            store = RunStore(root)
            packet = {"task": "unsafe id"}
            event = RunEvent(
                type="agent.context_packet",
                message="context",
                payload={"packet_id": "context/unsafe", "packet": packet},
            )
            result = RunResult(
                status="completed",
                data={},
                summaries={},
                events=[event],
                estimated_tokens_used=1,
                agent_calls=1,
                tool_calls=0,
            )

            stored = store.save("workflow-1", "/repo", "inspect", result)
            event_page = store.get_events(stored.id)
            packet_id = event_page["events"][0]["payload"]["packet_id"]

            self.assertEqual(packet_id, event.id)
            self.assertEqual(store.get_context_packet(stored.id, packet_id), packet)


def _result() -> RunResult:
    return RunResult(
        status="completed",
        data={
            "answer": "done",
            "token_ledger": [{"ledger_id": "token_1", "work_item_id": "worker", "estimated_input_tokens": 12}],
            "trace_spans": [{"span_id": "span_1", "name": "run", "kind": "run"}],
        },
        summaries={"answer": "done"},
        artifacts={
            "execution_result_1": {
                "artifact_type": "execution_result",
                "status": "completed",
                "summary": "done",
            }
        },
        events=[
            RunEvent(type="run.started", message="started"),
            RunEvent(type="node.started", node_id="start", message="start"),
            RunEvent(type="run.completed", message="completed"),
        ],
        estimated_tokens_used=12,
        agent_calls=1,
        tool_calls=2,
    )


if __name__ == "__main__":
    unittest.main()
