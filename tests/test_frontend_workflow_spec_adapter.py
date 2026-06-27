from __future__ import annotations

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FRONTEND = ROOT / "frontend" / "src"
ADAPTER = FRONTEND / "workflowSpecAdapter.ts"
APP = FRONTEND / "App.tsx"
TYPES = FRONTEND / "types.ts"


class FrontendWorkflowSpecAdapterTests(unittest.TestCase):
    def test_adapter_exposes_phase_8_compatibility_functions(self) -> None:
        source = ADAPTER.read_text(encoding="utf-8")

        for token in [
            "legacyCanvasToWorkflowSpec",
            "workflowSpecToLegacyCanvas",
            "validateWorkflowSpec",
            "parseWorkflowImport",
            "legacyCanvasToWorkflowExport",
        ]:
            with self.subTest(token=token):
                self.assertIn(f"function {token}", source)

    def test_versioned_export_is_a_rust_project_config_envelope(self) -> None:
        adapter = ADAPTER.read_text(encoding="utf-8")
        types = TYPES.read_text(encoding="utf-8")

        self.assertIn('kind: "coder.workflow"', types)
        self.assertIn('kind: "coder.workflow"', adapter)
        self.assertIn("workflows:", adapter)
        self.assertIn("legacy_agent_workflow", adapter)

    def test_app_import_export_uses_adapter_without_dropping_legacy_json(self) -> None:
        source = APP.read_text(encoding="utf-8")

        self.assertIn("legacyCanvasToWorkflowExport", source)
        self.assertIn("parseWorkflowImport(raw)", source)
        self.assertIn(".coder-workflow.json", source)
        self.assertIn("legacyCanvasToWorkflowSpec(imported)", source)


if __name__ == "__main__":
    unittest.main()
