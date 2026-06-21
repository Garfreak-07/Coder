from __future__ import annotations

import unittest

from coder_workbench.agent_engine import default_agent_engine_registry
from coder_workbench.agent_model import AgentRecipe, RuntimeProfileCompiler


class RuntimeProfileClosureTests(unittest.TestCase):
    def test_all_recipe_roles_compile_to_registered_default_engines(self) -> None:
        registry = default_agent_engine_registry()
        compiler = RuntimeProfileCompiler()

        for role in ["planner", "do_work", "check_result", "organize", "research", "write_draft"]:
            profile = compiler.compile(
                AgentRecipe(id=f"{role}-agent", name=f"{role} Agent", role=role)
            )
            self.assertIn(profile.engine_id, registry.ids(), role)

    def test_knowledge_roles_use_synthesis_artifacts(self) -> None:
        compiler = RuntimeProfileCompiler()

        for role in ["organize", "research", "write_draft"]:
            profile = compiler.compile(AgentRecipe(id=role, name=role, role=role))
            self.assertEqual(profile.engine_id, "synthesizer-engine")
            self.assertIn("synthesis_artifact", profile.allowed_artifacts)


if __name__ == "__main__":
    unittest.main()
