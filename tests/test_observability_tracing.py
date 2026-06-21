from __future__ import annotations

import unittest

from coder_workbench.observability import TraceContext
from coder_workbench.runtime import RunEvent


class TraceSpanTests(unittest.TestCase):
    def test_child_spans_share_trace_id_and_parent(self) -> None:
        trace = TraceContext(trace_id="trace")
        run = trace.start_span(name="run", kind="run")
        child = trace.start_span(name="agent", kind="agent_run", parent=run)

        self.assertEqual(child.trace_id, "trace")
        self.assertEqual(child.parent_span_id, run.span_id)

    def test_run_event_can_carry_trace_payload(self) -> None:
        trace = TraceContext(trace_id="trace")
        span = trace.start_span(name="action", kind="action")

        event = RunEvent(type="agent_task.started", message="started", payload=span.event_payload())

        self.assertEqual(event.payload["trace_id"], "trace")
        self.assertEqual(event.payload["span_id"], span.span_id)


if __name__ == "__main__":
    unittest.main()
