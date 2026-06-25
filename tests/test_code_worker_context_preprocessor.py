from __future__ import annotations

import json
import unittest

from coder_workbench.agent_graph.schema import AgentTaskEnvelope, WorkItem
from coder_workbench.agent_harness import (
    CodeWorkerContextBudget,
    CodeWorkerContextPreprocessor,
    CodeWorkerLoopState,
    HarnessObservation,
    HarnessSession,
)


class CodeWorkerContextPreprocessorTests(unittest.TestCase):
    def test_prompt_contains_recent_observations_and_output_refs(self) -> None:
        state = _state()
        state.session.observations.append(
            HarnessObservation(
                action_id="read-1",
                action_type="read_file",
                status="ok",
                summary="Read file.",
                output_ref="sha256:abc",
                evidence_refs=["sha256:abc"],
            )
        )
        state.session.evidence_refs.append("sha256:abc")

        prepared = CodeWorkerContextPreprocessor().prepare(item=_item(), envelope=_envelope(), state=state)

        hot = prepared.prompt_payload["hot_context"]
        cold = prepared.prompt_payload["cold_context"]
        self.assertEqual(hot["recent_observations"][0]["action_id"], "read-1")
        self.assertIn("sha256:abc", cold["output_refs"])

    def test_old_observations_are_summarized(self) -> None:
        state = _state()
        for index in range(12):
            state.session.observations.append(
                HarnessObservation(
                    action_id=f"obs-{index}",
                    action_type="search_files",
                    status="ok",
                    summary=f"Observation {index}.",
                )
            )

        prepared = CodeWorkerContextPreprocessor(
            CodeWorkerContextBudget(max_recent_observations=4)
        ).prepare(item=_item(), envelope=_envelope(), state=state)

        self.assertTrue(prepared.compacted)
        self.assertEqual(prepared.omitted_counts["observations"], 8)
        self.assertEqual(len(prepared.prompt_payload["hot_context"]["recent_observations"]), 4)
        self.assertTrue(prepared.prompt_payload["warm_context"]["older_observation_summary"])

    def test_prompt_omits_full_large_output(self) -> None:
        state = _state()
        large = "x" * 5000
        state.session.observations.append(
            HarnessObservation(
                action_id="cmd",
                action_type="run_command_sandbox",
                status="ok",
                summary=large,
                payload_preview={"output": large},
            )
        )

        prepared = CodeWorkerContextPreprocessor(
            CodeWorkerContextBudget(max_observation_chars=100)
        ).prepare(item=_item(), envelope=_envelope(), state=state)
        prompt_json = json.dumps(prepared.prompt_payload)

        self.assertTrue(prepared.compacted)
        self.assertNotIn(large, prompt_json)
        self.assertIn("truncated", prompt_json)

    def test_prompt_payload_stays_under_budget(self) -> None:
        state = _state()
        for index in range(20):
            state.session.observations.append(
                HarnessObservation(
                    action_id=f"obs-{index}",
                    action_type="read_file",
                    status="ok",
                    summary="Read file.",
                    payload_preview={"content": "x" * 1000},
                )
            )

        prepared = CodeWorkerContextPreprocessor(
            CodeWorkerContextBudget(max_recent_observations=12, max_total_context_chars=4000)
        ).prepare(item=_item(), envelope=_envelope(), state=state)

        self.assertLessEqual(len(json.dumps(prepared.prompt_payload)), 4000)
        self.assertTrue(prepared.compacted)


def _state() -> CodeWorkerLoopState:
    return CodeWorkerLoopState(
        session=HarnessSession(
            run_id="run",
            round=1,
            work_item_id="executor-work",
            agent_id="executor",
            merge_index=1,
            task_summary="Fix src/app.py.",
        )
    )


def _item() -> WorkItem:
    return WorkItem(
        work_item_id="executor-work",
        merge_index=1,
        assignee_agent_id="executor",
        task_summary="Fix src/app.py.",
    )


def _envelope() -> AgentTaskEnvelope:
    return AgentTaskEnvelope(
        round=1,
        work_item_id="executor-work",
        merge_index=1,
        assigned_agent_id="executor",
        task_summary="Fix src/app.py.",
        planner_order_ref="planner_order_round_1",
    )


if __name__ == "__main__":
    unittest.main()
