from __future__ import annotations

import tomllib
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]


class PythonCiDependencyContractTests(unittest.TestCase):
    def test_openhands_extra_declares_sdk_and_tool_packages(self) -> None:
        pyproject = tomllib.loads((REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8"))

        openhands_extra = pyproject["project"]["optional-dependencies"]["openhands"]

        self.assertIn("openhands-sdk>=1.29.2", openhands_extra)
        self.assertIn("openhands-tools>=1.29.2", openhands_extra)

    def test_python_ci_installs_openhands_extra_before_unittest_discovery(self) -> None:
        ci_workflow = (REPO_ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")

        install_index = ci_workflow.index('python -m pip install -e ".[openhands]"')
        test_index = ci_workflow.index("python -m unittest discover -s tests")

        self.assertLess(install_index, test_index)


if __name__ == "__main__":
    unittest.main()
