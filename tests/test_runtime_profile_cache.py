from __future__ import annotations

import unittest

from coder_workbench.agent_model import RuntimeProfileCache
from coder_workbench.core import AgentWorkflowSpec, default_planner_led_agent_workflow


class RuntimeProfileCacheTests(unittest.TestCase):
    def test_same_workflow_hits_cache(self) -> None:
        cache = RuntimeProfileCache()
        workflow = default_planner_led_agent_workflow()

        first = cache.compile_or_get(workflow)
        second = cache.compile_or_get(workflow)

        self.assertFalse(first.cache_hit)
        self.assertTrue(second.cache_hit)
        self.assertEqual(first.profile_hash, second.profile_hash)
        self.assertEqual([profile.agent_id for profile in first.profiles], [profile.agent_id for profile in second.profiles])

    def test_extension_version_and_agent_role_change_cache_key(self) -> None:
        cache = RuntimeProfileCache()
        workflow = default_planner_led_agent_workflow()
        role_changed_payload = workflow.model_dump(mode="json", by_alias=True)
        role_changed_payload["agents"][1]["role"] = "writer"
        role_changed = AgentWorkflowSpec.model_validate(role_changed_payload)

        base = cache.compile_or_get(workflow, installed_extensions=[{"id": "skill", "version": "1"}])
        extension_changed = cache.compile_or_get(workflow, installed_extensions=[{"id": "skill", "version": "2"}])
        role_changed_result = cache.compile_or_get(role_changed, installed_extensions=[{"id": "skill", "version": "1"}])

        self.assertNotEqual(base.profile_hash, extension_changed.profile_hash)
        self.assertNotEqual(base.profile_hash, role_changed_result.profile_hash)


if __name__ == "__main__":
    unittest.main()
