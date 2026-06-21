from __future__ import annotations

import tempfile
import unittest

from coder_workbench.runtime import RunEvent
from coder_workbench.server.stores import PartitionedRunStores


class PartitionedStoreTests(unittest.TestCase):
    def test_partitioned_store_facade_reads_and_writes_run_objects(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            stores = PartitionedRunStores(tmp)
            event = RunEvent(type="run.started", message="started")

            stores.events.append("run-1", event)
            stores.artifacts.write("run-1", "artifact_1", {"artifact_type": "sample", "value": 1})
            stores.ledgers.write("run-1", "ledger_1", {"kind": "token", "tokens": 12})
            blob_id = stores.blobs.write_text("large output")

            self.assertEqual(stores.events.read("run-1")[0].type, "run.started")
            self.assertEqual(stores.artifacts.read("run-1", "artifact_1")["value"], 1)
            self.assertEqual(stores.ledgers.read("run-1", "ledger_1")["tokens"], 12)
            self.assertEqual(stores.ledgers.list("run-1")[0]["kind"], "token")
            self.assertEqual(stores.blobs.read_text(blob_id), "large output")
            self.assertTrue(stores.extensions.plugins_dir.exists())
            self.assertTrue(stores.extensions.skills_dir.exists())
            self.assertTrue(stores.cache.namespace("repo-index").exists())


if __name__ == "__main__":
    unittest.main()
