from __future__ import annotations

import tempfile
import unittest

from coder_workbench.harness_runtime import NativeRuntimeStore
from coder_workbench.server.stores.blobs import BlobStore


class NativeRuntimeStoreTests(unittest.TestCase):
    def test_append_event_stores_payload_by_ref(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store = NativeRuntimeStore(blob_store=BlobStore(tmp))
            event = store.append_event(
                run_id="run-1",
                round=1,
                work_item_id="work-1",
                agent_id="executor",
                provider_id="openhands-sdk",
                harness_id="task-execution-harness",
                mode="task_execution",
                native_type="tool.output",
                status="completed",
                summary="Tool completed.",
                payload={"output": "x" * 2000},
                preview_chars=80,
            )

            self.assertEqual(event.run_id, "run-1")
            self.assertIsNotNone(event.payload_ref)
            self.assertLess(len(event.payload_preview or ""), 120)
            self.assertIn("truncated", event.payload_preview or "")
            self.assertIn('"output"', store.read_payload(event.payload_ref or ""))
            self.assertNotIn("x" * 2000, event.model_dump_json())

    def test_list_events_and_refs_can_filter_by_work_item(self) -> None:
        store = NativeRuntimeStore()
        first = store.append_event(
            run_id="run-1",
            work_item_id="work-1",
            provider_id="internal-fallback",
            harness_id="task-execution-harness",
            mode="task_execution",
            native_type="check",
        )
        second = store.append_event(
            run_id="run-1",
            work_item_id="work-2",
            provider_id="internal-fallback",
            harness_id="task-execution-harness",
            mode="task_execution",
            native_type="check",
        )

        self.assertEqual(store.list_events("run-1", work_item_id="work-1"), [first])
        self.assertEqual(store.refs_for_work_item("run-1", "work-2"), [second.event_id])
        self.assertEqual(store.refs_for_run("run-1"), [first.event_id, second.event_id])


if __name__ == "__main__":
    unittest.main()
