from __future__ import annotations

from typing import Any

from coder_workbench.actions import ActionGateway
from coder_workbench.agent_engine import AgentEngineRegistry, default_agent_engine_registry
from coder_workbench.agent_graph.planner_strategy import (
    PlannerStrategyContext,
    planner_mode_from,
    planner_strategy_for_mode,
)
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, ExecutionRecord, WorkItem
from coder_workbench.agent_harness.contracts import (
    CODE_WORKER_HARNESS,
    PLANNER_DECISION_HARNESS,
    PLANNER_ORDER_HARNESS,
)
from coder_workbench.agent_model import RuntimeProfileCache, RuntimeProfileCompiler, recipe_from_workflow_agent
from coder_workbench.budget import BudgetBroker
from coder_workbench.config import RuntimeConfig, load_runtime_config
from coder_workbench.core import AgentWorkflowAgent, AgentWorkflowSpec
from coder_workbench.llm import create_chat_model
from coder_workbench.runtime_capabilities import CapabilitySet, resolve_capabilities
from coder_workbench.runtime_state import SharedRunState, build_executor_state_view, build_planner_state_view


ModelFactory = Any


class AgentRun:
    """Runs one Agent work item through a compiled runtime profile and AgentEngine."""

    def __init__(
        self,
        agent_workflow: AgentWorkflowSpec,
        *,
        engine_registry: AgentEngineRegistry | None = None,
        profile_compiler: RuntimeProfileCompiler | None = None,
        profile_cache: RuntimeProfileCache | None = None,
        runtime_settings: Any | None = None,
        model_factory: ModelFactory = create_chat_model,
        budget_broker: BudgetBroker | None = None,
        action_gateway: ActionGateway | None = None,
        run_id: str | None = None,
        initial_data: dict[str, Any] | None = None,
    ) -> None:
        self.agent_workflow = agent_workflow
        self.engine_registry = engine_registry or default_agent_engine_registry()
        self.profile_compiler = profile_compiler or RuntimeProfileCompiler()
        self.profile_cache = profile_cache or RuntimeProfileCache()
        self.runtime_settings = runtime_settings
        self.model_factory = model_factory
        self.budget_broker = budget_broker
        self.action_gateway = action_gateway
        self.run_id = run_id
        self.initial_data = initial_data or {}

    def run_planner_order(
        self,
        request: str,
        *,
        previous_bundle: Any | None = None,
        previous_round_summary: dict[str, Any] | None = None,
        skill_index: Any | None = None,
        repo_intelligence: dict[str, Any] | None = None,
        round_number: int = 1,
        emit: Any | None = None,
    ) -> Any:
        planner = self._agent(self.agent_workflow.primary_planner_id)
        profile = self._profile_for_agent(planner)
        state_view = self._planner_state_view()
        capability_set = resolve_capabilities(
            agent=planner,
            runtime_profile=profile,
            harness_id=PLANNER_ORDER_HARNESS.harness_id,
            work_item=None,
            state_view=state_view,
            installed_capabilities=skill_index,
        )
        self._record_capability_set(
            agent_id=planner.id,
            harness_id=PLANNER_ORDER_HARNESS.harness_id,
            capability_set=capability_set,
        )
        mode = self._planner_mode()
        strategy = planner_strategy_for_mode(mode)
        order = strategy.create_order(
            PlannerStrategyContext(
                agent_workflow=self.agent_workflow,
                request=request,
                round_number=round_number,
                previous_bundle=previous_bundle,
                previous_round_summary=previous_round_summary,
                skill_index=skill_index,
                repo_intelligence=repo_intelligence,
                initial_data=self.initial_data,
            )
        )
        if order is not None:
            self._emit_strategy_used(emit, mode, "planner_order", round_number)
            return order
        return self.engine_registry.planner().run_planner_order(
            request,
            agent_workflow=self.agent_workflow,
            runtime_settings=self.runtime_settings,
            model_factory=self.model_factory,
            budget_broker=self.budget_broker,
            action_gateway=self.action_gateway,
            run_id=self.run_id,
            previous_bundle=previous_bundle,
            previous_round_summary=previous_round_summary,
            skill_index=skill_index,
            repo_intelligence=repo_intelligence,
            state_view=state_view,
            capability_set=capability_set.model_dump(mode="json"),
            round_number=round_number,
            emit=emit,
        )

    def run_execution(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        model: Any | None = None,
        emit: Any | None = None,
    ) -> ExecutionRecord:
        agent = self._agent(item.assignee_agent_id)
        profile = self._profile_for_agent(agent)
        state_view = self._executor_state_view(item.work_item_id)
        harness_id = profile.harness_id or CODE_WORKER_HARNESS.harness_id
        capability_set = resolve_capabilities(
            agent=agent,
            runtime_profile=profile,
            harness_id=harness_id,
            work_item=item,
            state_view=state_view,
            installed_capabilities={"allowed_skill_ids": envelope.allowed_skill_ids},
        )
        capability_payload = capability_set.model_dump(mode="json")
        envelope = envelope.model_copy(update={"capability_set": capability_payload})
        self._record_capability_set(
            agent_id=agent.id,
            harness_id=harness_id,
            capability_set=capability_set,
        )
        engine = self.engine_registry.get(profile.engine_id)
        return engine.run_execution(
            agent=agent,
            item=item,
            envelope=envelope,
            capability_set=capability_payload,
            model=model or self._chat_model(),
            emit=emit,
        )

    def run_planner_decision(
        self,
        *,
        bundle: Any,
        emit: Any | None = None,
    ) -> dict[str, Any]:
        planner = self._agent(self.agent_workflow.primary_planner_id)
        profile = self._profile_for_agent(planner)
        state_view = self._planner_state_view()
        capability_set = resolve_capabilities(
            agent=planner,
            runtime_profile=profile,
            harness_id=PLANNER_DECISION_HARNESS.harness_id,
            work_item=None,
            state_view=state_view,
            installed_capabilities=self.initial_data.get("skill_index"),
        )
        self._record_capability_set(
            agent_id=planner.id,
            harness_id=PLANNER_DECISION_HARNESS.harness_id,
            capability_set=capability_set,
        )
        mode = self._planner_mode()
        strategy = planner_strategy_for_mode(mode)
        decision = strategy.create_decision(
            PlannerStrategyContext(
                agent_workflow=self.agent_workflow,
                round_number=getattr(bundle, "round", 1),
                initial_data=self.initial_data,
                bundle=bundle,
            )
        )
        if decision is not None:
            self._emit_strategy_used(emit, mode, "planner_decision", int(getattr(bundle, "round", 1) or 1))
            return decision
        return self.engine_registry.planner().run_planner_decision(
            agent_workflow=self.agent_workflow,
            bundle=bundle,
            runtime_settings=self.runtime_settings,
            model_factory=self.model_factory,
            budget_broker=self.budget_broker,
            action_gateway=self.action_gateway,
            run_id=self.run_id,
            state_view=state_view,
            capability_set=capability_set.model_dump(mode="json"),
            emit=emit,
        )

    def _agent(self, agent_id: str) -> AgentWorkflowAgent:
        for agent in self.agent_workflow.agents:
            if agent.id == agent_id:
                return agent
        raise KeyError(agent_id)

    def _profile_for_agent(self, agent: AgentWorkflowAgent) -> Any:
        profiles = self.profile_cache.compile_or_get(
            self.agent_workflow,
            compiler=self.profile_compiler,
        ).profiles
        profile = next((profile for profile in profiles if profile.agent_id == agent.id), None)
        if profile is not None:
            return profile
        return self.profile_compiler.compile(
            recipe_from_workflow_agent(agent, primary_planner_id=self.agent_workflow.primary_planner_id)
        )

    def _shared_run_state(self) -> SharedRunState | None:
        value = self.initial_data.get("shared_run_state")
        if not isinstance(value, dict):
            return None
        try:
            return SharedRunState.model_validate(value)
        except Exception:
            return None

    def _planner_state_view(self) -> dict[str, Any]:
        state = self._shared_run_state()
        return build_planner_state_view(state) if state is not None else {}

    def _executor_state_view(self, work_item_id: str) -> dict[str, Any]:
        state = self._shared_run_state()
        return build_executor_state_view(state, work_item_id) if state is not None else {}

    def _record_capability_set(
        self,
        *,
        agent_id: str,
        harness_id: str,
        capability_set: CapabilitySet,
    ) -> None:
        payload = capability_set.model_dump(mode="json")
        records = self.initial_data.setdefault("capability_sets", [])
        if isinstance(records, list):
            records.append(
                {
                    "agent_id": agent_id,
                    "harness_id": harness_id,
                    "skills": [skill["skill_id"] for skill in payload.get("skills", [])],
                    "tools": [tool["name"] for tool in payload.get("tools", [])],
                    "memory_scopes": [
                        f"{scope['scope']}:{scope['access']}"
                        for scope in payload.get("memory_scopes", [])
                    ],
                    "denied": [capability["name"] for capability in payload.get("denied", [])],
                }
            )
        by_harness = self.initial_data.setdefault("capability_sets_by_harness", {})
        if isinstance(by_harness, dict):
            by_harness[f"{agent_id}:{harness_id}"] = payload

    def _chat_model(self) -> Any | None:
        config = self._runtime_config()
        if not config.has_llm_credentials:
            return None
        return self.model_factory(config)

    def _runtime_config(self) -> RuntimeConfig:
        if self.runtime_settings is not None:
            from coder_workbench.server.settings import resolve_settings_config

            values = resolve_settings_config(self.runtime_settings, None, None)
            return RuntimeConfig(
                provider=str(values["provider"]),
                model=str(values["model"]),
                api_key=values["api_key"],
                base_url=values["base_url"],
            )
        return load_runtime_config()

    def _planner_mode(self) -> str:
        return planner_mode_from(self.initial_data, self.runtime_settings)

    def _emit_strategy_used(self, emit: Any | None, mode: str, artifact_type: str, round_number: int) -> None:
        if emit is None:
            return
        emit(
            "planner.strategy.used",
            "PlannerStrategy produced local artifact",
            planner_mode=mode,
            artifact_type=artifact_type,
            round=round_number,
        )
