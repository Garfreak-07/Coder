from __future__ import annotations

from typing import Any, Protocol

from coder_workbench.agent_engine.runtime import AgentGraphExecutorError, ModelFactory
from coder_workbench.agent_graph.agent_run import AgentRun
from coder_workbench.agent_graph.schema import (
    AgentTaskEnvelope,
    ExecutionRecord,
    FinalTestRecord,
    PlannerInputBundle,
    PlannerOrder,
    TestRecord,
    WorkItem,
)
from coder_workbench.budget import BudgetBroker
from coder_workbench.config import RuntimeConfig, load_runtime_config
from coder_workbench.core import AgentWorkflowSpec
from coder_workbench.llm import create_chat_model
from coder_workbench.skills.index import SkillIndex


class AgentGraphExecutorProtocol(Protocol):
    def create_planner_order(
        self,
        request: str,
        *,
        previous_bundle: PlannerInputBundle | None = None,
        previous_round_summary: dict[str, Any] | None = None,
        planner_human_response: dict[str, Any] | None = None,
        skill_index: SkillIndex | None = None,
        repo_intelligence: dict[str, Any] | None = None,
        round_number: int = 1,
        emit: Any | None = None,
    ) -> PlannerOrder:
        ...

    def create_execution_result(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        emit: Any | None = None,
    ) -> ExecutionRecord:
        ...

    def create_test_result(
        self,
        *,
        item: WorkItem,
        execution_artifact: dict[str, Any],
        tester_agent_id: str,
        emit: Any | None = None,
    ) -> TestRecord:
        ...

    def create_planner_decision(
        self,
        *,
        bundle: PlannerInputBundle,
        planner_human_response: dict[str, Any] | None = None,
        emit: Any | None = None,
    ) -> dict[str, Any]:
        ...

    def create_final_test_result(
        self,
        *,
        bundle: PlannerInputBundle,
        final_tester_agent_id: str,
        emit: Any | None = None,
    ) -> FinalTestRecord:
        ...


class AgentGraphExecutor:
    """Compatibility adapter only. Do not add artifact-specific execution logic here."""

    def __init__(
        self,
        agent_workflow: AgentWorkflowSpec,
        *,
        runtime_settings: Any | None = None,
        model_factory: ModelFactory = create_chat_model,
        agent_run: AgentRun | None = None,
        budget_broker: BudgetBroker | None = None,
        run_id: str | None = None,
    ) -> None:
        self.agent_workflow = agent_workflow
        self.runtime_settings = runtime_settings
        self.model_factory = model_factory
        self.agent_run = agent_run or AgentRun(agent_workflow)
        self.budget_broker = budget_broker
        self.run_id = run_id

    def create_planner_order(
        self,
        request: str,
        *,
        previous_bundle: PlannerInputBundle | None = None,
        previous_round_summary: dict[str, Any] | None = None,
        planner_human_response: dict[str, Any] | None = None,
        skill_index: SkillIndex | None = None,
        repo_intelligence: dict[str, Any] | None = None,
        round_number: int = 1,
        emit: Any | None = None,
    ) -> PlannerOrder:
        return self.agent_run.engine_registry.planner().run_planner_order(
            request,
            agent_workflow=self.agent_workflow,
            runtime_settings=self.runtime_settings,
            model_factory=self.model_factory,
            budget_broker=self.budget_broker,
            run_id=self.run_id,
            previous_bundle=previous_bundle,
            previous_round_summary=previous_round_summary,
            planner_human_response=planner_human_response,
            skill_index=skill_index,
            repo_intelligence=repo_intelligence,
            round_number=round_number,
            emit=emit,
        )

    def create_execution_result(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        emit: Any | None = None,
    ) -> ExecutionRecord:
        return self.agent_run.run_execution(
            item=item,
            envelope=envelope,
            model=self._chat_model(),
            emit=emit,
        )

    def create_test_result(
        self,
        *,
        item: WorkItem,
        execution_artifact: dict[str, Any],
        tester_agent_id: str,
        emit: Any | None = None,
    ) -> TestRecord:
        return self.agent_run.engine_registry.tester().run_test(
            agent_workflow=self.agent_workflow,
            item=item,
            execution_artifact=execution_artifact,
            tester_agent_id=tester_agent_id,
            runtime_settings=self.runtime_settings,
            model_factory=self.model_factory,
            budget_broker=self.budget_broker,
            run_id=self.run_id,
            emit=emit,
        )

    def create_planner_decision(
        self,
        *,
        bundle: PlannerInputBundle,
        planner_human_response: dict[str, Any] | None = None,
        emit: Any | None = None,
    ) -> dict[str, Any]:
        return self.agent_run.engine_registry.planner().run_planner_decision(
            agent_workflow=self.agent_workflow,
            bundle=bundle,
            planner_human_response=planner_human_response,
            runtime_settings=self.runtime_settings,
            model_factory=self.model_factory,
            budget_broker=self.budget_broker,
            run_id=self.run_id,
            emit=emit,
        )

    def create_final_test_result(
        self,
        *,
        bundle: PlannerInputBundle,
        final_tester_agent_id: str,
        emit: Any | None = None,
    ) -> FinalTestRecord:
        return self.agent_run.engine_registry.final_review().run_final_test(
            agent_workflow=self.agent_workflow,
            bundle=bundle,
            final_tester_agent_id=final_tester_agent_id,
            runtime_settings=self.runtime_settings,
            model_factory=self.model_factory,
            budget_broker=self.budget_broker,
            run_id=self.run_id,
            emit=emit,
        )

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
