from __future__ import annotations

import tempfile
import unittest

from coder_workbench.core import WorkflowSpec
from coder_workbench.runtime import run_workflow


class McpToolTests(unittest.TestCase):
    def test_mcp_tool_node_blocks_for_specific_approval(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workflow = WorkflowSpec.model_validate(
                {
                    "id": "mcp-test",
                    "name": "MCP test",
                    "nodes": [
                        {"id": "start", "type": "start"},
                        {
                            "id": "mcp",
                            "type": "mcp_tool",
                            "tool": "echo",
                            "input": {"server_command": "fake-mcp-server"},
                            "output_key": "mcp_result",
                        },
                        {"id": "end", "type": "end"},
                    ],
                    "edges": [
                        {"from": "start", "to": "mcp"},
                        {"from": "mcp", "to": "end"},
                    ],
                }
            )

            result = run_workflow(workflow, "call mcp", tmp, initial_data={"scopes": []})

            self.assertEqual(result.status, "blocked")
            self.assertEqual(result.blocked_node_id, "mcp")
            self.assertEqual(result.data["mcp_result"]["approval_type"], "mcp_tool")
            self.assertTrue(result.data["mcp_result"]["approval_key"].startswith("mcp:"))


if __name__ == "__main__":
    unittest.main()
