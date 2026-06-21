from __future__ import annotations

import sys
import tempfile
import unittest

from coder_workbench.coding.command_service import CommandService


class CommandPolicyTests(unittest.TestCase):
    def test_command_service_runs_argv_without_shell(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = CommandService(tmp, data={"preapprove_all": True})
            result = service.run_check(
                argv=[sys.executable, "-c", "print('ok')"],
                command="",
                shell=False,
                require_approval=False,
                source="discovered",
            )

        self.assertTrue(result["passed"])
        self.assertIn("ok", result["output"])
        self.assertEqual(result["policy"]["risk"], "low")

    def test_shell_command_requires_approval_outside_sandbox(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = CommandService(tmp)
            result = service.run_check(
                command="echo ok && echo done",
                shell=True,
                require_approval=False,
                source="model",
                sandbox=False,
            )

        self.assertTrue(result["blocked"])
        self.assertTrue(result["requires_approval"])
        self.assertEqual(result["policy"]["risk"], "medium")

    def test_model_generated_command_requires_approval_outside_sandbox(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = CommandService(tmp)
            result = service.run_check(
                command=sys.executable,
                shell=False,
                require_approval=False,
                source="model",
                sandbox=False,
            )

        self.assertTrue(result["blocked"])
        self.assertTrue(result["requires_approval"])
        self.assertEqual(result["policy"]["risk"], "medium")

    def test_sandbox_command_can_run_without_extra_approval(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = CommandService(tmp)
            result = service.run_check(
                argv=[sys.executable, "-c", "print('sandbox-ok')"],
                command="",
                shell=False,
                require_approval=False,
                source="model",
                sandbox=True,
            )

        self.assertTrue(result["passed"])
        self.assertIn("sandbox-ok", result["output"])

    def test_path_escape_still_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            service = CommandService(tmp)
            with self.assertRaises(ValueError):
                service.run_check(
                    argv=[sys.executable, "-c", "print('nope')"],
                    command="",
                    cwd="..",
                    shell=False,
                    require_approval=False,
                    source="discovered",
                )


if __name__ == "__main__":
    unittest.main()
