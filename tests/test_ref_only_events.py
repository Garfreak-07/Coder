from __future__ import annotations

import tempfile
import unittest
from typing import Any

from coder_workbench.agent_graph.runner import AgentGraphRunner
from coder_workbench.core import default_planner_led_agent_workflow


BANNED_KEYS = {
    "packet",
    "context_packet",
    "coding_context_packet",
    "full_output",
    "raw_output",
    "graph_run_cache",
    "token_ledger",
    "full_transcript",
}


class RefOnlyEventTests(unittest.TestCase):
    def test_normal_agent_graph_events_are_ref_only(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = AgentGraphRunner(default_planner_led_agent_workflow()).run(
                "Verify compact event payloads.",
                tmp,
            )

        for event in result.events:
            bad_key = _first_banned_key(event.payload)
            self.assertIsNone(bad_key, f"{event.type} contains banned payload key {bad_key}")


def _first_banned_key(value: Any, prefix: str = "") -> str | None:
    if isinstance(value, dict):
        for key, child in value.items():
            key_text = str(key)
            path = f"{prefix}.{key_text}" if prefix else key_text
            if key_text in BANNED_KEYS:
                return path
            found = _first_banned_key(child, path)
            if found:
                return found
    if isinstance(value, list):
        for index, child in enumerate(value):
            found = _first_banned_key(child, f"{prefix}[{index}]")
            if found:
                return found
    return None


if __name__ == "__main__":
    unittest.main()
