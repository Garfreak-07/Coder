from __future__ import annotations

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class ProductUISurfaceTests(unittest.TestCase):
    def test_workbench_app_does_not_expose_legacy_runtime_surface(self) -> None:
        source = (ROOT / "frontend" / "src" / "App.tsx").read_text(encoding="utf-8")

        for token in [
            "legacy runtime",
            "runtimeJsonText",
            "showAdvancedRuntime",
            "compileLegacyRuntimePreview",
            "Compile Runtime Profiles",
            "AgentWorkflowSpec JSON",
            "startLiveRun",
            "getLiveRun",
            "saveWorkflow",
            "validateWorkflow",
        ]:
            with self.subTest(token=token):
                self.assertNotIn(token, source)

    def test_workbench_app_centers_planner_chat_and_run_evidence(self) -> None:
        sources = "\n".join(
            [
                (ROOT / "frontend" / "src" / "App.tsx").read_text(encoding="utf-8"),
                (ROOT / "frontend" / "src" / "components" / "AppSidebar.tsx").read_text(encoding="utf-8"),
                (ROOT / "frontend" / "src" / "features" / "planner-chat" / "PlannerChatPage.tsx").read_text(
                    encoding="utf-8"
                ),
            ]
        )

        for token in [
            "Planner Chat",
            "Message the Planner",
            "Planner strength",
            "Run settings",
            "Advanced debug: event log",
        ]:
            with self.subTest(token=token):
                self.assertIn(token, sources)

        self.assertNotIn("submitPlannerResponse", sources)

    def test_app_uses_left_sidebar_not_top_navigation(self) -> None:
        app = (ROOT / "frontend" / "src" / "App.tsx").read_text(encoding="utf-8")
        sidebar = (ROOT / "frontend" / "src" / "components" / "AppSidebar.tsx").read_text(encoding="utf-8")

        self.assertIn("<AppSidebar", app)
        self.assertIn('<nav className="side-nav"', sidebar)
        self.assertNotIn("top-nav", app)
        self.assertNotIn("Workbench", sidebar)
        self.assertNotIn('activeSection === "runs"', app)
        self.assertNotIn(">Runs<", sidebar)

    def test_visible_frontend_copy_does_not_reference_legacy_runtime(self) -> None:
        for relative_path in [
            Path("frontend/src/App.tsx"),
            Path("frontend/src/i18n.ts"),
        ]:
            source = (ROOT / relative_path).read_text(encoding="utf-8").lower()
            with self.subTest(path=str(relative_path)):
                self.assertNotIn("legacy runtime", source)


if __name__ == "__main__":
    unittest.main()
