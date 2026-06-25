from __future__ import annotations

import unittest

from coder_workbench.agent_harness.tool_gate import ALLOWED_CODE_WORKER_ACTIONS
from coder_workbench.agent_harness.tool_metadata import ToolMetadataRegistry


class CodeWorkerToolMetadataTests(unittest.TestCase):
    def test_metadata_exists_for_every_allowed_action(self) -> None:
        registry = ToolMetadataRegistry()

        self.assertEqual(ALLOWED_CODE_WORKER_ACTIONS, registry.names())

    def test_unknown_metadata_fails_closed(self) -> None:
        registry = ToolMetadataRegistry([])

        with self.assertRaises(KeyError):
            registry.require("read_file")

    def test_metadata_marks_read_tools_concurrency_safe(self) -> None:
        registry = ToolMetadataRegistry()

        for name in ("read_file", "search_files", "inspect_git_diff", "read_tool_output"):
            metadata = registry.require(name)
            self.assertTrue(metadata.is_read_only)
            self.assertTrue(metadata.is_concurrency_safe)

    def test_metadata_marks_patch_and_command_exclusive(self) -> None:
        registry = ToolMetadataRegistry()

        for name in ("propose_patch", "apply_patch_sandbox", "run_command_sandbox", "return_execution_result"):
            metadata = registry.require(name)
            self.assertFalse(metadata.is_concurrency_safe)


if __name__ == "__main__":
    unittest.main()
