from __future__ import annotations

from typing import Any

from coder_workbench.agent_engine import AgentEngineRegistry, default_agent_engine_registry
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, ExecutionRecord, WorkItem
from coder_workbench.agent_model import RuntimeProfileCache, RuntimeProfileCompiler, recipe_from_workflow_agent
from coder_workbench.core import AgentWorkflowAgent, AgentWorkflowSpec


class AgentRun:
    """Runs one Agent work item through a compiled runtime profile and AgentEngine."""

    def __init__(
        self,
        agent_workflow: AgentWorkflowSpec,
        *,
        engine_registry: AgentEngineRegistry | None = None,
        profile_compiler: RuntimeProfileCompiler | None = None,
        profile_cache: RuntimeProfileCache | None = None,
    ) -> None:
        self.agent_workflow = agent_workflow
        self.engine_registry = engine_registry or default_agent_engine_registry()
        self.profile_compiler = profile_compiler or RuntimeProfileCompiler()
        self.profile_cache = profile_cache or RuntimeProfileCache()

    def run_execution(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        model: Any | None = None,
        emit: Any | None = None,
    ) -> ExecutionRecord:
        agent = self._agent(item.assignee_agent_id)
        profiles = self.profile_cache.compile_or_get(
            self.agent_workflow,
            compiler=self.profile_compiler,
        ).profiles
        profile = next((profile for profile in profiles if profile.agent_id == agent.id), None)
        if profile is None:
            profile = self.profile_compiler.compile(
                recipe_from_workflow_agent(agent, primary_planner_id=self.agent_workflow.primary_planner_id)
            )
        engine = self.engine_registry.get(profile.engine_id)
        return engine.run_execution(agent=agent, item=item, envelope=envelope, model=model, emit=emit)

    def _agent(self, agent_id: str) -> AgentWorkflowAgent:
        for agent in self.agent_workflow.agents:
            if agent.id == agent_id:
                return agent
        raise KeyError(agent_id)
