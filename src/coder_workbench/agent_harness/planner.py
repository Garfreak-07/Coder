from __future__ import annotations

from typing import Any

from coder_workbench.agent_graph.schema import PlannerOrder

from .base import AgentHarness
from .policies import planner_policy


class PlannerHarness(AgentHarness):
    def __init__(self) -> None:
        super().__init__(policy=planner_policy())

    def repo_intelligence_context(self, repo_intelligence: dict[str, Any] | None) -> dict[str, Any]:
        if not repo_intelligence:
            return {}
        repo_index = repo_intelligence.get("repo_index") if isinstance(repo_intelligence.get("repo_index"), dict) else {}
        command_discovery = (
            repo_intelligence.get("command_discovery")
            if isinstance(repo_intelligence.get("command_discovery"), dict)
            else {}
        )
        symbol_index = repo_intelligence.get("symbol_index") if isinstance(repo_intelligence.get("symbol_index"), dict) else {}
        return {
            "languages": repo_index.get("languages", []),
            "frameworks": repo_index.get("frameworks", []),
            "source_dirs": repo_index.get("source_dirs", []),
            "test_dirs": repo_index.get("test_dirs", []),
            "test_commands": command_discovery.get("test_commands", []),
            "build_commands": command_discovery.get("build_commands", []),
            "symbol_files": len(symbol_index.get("files", [])),
        }

    def validate_order(self, payload: dict[str, Any]) -> PlannerOrder:
        return PlannerOrder.model_validate(payload)
