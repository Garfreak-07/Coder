from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FRONTEND = ROOT / "frontend" / "src"
APP = FRONTEND / "App.tsx"
APP_SIDEBAR = FRONTEND / "components" / "AppSidebar.tsx"
MAIN = FRONTEND / "main.tsx"
PLANNER_CHAT_PAGE = FRONTEND / "features" / "planner-chat" / "PlannerChatPage.tsx"
AGENT_WORKFLOW_PAGE = FRONTEND / "features" / "agent-workflow" / "AgentWorkflowPage.tsx"
WORKFLOW_SELECTOR = FRONTEND / "features" / "agent-workflow" / "WorkflowSelector.tsx"
WORKFLOW_STRUCTURE_PANEL = FRONTEND / "features" / "agent-workflow" / "WorkflowStructurePanel.tsx"
STYLES = FRONTEND / "styles.css"

PRODUCT_SURFACES = [
    APP,
    APP_SIDEBAR,
    PLANNER_CHAT_PAGE,
    AGENT_WORKFLOW_PAGE,
    WORKFLOW_SELECTOR,
    WORKFLOW_STRUCTURE_PANEL,
]


class WorkbenchUiSimplifiedModelTests(unittest.TestCase):
    def test_default_frontend_workflow_uses_product_role_names(self) -> None:
        source = (FRONTEND / "examples.ts").read_text(encoding="utf-8")

        self.assertIn('name: "Planner"', source)
        self.assertIn('name: "Executor"', source)
        self.assertIn('role_card: "executor"', source)
        self.assertNotIn("Planner Agent", source)
        self.assertNotIn("Executor Agent", source)
        self.assertNotIn("Tester Agent", source)
        self.assertNotIn('name: "Tester"', source)
        self.assertNotIn('role_card: "tester"', source)

    def test_canvas_role_labels_are_only_planner_executor(self) -> None:
        source = (FRONTEND / "workflowGraph.ts").read_text(encoding="utf-8")

        self.assertIn('planner: "Planner"', source)
        self.assertIn('executor: "Executor"', source)
        self.assertNotIn('tester: "Tester"', source)
        self.assertNotIn("can ask user", source)

    def test_removed_workbench_components_are_not_referenced(self) -> None:
        source = APP.read_text(encoding="utf-8")

        for token in [
            "AgentWorkflowAgentInspector",
            "AgentWorkflowEdgeInspector",
            "agentWorkflowTemplateCards",
            "instantiateAgentWorkflowTemplate",
            "jsonText",
            "applyJson",
            "newAgentName",
            "setApproved",
        ]:
            with self.subTest(token=token):
                self.assertNotIn(token, source)

        self.assertFalse((FRONTEND / "template.ts").exists())
        self.assertFalse((FRONTEND / "features" / "agent-workflow" / "AgentWorkflowAgentInspector.tsx").exists())
        self.assertFalse((FRONTEND / "features" / "agent-workflow" / "AgentWorkflowEdgeInspector.tsx").exists())

    def test_split_app_sections_are_declared_and_chat_is_default(self) -> None:
        sidebar = APP_SIDEBAR.read_text(encoding="utf-8")
        app = APP.read_text(encoding="utf-8")

        self.assertIn('["chat", "workflow", "extensions", "settings"]', sidebar)
        self.assertNotIn('"runs"', sidebar)
        self.assertNotIn('runs:', sidebar)
        for label in ["Planner Chat", "Agent Workflow", "Extensions", "Settings"]:
            with self.subTest(label=label):
                self.assertIn(label, sidebar)

        self.assertIn('useState<AppSection>("chat")', app)
        self.assertIn("<PlannerChatPage", app)
        self.assertIn("<AgentWorkflowPage", app)

    def test_planner_delete_is_disabled_in_workflow_structure(self) -> None:
        app = APP.read_text(encoding="utf-8")
        panel = WORKFLOW_STRUCTURE_PANEL.read_text(encoding="utf-8")

        self.assertRegex(panel, re.compile(r"agent\.id !== workflow\.primary_planner_id"))
        self.assertIn("Primary Planner cannot be deleted.", app)
        self.assertNotIn("Runtime profiles are compiled internally", app)
        self.assertNotIn("capabilities={", app)

    def test_removed_words_are_absent_from_split_product_surfaces(self) -> None:
        forbidden = [
            "Workflow Library",
            "Start From Template",
            "System Status",
            "Agent Inspector",
            "Agent Topology",
            "Purpose",
            "Workflow ID",
            "Description",
            "Apply JSON",
            "This edge loops back to the Planner",
            "handoff inferred",
            "Worker",
            "Do work",
            "Custom",
            "Engine",
            "Harness",
            "Runtime Role",
            "raw capabilities",
            "legacy runtime preview",
            "Only Planner can ask the user",
            "Executors return execution results",
            "Testers return evidence",
            "Executor and Tester",
            "Planner reviews every round",
        ]
        offenders: list[str] = []
        for path in PRODUCT_SURFACES:
            source = path.read_text(encoding="utf-8")
            for word in forbidden:
                if word in source:
                    offenders.append(f"{path.relative_to(ROOT)} contains {word!r}")

        self.assertEqual(offenders, [])

    def test_agent_workflow_page_toolbar_minimap_and_controls(self) -> None:
        source = AGENT_WORKFLOW_PAGE.read_text(encoding="utf-8")

        for token in ["Save", "Save As", "Import", "Export"]:
            with self.subTest(token=token):
                self.assertIn(token, source)

        self.assertIn('className="workflow-minimap"', source)
        self.assertIn('className="workflow-flow"', source)
        self.assertIn("workflow-flow-shell", source)
        self.assertIn('position="top-left"', source)
        self.assertIn("width: 120", source)
        self.assertIn("height: 80", source)
        self.assertNotRegex(source, re.compile(r"\bControls\b"))

    def test_agent_workflow_canvas_has_stable_viewport_styles(self) -> None:
        styles = STYLES.read_text(encoding="utf-8")
        main = MAIN.read_text(encoding="utf-8")

        self.assertIn('import "@xyflow/react/dist/style.css";', main)
        self.assertIn(".workflow-canvas-panel .react-flow", styles)
        self.assertIn(".workflow-flow-shell", styles)
        self.assertIn("min-height: 620px", styles)
        self.assertIn("height: min(72vh, 760px)", styles)
        self.assertIn(".agent-workflow-node", styles)
        self.assertIn(".agent-role-planner", styles)
        self.assertIn(".agent-role-executor", styles)
        self.assertNotIn(".agent-role-tester", styles)
        self.assertIn(".workflow-minimap svg", styles)

    def test_workflow_selector_uses_saved_agent_workflows(self) -> None:
        workflow_page = AGENT_WORKFLOW_PAGE.read_text(encoding="utf-8")
        selector = WORKFLOW_SELECTOR.read_text(encoding="utf-8")

        self.assertIn("library.agent_workflows", workflow_page)
        self.assertIn("AgentWorkflowSummary", selector)
        self.assertIn("<select", selector)
        self.assertIn("agents /", selector)
        self.assertNotIn("Refresh", selector)

    def test_planner_strength_selector_lives_in_chat_composer(self) -> None:
        chat = PLANNER_CHAT_PAGE.read_text(encoding="utf-8")
        workflow = AGENT_WORKFLOW_PAGE.read_text(encoding="utf-8")

        self.assertIn("Planner strength", chat)
        self.assertIn("composer-footer", chat)
        self.assertNotIn("Planner strength", workflow)
        self.assertNotIn("plannerStrength", workflow)

    def test_save_as_and_import_collisions_create_new_workflow_ids(self) -> None:
        app = APP.read_text(encoding="utf-8")

        self.assertIn("persistWorkflowAsCopy", app)
        self.assertIn("uniqueWorkflowId", app)
        self.assertIn("Date.now()", app)
        self.assertIn("Saved new Agent workflow", app)
        self.assertIn("idExists", app)
        self.assertIn("Imported as new Agent workflow", app)
        self.assertRegex(app, re.compile(r"id:\s*idExists\s*\?\s*`\$\{rawId\}-\$\{Date\.now\(\)\}`"))

    def test_workflow_structure_panel_hides_edge_advanced_options(self) -> None:
        panel = WORKFLOW_STRUCTURE_PANEL.read_text(encoding="utf-8")

        self.assertIn("Agent type", panel)
        self.assertIn("Add Connection", panel)
        self.assertIn("Connections", panel)
        self.assertNotIn("Edge label", panel)
        self.assertNotIn("loop checkbox", panel)
        self.assertNotIn("handoff inferred", panel)


if __name__ == "__main__":
    unittest.main()
