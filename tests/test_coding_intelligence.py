from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

from coder_workbench.agent_graph.schema import AgentTaskEnvelope
from coder_workbench.coding import (
    CodingContextBuilder,
    build_repo_index,
    build_repo_intelligence,
    build_risk_map,
    build_run_coding_eval,
    build_symbol_index,
    discover_commands,
    evaluate_fake_coding_task,
    load_coding_task,
)
from coder_workbench.coding.sandbox import sandbox_apply_and_check


class CodingIntelligenceTests(unittest.TestCase):
    def test_repo_index_detects_current_python_and_frontend_stack(self) -> None:
        root = Path(__file__).resolve().parents[1]

        repo_index = build_repo_index(root)
        commands = discover_commands(root)

        self.assertIn("python", repo_index.languages)
        self.assertIn("typescript", repo_index.languages)
        self.assertIn("fastapi", repo_index.frameworks)
        self.assertIn("react", repo_index.frameworks)
        self.assertIn("vite", repo_index.frameworks)
        self.assertIn("src", repo_index.source_dirs)
        self.assertIn("frontend/src", repo_index.source_dirs)
        self.assertIn("tests", repo_index.test_dirs)
        self.assertIn("pyproject.toml", repo_index.important_files)
        self.assertIn("frontend/package.json", repo_index.important_files)
        self.assertIn(".env", repo_index.risk_files)
        self.assertTrue(any(command.command == "python -m unittest discover -s tests" for command in commands.test_commands))
        self.assertTrue(any(command.command == "npm run build" and command.cwd == "frontend" for command in commands.build_commands))

    def test_symbol_index_extracts_python_classes_with_regex_fallback(self) -> None:
        root = Path(__file__).resolve().parents[1]

        symbol_index = build_symbol_index(root, paths=["src/coder_workbench/agent_graph/runner.py"])

        runner_file = symbol_index.files[0]
        self.assertEqual(runner_file.path, "src/coder_workbench/agent_graph/runner.py")
        self.assertTrue(any(symbol.name == "AgentGraphRunner" and symbol.kind == "class" for symbol in runner_file.symbols))

    def test_context_builder_selects_relevant_files_without_whole_repo(self) -> None:
        root = Path(__file__).resolve().parents[1]
        intelligence = build_repo_intelligence(root, max_symbol_files=80)
        envelope = AgentTaskEnvelope(
            round=1,
            work_item_id="fix-runner",
            merge_index=1,
            assigned_agent_id="executor",
            task_summary="Modify AgentGraphRunner in src/coder_workbench/agent_graph/runner.py.",
            planner_order_ref="planner_order_round_1",
        )

        packet = CodingContextBuilder(root).build(
            envelope=envelope,
            repo_index=intelligence["repo_index"],
            symbol_index=intelligence["symbol_index"],
            command_discovery=intelligence["command_discovery"],
            risk_map=intelligence["risk_map"],
            token_budget=2000,
        )

        self.assertIn("src/coder_workbench/agent_graph/runner.py", packet.included_files)
        self.assertLess(len(packet.included_files), 10)
        self.assertTrue(packet.selection_reason)

    def test_sandbox_apply_and_check_does_not_mutate_repo(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            target = repo / "sample.py"
            target.write_text("value = 1\n", encoding="utf-8")
            result = sandbox_apply_and_check(
                repo,
                [{"path": "sample.py", "action": "update", "expected_before": "value = 1\n", "content": "value = 2\n"}],
                [{"command": f'"{sys.executable}" -m py_compile sample.py', "cwd": "."}],
            )

            self.assertEqual(result["status"], "pass")
            self.assertEqual(target.read_text(encoding="utf-8"), "value = 1\n")

    def test_eval_fixture_outputs_report(self) -> None:
        root = Path(__file__).resolve().parents[1]
        task = load_coding_task(root / "tests" / "fixtures" / "coding_tasks" / "python_bugfix_001.json")

        report = evaluate_fake_coding_task(task)

        self.assertEqual(report.task_id, "python_bugfix_001")
        self.assertEqual(report.task_pass_rate, 1.0)
        self.assertEqual(report.tests_pass_rate, 1.0)

    def test_run_coding_eval_summarizes_graph_cache(self) -> None:
        data = {
            "graph_run_cache": {
                "round": 1,
                "execution_cache": {
                    "worker": {"status": "completed"},
                },
                "test_cache": {
                    "worker": [{"status": "pass"}],
                },
                "hidden_effects": [
                    {"effect_type": "modify_files", "status": "patch_preview_created"},
                ],
                "interrupts": [],
            },
            "rounds": [{"round": 1}],
            "token_ledger": [{"estimated_input_tokens": 10}],
        }

        report = build_run_coding_eval(data)

        self.assertTrue(report["details"]["patch_created"])
        self.assertEqual(report["tests_pass_rate"], 1.0)


class RiskMapTests(unittest.TestCase):
    def test_risk_map_flags_secret_and_repo_paths(self) -> None:
        risk_map = build_risk_map(".")

        self.assertIn(".env", risk_map.risk_files)
        self.assertIn(".git", risk_map.risk_files)


if __name__ == "__main__":
    unittest.main()
