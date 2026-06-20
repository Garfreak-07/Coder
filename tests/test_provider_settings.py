from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from coder_workbench.core import WorkflowSpec
from coder_workbench.core.preflight import validate_workflow_preflight
from coder_workbench.server.settings import ProviderSettingsStore, provider_status, workflow_provider_status


class ProviderSettingsTests(unittest.TestCase):
    def test_settings_response_does_not_return_secret_values(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            store = ProviderSettingsStore(Path(tmp) / ".coder")
            store.save({"api_keys": {"openai": "sk-secret"}, "mock_mode": False})

            response = store.response()

            self.assertNotIn("sk-secret", str(response))
            self.assertTrue(response["api_keys"]["openai"]["configured"])
            self.assertEqual(response["api_keys"]["openai"]["source"], "settings")

    def test_provider_status_prefers_environment_credentials(self) -> None:
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, {"OPENAI_API_KEY": "env-secret"}, clear=False):
            settings = ProviderSettingsStore(Path(tmp) / ".coder").load()

            status = provider_status(settings, ["openai"])

            provider = status["providers"][0]
            self.assertTrue(provider["credential_configured"])
            self.assertEqual(provider["credential_source"], "environment")
            self.assertNotIn("env-secret", str(status))

    def test_preflight_warns_when_provider_credentials_missing_without_mock_mode(self) -> None:
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, {}, clear=True):
            store = ProviderSettingsStore(Path(tmp) / ".coder")
            settings = store.save({"default_provider": "openai", "mock_mode": False})
            workflow = WorkflowSpec.model_validate(
                {
                    "id": "provider-preflight",
                    "name": "Provider preflight",
                    "agents": [{"id": "worker", "role": "Worker", "goal": "Work"}],
                    "nodes": [
                        {"id": "start", "type": "start"},
                        {"id": "agent", "type": "agent", "agent_id": "worker"},
                        {"id": "end", "type": "end"},
                    ],
                    "edges": [
                        {"from": "start", "to": "agent"},
                        {"from": "agent", "to": "end"},
                    ],
                }
            )

            result = validate_workflow_preflight(workflow, provider_status=workflow_provider_status(settings, workflow))

            self.assertEqual(result["status"], "warning")
            self.assertIn("provider_status", result["summary"])
            self.assertIn("provider_credentials_missing", {issue["code"] for issue in result["issues"]})


if __name__ == "__main__":
    unittest.main()
