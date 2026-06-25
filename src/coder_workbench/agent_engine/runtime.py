from __future__ import annotations

from typing import TYPE_CHECKING, Any, Callable, Protocol

from pydantic import ValidationError

from coder_workbench.actions import ActionGateway, ActionSpec, RunContext
from coder_workbench.budget import BudgetBroker
from coder_workbench.config import RuntimeConfig, load_runtime_config
from coder_workbench.core import AgentWorkflowAgent
from coder_workbench.core.artifacts import ArtifactValidationError, validate_artifact
from coder_workbench.llm import create_chat_model
from coder_workbench.skills import estimate_tokens

if TYPE_CHECKING:
    from coder_workbench.agent_graph.schema import (
        AgentTaskEnvelope,
        ExecutionRecord,
        PlannerInputBundle,
        PlannerOrder,
        WorkItem,
    )
    from coder_workbench.core import AgentWorkflowSpec
    from coder_workbench.skills.index import SkillIndex


EmitEvent = Callable[..., None]
ModelFactory = Callable[[RuntimeConfig], Any]


class AgentEngineRuntimeError(ValueError):
    def __init__(self, message: str, *, status_code: str) -> None:
        self.status_code = status_code
        super().__init__(message)


class AgentEngine(Protocol):
    id: str

    def run_execution(
        self,
        *,
        agent: AgentWorkflowAgent,
        item: "WorkItem",
        envelope: "AgentTaskEnvelope",
        capability_set: dict[str, Any] | None = None,
        model: Any | None = None,
        emit: Any | None = None,
    ) -> "ExecutionRecord":
        ...


class ModelBackedEngine:
    def _invoke_or_mock(
        self,
        *,
        artifact_type: str,
        agent_id: str,
        prompt: str,
        mock_payload: dict[str, Any],
        emit: EmitEvent | None,
        agent_workflow: "AgentWorkflowSpec",
        runtime_settings: Any | None,
        model_factory: ModelFactory,
        budget_broker: BudgetBroker | None,
        action_gateway: ActionGateway | None,
        run_id: str | None,
        model: Any | None = None,
        fallback_payload: dict[str, Any] | None = None,
        failure_status_code: str | None = None,
        work_item_id: str | None = None,
        merge_index: int | None = None,
    ) -> dict[str, Any]:
        active_model = model if model is not None else self._chat_model(runtime_settings, model_factory)
        if active_model is None:
            return self._validate_payload(
                mock_payload,
                artifact_type=artifact_type,
                agent_id=agent_id,
                emit=emit,
                fallback_payload=fallback_payload,
                failure_status_code=failure_status_code,
                work_item_id=work_item_id,
                merge_index=merge_index,
            )

        self._emit(
            emit,
            "agent_graph.agent_call.started",
            "AgentGraph model call started",
            agent_id=agent_id,
            artifact_type=artifact_type,
            work_item_id=work_item_id,
            merge_index=merge_index,
        )
        reservation = None
        if budget_broker is not None:
            reservation = budget_broker.reserve_model_call(
                run_id=run_id or agent_workflow.id,
                agent_id=agent_id,
                estimated_tokens=estimate_tokens(prompt),
                action_type=f"model_call:{artifact_type}",
            )
            if not reservation.approved:
                self._emit(
                    emit,
                    "budget.warning",
                    "BudgetBroker denied AgentGraph model call",
                    agent_id=agent_id,
                    artifact_type=artifact_type,
                    work_item_id=work_item_id,
                    merge_index=merge_index,
                    error_code=reservation.reason,
                )
                raise AgentEngineRuntimeError(
                    "BudgetBroker denied AgentGraph model call",
                    status_code=reservation.reason,
                )
        response = active_model.invoke(prompt)
        if reservation is not None:
            budget_broker.commit(reservation.reservation_id, actual_tokens=estimate_tokens(prompt))

        from coder_workbench.agent_graph.prompts import schema_notes_for_artifact
        from coder_workbench.agent_graph.repair import parse_json_object

        content = getattr(response, "content", str(response))
        payload = parse_json_object(str(content))
        if payload is None:
            payload = {"artifact_type": artifact_type}
            errors = [{"loc": ["response"], "msg": "model output was not a JSON object"}]
        else:
            errors = []

        artifact = self._validate_model_payload(
            payload,
            artifact_type=artifact_type,
            agent_id=agent_id,
            emit=emit,
            work_item_id=work_item_id,
            merge_index=merge_index,
            initial_errors=errors,
            action_gateway=action_gateway,
            agent_workflow=agent_workflow,
            active_model=active_model,
            run_id=run_id,
        )
        if artifact is not None:
            self._emit(
                emit,
                "agent_graph.agent_call.completed",
                "AgentGraph model call completed",
                agent_id=agent_id,
                artifact_type=artifact_type,
                work_item_id=work_item_id,
                merge_index=merge_index,
            )
            return artifact

        gateway = action_gateway or ActionGateway(budget_broker=budget_broker)
        repair = gateway.run(
            ActionSpec(
                action_id=f"repair_artifact:{artifact_type}:{agent_id}",
                action_type="repair_artifact",
                input={
                    "expected_type": artifact_type,
                    "agent_id": agent_id,
                    "invalid_output": str(content),
                    "work_item_id": work_item_id,
                    "merge_index": merge_index,
                    "schema_notes": schema_notes_for_artifact(artifact_type),
                },
            ),
            run_context=RunContext(
                run_id=run_id or agent_workflow.id,
                repo_root=".",
                model=active_model,
                emit=emit,
            ),
        )
        repaired = repair.payload.get("artifact") if repair.status == "ok" else None
        if isinstance(repaired, dict):
            return repaired

        if fallback_payload is not None:
            return self._validate_payload(
                fallback_payload,
                artifact_type=artifact_type,
                agent_id=agent_id,
                emit=emit,
                fallback_payload=None,
                work_item_id=work_item_id,
                merge_index=merge_index,
            )
        raise AgentEngineRuntimeError(
            f"{artifact_type} schema validation failed after one repair",
            status_code=failure_status_code or f"{artifact_type}_schema_failed",
        )

    def _validate_model_payload(
        self,
        payload: dict[str, Any],
        *,
        artifact_type: str,
        agent_id: str,
        emit: EmitEvent | None,
        work_item_id: str | None,
        merge_index: int | None,
        initial_errors: list[dict[str, Any]] | None,
        action_gateway: ActionGateway | None,
        agent_workflow: "AgentWorkflowSpec",
        active_model: Any,
        run_id: str | None,
    ) -> dict[str, Any] | None:
        if initial_errors:
            self._emit_schema_failed(
                emit,
                agent_id=agent_id,
                artifact_type=artifact_type,
                errors=initial_errors,
                work_item_id=work_item_id,
                merge_index=merge_index,
            )
        gateway = action_gateway or ActionGateway()
        result = gateway.run(
            ActionSpec(
                action_id=f"validate_artifact:{artifact_type}:{agent_id}",
                action_type="validate_artifact",
                input={
                    "expected_type": artifact_type,
                    "artifact": payload,
                },
            ),
            run_context=RunContext(
                run_id=run_id or agent_workflow.id,
                repo_root=".",
                model=active_model,
                emit=emit,
            ),
        )
        if result.status == "ok" and isinstance(result.payload.get("artifact"), dict):
            return result.payload["artifact"]
        self._emit_schema_failed(
            emit,
            agent_id=agent_id,
            artifact_type=artifact_type,
            errors=[{"loc": ["artifact"], "msg": result.summary or "artifact validation failed"}],
            work_item_id=work_item_id,
            merge_index=merge_index,
        )
        return None

    def _validate_payload(
        self,
        payload: dict[str, Any],
        *,
        artifact_type: str,
        agent_id: str,
        emit: EmitEvent | None,
        fallback_payload: dict[str, Any] | None = None,
        failure_status_code: str | None = None,
        work_item_id: str | None = None,
        merge_index: int | None = None,
        initial_errors: list[dict[str, Any]] | None = None,
    ) -> dict[str, Any] | None:
        if initial_errors:
            self._emit_schema_failed(
                emit,
                agent_id=agent_id,
                artifact_type=artifact_type,
                errors=initial_errors,
                work_item_id=work_item_id,
                merge_index=merge_index,
            )
            return None
        try:
            return validate_artifact(payload, expected_type=artifact_type)
        except ArtifactValidationError as exc:
            self._emit_schema_failed(
                emit,
                agent_id=agent_id,
                artifact_type=artifact_type,
                errors=exc.errors,
                work_item_id=work_item_id,
                merge_index=merge_index,
            )
            if fallback_payload is not None:
                return validate_artifact(fallback_payload, expected_type=artifact_type)
            if failure_status_code:
                raise AgentEngineRuntimeError(
                    f"{artifact_type} failed schema validation",
                    status_code=failure_status_code,
                ) from exc
            return None

    def _chat_model(self, runtime_settings: Any | None, model_factory: ModelFactory) -> Any | None:
        config = self._runtime_config(runtime_settings)
        if not config.has_llm_credentials:
            return None
        return model_factory(config)

    def _runtime_config(self, runtime_settings: Any | None) -> RuntimeConfig:
        if runtime_settings is not None:
            from coder_workbench.server.settings import resolve_settings_config

            values = resolve_settings_config(runtime_settings, None, None)
            return RuntimeConfig(
                provider=str(values["provider"]),
                model=str(values["model"]),
                api_key=values["api_key"],
                base_url=values["base_url"],
            )
        return load_runtime_config()

    def _emit_schema_failed(
        self,
        emit: EmitEvent | None,
        *,
        agent_id: str,
        artifact_type: str,
        errors: list[dict[str, Any]],
        work_item_id: str | None,
        merge_index: int | None,
    ) -> None:
        self._emit(
            emit,
            "agent_graph.agent_call.schema_failed",
            "AgentGraph artifact schema validation failed",
            agent_id=agent_id,
            artifact_type=artifact_type,
            work_item_id=work_item_id,
            merge_index=merge_index,
            schema_errors=errors[:8],
        )

    def _emit(self, emit: EmitEvent | None, event_type: str, message: str, **payload: Any) -> None:
        if emit is None:
            return
        compact = {key: value for key, value in payload.items() if value is not None}
        emit(event_type, message, **compact)


class PlannerEngine(ModelBackedEngine):
    id = "planner-engine"

    def run_planner_order(
        self,
        request: str,
        *,
        agent_workflow: "AgentWorkflowSpec",
        runtime_settings: Any | None = None,
        model_factory: ModelFactory = create_chat_model,
        budget_broker: BudgetBroker | None = None,
        action_gateway: ActionGateway | None = None,
        run_id: str | None = None,
        previous_bundle: "PlannerInputBundle | None" = None,
        previous_round_summary: dict[str, Any] | None = None,
        skill_index: "SkillIndex | None" = None,
        repo_intelligence: dict[str, Any] | None = None,
        state_view: dict[str, Any] | None = None,
        capability_set: dict[str, Any] | None = None,
        round_number: int = 1,
        emit: EmitEvent | None = None,
    ) -> "PlannerOrder":
        from coder_workbench.agent_graph.prompts import build_planner_order_prompt
        from coder_workbench.agent_graph.schema import PlannerOrder

        payload = self._invoke_or_mock(
            artifact_type="planner_order",
            agent_id=agent_workflow.primary_planner_id,
            prompt=build_planner_order_prompt(
                request=request,
                agent_workflow=agent_workflow,
                previous_bundle=previous_bundle,
                previous_round_summary=previous_round_summary,
                skill_index=skill_index,
                repo_intelligence=repo_intelligence,
                state_view=state_view,
                capability_set=capability_set,
                round_number=round_number,
            ),
            mock_payload=_mock_planner_order_payload(
                agent_workflow,
                request,
                round_number=round_number,
                repo_intelligence=repo_intelligence,
            ),
            emit=emit,
            agent_workflow=agent_workflow,
            runtime_settings=runtime_settings,
            model_factory=model_factory,
            budget_broker=budget_broker,
            action_gateway=action_gateway,
            run_id=run_id,
            failure_status_code="planner_order_schema_failed",
        )
        try:
            return PlannerOrder.model_validate(_planner_order_payload(payload))
        except ValidationError as exc:
            raise AgentEngineRuntimeError(
                f"planner_order failed AgentGraph schema validation: {exc}",
                status_code="planner_order_schema_failed",
            ) from exc

    def run_planner_decision(
        self,
        *,
        agent_workflow: "AgentWorkflowSpec",
        bundle: "PlannerInputBundle",
        runtime_settings: Any | None = None,
        model_factory: ModelFactory = create_chat_model,
        budget_broker: BudgetBroker | None = None,
        action_gateway: ActionGateway | None = None,
        run_id: str | None = None,
        state_view: dict[str, Any] | None = None,
        capability_set: dict[str, Any] | None = None,
        emit: EmitEvent | None = None,
    ) -> dict[str, Any]:
        from coder_workbench.agent_graph.prompts import build_planner_decision_prompt

        planner = _agent(agent_workflow, agent_workflow.primary_planner_id)
        has_interrupts = bool(bundle.interrupts)
        has_failed_verification = any(item.verification_status == "fail" for item in bundle.items)
        has_blocked_work = any(item.execution_status == "blocked" or item.verification_status == "blocked" for item in bundle.items)
        has_debug_findings = any(effect.get("effect_type") == "debug_finding" for effect in bundle.effects)
        has_failed_check_effects = any(
            effect.get("effect_type") == "optional_check_command"
            and (effect.get("status") == "failed" or effect.get("passed") is False)
            for effect in bundle.effects
        )
        has_blocked_check_effects = any(
            effect.get("effect_type") == "optional_check_command"
            and effect.get("status") == "check_requires_planner_confirmation"
            for effect in bundle.effects
        )
        has_failed_runtime_actions = any(
            effect.get("effect_type") == "runtime_action" and effect.get("status") == "failed"
            for effect in bundle.effects
        )
        has_blocked_runtime_actions = any(
            effect.get("effect_type") == "runtime_action" and effect.get("status") == "blocked"
            for effect in bundle.effects
        )
        can_continue_from_interrupts = has_interrupts and all(
            interrupt.continue_without_human_possible is True
            for interrupt in bundle.interrupts
        )
        blocked_requires_finish = (
            has_interrupts
            or has_blocked_work
            or has_blocked_check_effects
            or has_blocked_runtime_actions
        ) and not can_continue_from_interrupts
        mock_next_action = (
            "continue"
            if can_continue_from_interrupts
            or has_failed_verification
            or has_debug_findings
            or has_failed_check_effects
            or has_failed_runtime_actions
            else "finish"
        )
        mock_final_status = "blocked" if blocked_requires_finish else None
        mock_reason = (
            "DebugFinding is inside the current RunContract; Planner will replan."
            if has_debug_findings
            else "Check result failed; Planner will replan inside the current RunContract."
            if has_failed_check_effects
            else "Check command requires Planner confirmation before it can continue."
            if has_blocked_check_effects
            else "Runtime action failed; Planner will replan inside the current RunContract."
            if has_failed_runtime_actions
            else "Runtime action requires approval before Planner can continue."
            if has_blocked_runtime_actions
            else "Executor requested Planner intervention."
            if has_interrupts
            else "Execution verification failed; Planner will replan inside the existing RunContract."
            if has_failed_verification
            else "Work is blocked and requires Planner or user judgment."
            if has_blocked_work
            else "Mock AgentGraph execution artifacts are complete."
        )
        return self._invoke_or_mock(
            artifact_type="planner_decision",
            agent_id=planner.id,
            prompt=build_planner_decision_prompt(
                planner=planner,
                bundle=bundle,
                state_view=state_view,
                capability_set=capability_set,
            ),
            mock_payload={
                "artifact_type": "planner_decision",
                "round": bundle.round,
                "task_done": mock_next_action == "finish" and mock_final_status is None,
                "next_action": mock_next_action,
                "final_status": mock_final_status,
                "risk_level": "medium"
                if has_interrupts
                or has_debug_findings
                or has_failed_check_effects
                or has_blocked_check_effects
                or has_blocked_runtime_actions
                or has_failed_runtime_actions
                else "low",
                "requires_human_confirmation": False,
                "reason": mock_reason,
                "next_round_goal": "Fix debug finding evidence and rerun checks." if has_debug_findings else "Fix failed check evidence and rerun checks." if has_failed_check_effects else "Replan around failed runtime action evidence." if has_failed_runtime_actions else "Fix failed execution verification and rerun checks." if has_failed_verification else "Resolve the blocked work item." if mock_next_action == "continue" else "",
                "remaining_auto_rounds": 2 if mock_next_action == "continue" else 0,
                "human_message": None,
            },
            emit=emit,
            agent_workflow=agent_workflow,
            runtime_settings=runtime_settings,
            model_factory=model_factory,
            budget_broker=budget_broker,
            action_gateway=action_gateway,
            run_id=run_id,
            failure_status_code="planner_decision_schema_failed",
        )


class CodeWorkerEngine:
    id = "code-worker-engine"

    def run_execution(
        self,
        *,
        agent: AgentWorkflowAgent,
        item: "WorkItem",
        envelope: "AgentTaskEnvelope",
        capability_set: dict[str, Any] | None = None,
        model: Any | None = None,
        emit: Any | None = None,
    ) -> "ExecutionRecord":
        from coder_workbench.agent_graph.prompts import build_worker_execution_prompt
        from coder_workbench.agent_harness import CodeWorkerHarness

        return CodeWorkerHarness(model=model).create_execution_result(
            item=item,
            envelope=envelope,
            coding_context_packet=envelope.coding_context_packet if model is not None else None,
            emit=emit,
            prompt=build_worker_execution_prompt(
                agent=agent,
                item=item,
                envelope=envelope,
                capability_set=capability_set,
            ),
        )


def _agent(agent_workflow: "AgentWorkflowSpec", agent_id: str) -> AgentWorkflowAgent:
    for agent in agent_workflow.agents:
        if agent.id == agent_id:
            return agent
    raise KeyError(agent_id)


def _planner_order_payload(payload: dict[str, Any]) -> dict[str, Any]:
    return {
        key: payload[key]
        for key in ("artifact_type", "round", "round_goal", "plan_graph")
        if key in payload
    }


def _with_forced_fields(payload: dict[str, Any], forced: dict[str, Any]) -> dict[str, Any]:
    merged = dict(payload)
    merged.update(forced)
    return merged


def _mock_planner_order_payload(
    agent_workflow: "AgentWorkflowSpec",
    request: str,
    *,
    round_number: int = 1,
    repo_intelligence: dict[str, Any] | None = None,
) -> dict[str, Any]:
    executors = [
        agent
        for agent in agent_workflow.agents
        if agent.id != agent_workflow.primary_planner_id
    ]
    repo_hint = _repo_intelligence_hint(repo_intelligence)
    return {
        "artifact_type": "planner_order",
        "round": round_number,
        "round_goal": request,
        "plan_graph": {
            "work_items": [
                {
                    "work_item_id": f"{_safe_id(agent.id)}-work",
                    "merge_index": index,
                    "assignee_agent_id": agent.id,
                    "task_summary": f"Mock task for {agent.name or agent.id}. {repo_hint}".strip(),
                    "depends_on": [],
                }
                for index, agent in enumerate(executors, start=1)
            ],
        },
    }

def _safe_id(value: str) -> str:
    safe = "".join(char if char.isalnum() or char == "_" else "_" for char in value.strip())
    return safe or "agent"


def _repo_intelligence_hint(repo_intelligence: dict[str, Any] | None) -> str:
    if not repo_intelligence:
        return ""
    repo_index = repo_intelligence.get("repo_index") if isinstance(repo_intelligence.get("repo_index"), dict) else {}
    command_discovery = (
        repo_intelligence.get("command_discovery")
        if isinstance(repo_intelligence.get("command_discovery"), dict)
        else {}
    )
    important = [str(item) for item in repo_index.get("important_files", [])][:2]
    commands = command_discovery.get("test_commands") if isinstance(command_discovery.get("test_commands"), list) else []
    command = ""
    if commands and isinstance(commands[0], dict):
        command = str(commands[0].get("command") or "")
    details = []
    if important:
        details.append(f"Use repo intelligence files: {', '.join(important)}.")
    if command:
        details.append(f"Discovered check command: {command}.")
    return " ".join(details)
