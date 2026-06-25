from __future__ import annotations

from typing import Any

from coder_workbench.actions import ActionGateway
from coder_workbench.agent_graph.prompts import build_worker_execution_prompt
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
from coder_workbench.context import build_harness_context_packet
from coder_workbench.core import AgentWorkflowAgent, AgentWorkflowSpec
from coder_workbench.harness_runtime import HarnessRuntimeContext, HarnessRuntimeManager, OpenHandsRuntimeProvider
from coder_workbench.harness_runtime.fallback_provider import InternalFallbackProvider
from coder_workbench.llm import create_chat_model
from coder_workbench.runtime_capabilities import CapabilitySet, resolve_capabilities
from coder_workbench.runtime_state import SharedRunState, build_executor_state_view, build_planner_state_view


ModelFactory = Any


class AgentRun:
    """Runs Agent work through HarnessRuntimeManager and compatibility fallback providers."""

    def __init__(
        self,
        agent_workflow: AgentWorkflowSpec,
        *,
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
        self.profile_compiler = profile_compiler or RuntimeProfileCompiler()
        self.profile_cache = profile_cache or RuntimeProfileCache()
        self.runtime_settings = runtime_settings
        self.model_factory = model_factory
        self.budget_broker = budget_broker
        self.action_gateway = action_gateway
        self.run_id = run_id
        self.initial_data = initial_data or {}
        self.harness_runtime_manager = HarnessRuntimeManager(
            providers=[
                OpenHandsRuntimeProvider(),
                InternalFallbackProvider(
                    planner_order_runner=self._run_planner_order_legacy,
                    task_execution_runner=self._run_execution_legacy,
                    planner_decision_runner=self._run_planner_decision_legacy,
                )
            ]
        )

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
        profile_id = _runtime_profile_id(profile, "openhands-workflow-supervisor-default")
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
        context = self._harness_context(
            agent_id=planner.id,
            harness_id="conversation-harness",
            mode="workflow_supervisor",
            profile_id=profile_id,
            round_number=round_number,
            state_view=state_view,
            capability_set=capability_set.model_dump(mode="json"),
            request_text=request,
        )
        result = self.harness_runtime_manager.run_workflow_supervisor(
            context=context,
            profile_id=profile_id,
            input_artifacts={
                "legacy_operation": "planner_order",
                "legacy_kwargs": {
                    "request": request,
                    "previous_bundle": previous_bundle,
                    "previous_round_summary": previous_round_summary,
                    "skill_index": skill_index,
                    "repo_intelligence": repo_intelligence,
                    "round_number": round_number,
                    "state_view": state_view,
                    "capability_set": capability_set.model_dump(mode="json"),
                },
            },
            emit=emit,
        )
        legacy_output = getattr(result, "_legacy_output", None)
        if legacy_output is not None:
            return legacy_output
        return result.artifact

    def _run_planner_order_legacy(
        self,
        *,
        request: str,
        previous_bundle: Any | None = None,
        previous_round_summary: dict[str, Any] | None = None,
        skill_index: Any | None = None,
        repo_intelligence: dict[str, Any] | None = None,
        round_number: int = 1,
        state_view: dict[str, Any] | None = None,
        capability_set: dict[str, Any] | None = None,
        emit: Any | None = None,
    ) -> Any:
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
        raise RuntimeError(f"PlannerStrategy mode {mode!r} did not produce a planner_order")

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
        profile_id = _runtime_profile_id(profile, "openhands-task-executor-default")
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
        context = self._harness_context(
            agent_id=agent.id,
            harness_id="task-execution-harness",
            mode="task_execution",
            profile_id=profile_id,
            round_number=envelope.round,
            state_view=state_view,
            capability_set=capability_payload,
            work_item=item,
            task_envelope=envelope,
        )
        result = self.harness_runtime_manager.run_task_execution(
            context=context,
            profile_id=profile_id,
            input_artifacts={
                "legacy_operation": "task_execution",
                "legacy_kwargs": {
                    "agent": agent,
                    "engine_id": profile.engine_id,
                    "item": item,
                    "envelope": envelope,
                    "capability_set": capability_payload,
                    "model": model,
                },
            },
            emit=emit,
        )
        legacy_output = getattr(result, "_legacy_output", None)
        if legacy_output is not None:
            return legacy_output
        raise RuntimeError("HarnessRuntimeManager did not return an ExecutionRecord")

    def _run_execution_legacy(
        self,
        *,
        agent: AgentWorkflowAgent,
        engine_id: str,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        capability_set: dict[str, Any],
        model: Any | None = None,
        emit: Any | None = None,
    ) -> ExecutionRecord:
        from coder_workbench.agent_harness import CodeWorkerHarness

        active_model = model or self._chat_model()
        return CodeWorkerHarness(model=active_model).create_execution_result(
            item=item,
            envelope=envelope,
            coding_context_packet=envelope.coding_context_packet if active_model is not None else None,
            prompt=build_worker_execution_prompt(
                agent=agent,
                item=item,
                envelope=envelope,
                capability_set=capability_set,
            ),
            capability_set=capability_set,
            repo_root=str(self.initial_data.get("repo_root") or "."),
            sandbox_root=_optional_string(self.initial_data.get("sandbox_root")),
            scopes=_scopes_from_data(self.initial_data),
            run_id=self.run_id,
            data=self.initial_data,
            action_gateway=self.action_gateway,
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
        profile_id = _runtime_profile_id(profile, "openhands-workflow-supervisor-default")
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
        round_number = int(getattr(bundle, "round", 1) or 1)
        supervisor_facts = _workflow_supervisor_facts_from_data(self.initial_data)
        evidence_refs = _unique_strings([*_evidence_refs_from_bundle(bundle), *supervisor_facts["evidence_refs"]])
        context = self._harness_context(
            agent_id=planner.id,
            harness_id="conversation-harness",
            mode="workflow_supervisor",
            profile_id=profile_id,
            round_number=round_number,
            state_view=state_view,
            capability_set=capability_set.model_dump(mode="json"),
            request_text=str(getattr(bundle, "user_goal", "") or self.initial_data.get("request") or ""),
            round_summary=supervisor_facts["round_summary"],
            execution_results=supervisor_facts["execution_results"],
            verification_summaries=supervisor_facts["verification_summaries"],
            blocked_reasons=supervisor_facts["blocked_reasons"],
            changed_files_summary=supervisor_facts["changed_files_summary"],
            evidence_refs=evidence_refs,
            native_event_refs=supervisor_facts["native_runtime_refs"],
            diff_refs=supervisor_facts["diff_refs"],
            log_refs=supervisor_facts["log_refs"],
        )
        result = self.harness_runtime_manager.run_workflow_supervisor(
            context=context,
            profile_id=profile_id,
            input_artifacts={
                "legacy_operation": "planner_decision",
                "legacy_kwargs": {
                    "bundle": bundle,
                    "state_view": state_view,
                    "capability_set": capability_set.model_dump(mode="json"),
                },
            },
            emit=emit,
        )
        legacy_output = getattr(result, "_legacy_output", None)
        if legacy_output is not None:
            return legacy_output
        return result.artifact or {}

    def _run_planner_decision_legacy(
        self,
        *,
        bundle: Any,
        state_view: dict[str, Any] | None = None,
        capability_set: dict[str, Any] | None = None,
        emit: Any | None = None,
    ) -> dict[str, Any]:
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
        raise RuntimeError(f"PlannerStrategy mode {mode!r} did not produce a planner_decision")

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

    def _harness_context(
        self,
        *,
        agent_id: str,
        harness_id: str,
        mode: str,
        profile_id: str,
        round_number: int | None,
        state_view: dict[str, Any] | None,
        capability_set: dict[str, Any] | None,
        request_text: str | None = None,
        work_item: WorkItem | None = None,
        task_envelope: AgentTaskEnvelope | None = None,
        evidence_refs: list[str] | None = None,
        native_event_refs: list[str] | None = None,
        diff_refs: list[str] | None = None,
        log_refs: list[str] | None = None,
        round_summary: dict[str, Any] | None = None,
        execution_results: list[dict[str, Any]] | None = None,
        verification_summaries: list[dict[str, Any]] | None = None,
        blocked_reasons: list[str] | None = None,
        changed_files_summary: dict[str, Any] | None = None,
    ) -> HarnessRuntimeContext:
        shared_state = self.initial_data.get("shared_run_state")
        user_goal = request_text or str(self.initial_data.get("request") or "")
        context_packet = build_harness_context_packet(
            mode=mode,
            user_goal=user_goal,
            workflow_id=self.agent_workflow.id,
            agent_id=agent_id,
            round_number=round_number,
            work_item=work_item,
            task_envelope=task_envelope,
            state_view=state_view,
            capability_set=capability_set,
            round_summary=round_summary,
            execution_results=execution_results,
            verification_summaries=verification_summaries,
            blocked_reasons=blocked_reasons,
            changed_files_summary=changed_files_summary,
            evidence_refs=evidence_refs,
            native_event_refs=native_event_refs,
            diff_refs=diff_refs,
            log_refs=log_refs,
        )
        return HarnessRuntimeContext(
            run_id=self.run_id or str(self.initial_data.get("run_id") or self.agent_workflow.id),
            round=round_number,
            agent_id=agent_id,
            workflow_id=self.agent_workflow.id,
            harness_id=harness_id,
            mode=mode,
            profile_id=profile_id,
            repo_root=str(self.initial_data.get("repo_root") or "."),
            sandbox_root=_optional_string(self.initial_data.get("sandbox_root")),
            context_packet=context_packet,
            capability_set=capability_set,
            shared_run_state=shared_state if isinstance(shared_state, dict) else None,
            round_working_set=state_view,
            initial_data=self.initial_data,
        )

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


def _optional_string(value: Any) -> str | None:
    text = str(value or "").strip()
    return text or None


def _runtime_profile_id(profile: Any, default: str) -> str:
    value = str(getattr(profile, "harness_runtime_profile_id", "") or "").strip()
    return value or default


def _scopes_from_data(data: dict[str, Any]) -> list[str]:
    value = data.get("scopes", data.get("scope"))
    if value is None:
        return []
    if isinstance(value, str):
        return [value] if value.strip() else []
    if isinstance(value, list):
        return [str(item) for item in value if str(item).strip()]
    return [str(value)]


def _evidence_refs_from_bundle(bundle: Any) -> list[str]:
    refs: list[str] = []
    for item in getattr(bundle, "items", []) or []:
        refs.extend(str(ref) for ref in getattr(item, "refs", []) or [] if str(ref).strip())
    return refs


def _workflow_supervisor_facts_from_data(data: dict[str, Any]) -> dict[str, Any]:
    graph_cache = data.get("graph_run_cache") if isinstance(data.get("graph_run_cache"), dict) else {}
    execution_cache = graph_cache.get("execution_cache") if isinstance(graph_cache.get("execution_cache"), dict) else {}
    execution_results: list[dict[str, Any]] = []
    verification_summaries: list[dict[str, Any]] = []
    blocked_reasons: list[str] = []
    changed_files_summary: dict[str, list[str]] = {"created": [], "modified": [], "deleted": []}
    evidence_refs: list[str] = []
    for record in execution_cache.values():
        if not isinstance(record, dict):
            continue
        artifact = record.get("artifact_payload") if isinstance(record.get("artifact_payload"), dict) else {}
        if not artifact:
            continue
        execution_results.append(artifact)
        evidence_refs.extend(_artifact_evidence_refs(artifact))
        if artifact.get("status") == "blocked":
            reason = str(artifact.get("blocker_reason") or artifact.get("summary") or "").strip()
            if reason:
                blocked_reasons.append(reason)
        _extend_file_summary(changed_files_summary, "created", artifact.get("created_files"))
        _extend_file_summary(changed_files_summary, "modified", artifact.get("changed_files"))
        _extend_file_summary(changed_files_summary, "deleted", artifact.get("deleted_files"))
        verification = artifact.get("verification") if isinstance(artifact.get("verification"), dict) else {}
        if verification:
            verification_summaries.append(
                {
                    "work_item_id": artifact.get("work_item_id"),
                    "status": verification.get("status"),
                    "evidence_refs": _string_list(verification.get("evidence_refs")),
                    "remaining_work": _string_list(verification.get("remaining_work")),
                    "no_check_rationale": verification.get("no_check_rationale"),
                }
            )

    round_summary = data.get("round_summary") if isinstance(data.get("round_summary"), dict) else None
    native_runtime_refs = _flatten_ref_map(graph_cache.get("native_runtime_refs"))
    diff_refs = _flatten_ref_map(graph_cache.get("diff_refs"))
    log_refs = _flatten_ref_map(graph_cache.get("log_refs"))
    return {
        "round_summary": round_summary,
        "execution_results": execution_results,
        "verification_summaries": verification_summaries,
        "blocked_reasons": _unique_strings(blocked_reasons),
        "changed_files_summary": {key: refs for key, refs in changed_files_summary.items() if refs},
        "evidence_refs": _unique_strings(evidence_refs),
        "native_runtime_refs": native_runtime_refs,
        "diff_refs": diff_refs,
        "log_refs": log_refs,
    }


def _artifact_evidence_refs(artifact: dict[str, Any]) -> list[str]:
    refs: list[str] = []
    for key in ("artifact_id", "evidence_refs", "patch_refs", "outputs"):
        value = artifact.get(key)
        if isinstance(value, str):
            refs.append(value)
        else:
            refs.extend(_string_list(value))
    verification = artifact.get("verification") if isinstance(artifact.get("verification"), dict) else {}
    refs.extend(_string_list(verification.get("evidence_refs")))
    for check in verification.get("checks_run") or []:
        if not isinstance(check, dict):
            continue
        refs.extend(_string_list(check.get("evidence_refs")))
        output_ref = check.get("output_ref")
        if isinstance(output_ref, str):
            refs.append(output_ref)
    return _unique_strings(refs)


def _extend_file_summary(summary: dict[str, list[str]], key: str, value: Any) -> None:
    for item in _string_list(value):
        if item not in summary[key]:
            summary[key].append(item)


def _flatten_ref_map(value: Any) -> list[str]:
    refs: list[str] = []
    if isinstance(value, dict):
        for item in value.values():
            refs.extend(_string_list(item))
    return _unique_strings(refs)


def _string_list(value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    return [str(item) for item in value if str(item).strip()]


def _unique_strings(values: list[str]) -> list[str]:
    seen: set[str] = set()
    output: list[str] = []
    for value in values:
        text = str(value).strip()
        if not text or text in seen:
            continue
        seen.add(text)
        output.append(text)
    return output
