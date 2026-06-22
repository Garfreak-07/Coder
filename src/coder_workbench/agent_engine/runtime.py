from __future__ import annotations

from types import SimpleNamespace
from typing import TYPE_CHECKING, Any, Callable, Protocol

from pydantic import ValidationError

from coder_workbench.actions import ActionGateway, ActionSpec, RunContext
from coder_workbench.budget import BudgetBroker
from coder_workbench.config import RuntimeConfig, load_runtime_config
from coder_workbench.core import AgentWorkflowAgent
from coder_workbench.core.artifacts import ArtifactValidationError, validate_artifact
from coder_workbench.core.authority import authority_profile_for_agent
from coder_workbench.llm import create_chat_model
from coder_workbench.skills import estimate_tokens

if TYPE_CHECKING:
    from coder_workbench.agent_graph.schema import (
        AgentTaskEnvelope,
        ExecutionRecord,
        FinalTestRecord,
        PlannerInputBundle,
        PlannerOrder,
        TestRecord,
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
        planner_human_response: dict[str, Any] | None = None,
        skill_index: "SkillIndex | None" = None,
        repo_intelligence: dict[str, Any] | None = None,
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
                planner_human_response=planner_human_response,
                skill_index=skill_index,
                repo_intelligence=repo_intelligence,
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
        planner_human_response: dict[str, Any] | None = None,
        runtime_settings: Any | None = None,
        model_factory: ModelFactory = create_chat_model,
        budget_broker: BudgetBroker | None = None,
        action_gateway: ActionGateway | None = None,
        run_id: str | None = None,
        emit: EmitEvent | None = None,
    ) -> dict[str, Any]:
        from coder_workbench.agent_graph.prompts import build_planner_decision_prompt

        planner = _agent(agent_workflow, agent_workflow.primary_planner_id)
        has_interrupts = bool(bundle.interrupts)
        has_failed_tests = any(item.execution_status == "failed" or item.test_status == "fail" for item in bundle.items)
        has_blocked_work = any(item.execution_status == "blocked" or item.test_status == "blocked" for item in bundle.items)
        has_debug_findings = any(effect.get("effect_type") == "debug_finding" for effect in bundle.effects)
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
        mock_next_action = (
            "continue"
            if can_continue_from_interrupts or has_failed_tests or has_debug_findings or has_failed_runtime_actions
            else "ask_human"
            if has_interrupts or has_blocked_work or has_blocked_runtime_actions
            else "finish"
        )
        mock_reason = (
            "DebugFinding is inside the current RunContract; Planner will replan."
            if has_debug_findings
            else "Runtime action failed; Planner will replan inside the current RunContract."
            if has_failed_runtime_actions
            else "Runtime action requires approval before Planner can continue."
            if has_blocked_runtime_actions
            else "Worker requested Planner intervention."
            if has_interrupts
            else "Tests failed; Planner will replan inside the existing RunContract."
            if has_failed_tests
            else "Work is blocked and requires Planner or user judgment."
            if has_blocked_work
            else (
                "Planner human response recorded; AgentGraph resume completed."
                if planner_human_response
                else "Mock AgentGraph execution and test artifacts are complete."
            )
        )
        return self._invoke_or_mock(
            artifact_type="planner_decision",
            agent_id=planner.id,
            prompt=build_planner_decision_prompt(
                planner=planner,
                bundle=bundle,
                planner_human_response=planner_human_response,
            ),
            mock_payload={
                "artifact_type": "planner_decision",
                "round": bundle.round,
                "task_done": mock_next_action == "finish",
                "next_action": mock_next_action,
                "risk_level": "medium"
                if has_interrupts or has_debug_findings or has_blocked_runtime_actions or has_failed_runtime_actions
                else "low",
                "requires_human_confirmation": mock_next_action == "ask_human",
                "reason": mock_reason,
                "next_round_goal": "Fix debug finding evidence and rerun checks." if has_debug_findings else "Replan around failed runtime action evidence." if has_failed_runtime_actions else "Fix failing test evidence and rerun checks." if has_failed_tests else "Resolve the blocked work item." if mock_next_action == "continue" else "",
                "remaining_auto_rounds": 2 if mock_next_action == "continue" else 0,
                "human_message": "Planner needs user input to resolve the blocked work item."
                if mock_next_action == "ask_human"
                else None,
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


class TesterEngine(ModelBackedEngine):
    id = "tester-engine"

    def run_test(
        self,
        *,
        agent_workflow: "AgentWorkflowSpec",
        item: "WorkItem",
        execution_artifact: dict[str, Any],
        tester_agent_id: str,
        runtime_settings: Any | None = None,
        model_factory: ModelFactory = create_chat_model,
        budget_broker: BudgetBroker | None = None,
        action_gateway: ActionGateway | None = None,
        run_id: str | None = None,
        emit: EmitEvent | None = None,
    ) -> "TestRecord":
        from coder_workbench.agent_graph.prompts import build_tester_prompt
        from coder_workbench.agent_graph.schema import TestRecord

        tester = _agent(agent_workflow, tester_agent_id)
        payload = self._invoke_or_mock(
            artifact_type="test_result",
            agent_id=tester.id,
            prompt=build_tester_prompt(tester=tester, item=item, execution_result=execution_artifact),
            mock_payload={
                "artifact_type": "test_result",
                "round": int(execution_artifact.get("round") or 1),
                "status": "pass",
                "summary": "Mock AgentGraph tester found no blocking issue.",
                "evidence": [str(execution_artifact.get("artifact_id") or "")],
                "issues": [],
                "remaining_work": [],
                "confidence": "medium",
                "check_commands": [],
                "check_outputs_ref": None,
            },
            emit=emit,
            agent_workflow=agent_workflow,
            runtime_settings=runtime_settings,
            model_factory=model_factory,
            budget_broker=budget_broker,
            action_gateway=action_gateway,
            run_id=run_id,
            fallback_payload=_blocked_test_payload(item, tester_agent_id, int(execution_artifact.get("round") or 1)),
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
        )
        round_number = int(execution_artifact.get("round") or payload.get("round") or 1)
        payload = _with_forced_fields(
            payload,
            {
                "artifact_type": "test_result",
                "round": round_number,
                "work_item_id": item.work_item_id,
                "merge_index": item.merge_index,
                "tester_agent_id": tester_agent_id,
            },
        )
        artifact = self._validate_payload(
            payload,
            artifact_type="test_result",
            agent_id=tester.id,
            emit=emit,
            fallback_payload=_blocked_test_payload(item, tester_agent_id, round_number),
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
        )
        return TestRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            tester_agent_id=tester_agent_id,
            status=artifact["status"],
            test_summary=artifact["summary"],
            test_result_ref=_artifact_ref("test_result", item.work_item_id, tester_agent_id),
            artifact_payload=artifact,
        )


class FinalReviewEngine(ModelBackedEngine):
    id = "final-review-engine"

    def run_final_test(
        self,
        *,
        agent_workflow: "AgentWorkflowSpec",
        bundle: "PlannerInputBundle",
        final_tester_agent_id: str,
        runtime_settings: Any | None = None,
        model_factory: ModelFactory = create_chat_model,
        budget_broker: BudgetBroker | None = None,
        action_gateway: ActionGateway | None = None,
        run_id: str | None = None,
        emit: EmitEvent | None = None,
    ) -> "FinalTestRecord":
        from coder_workbench.agent_graph.prompts import build_final_tester_prompt
        from coder_workbench.agent_graph.schema import FinalTestRecord

        final_tester = _agent(agent_workflow, final_tester_agent_id)
        fallback = {
            "artifact_type": "test_result",
            "round": bundle.round,
            "tester_agent_id": final_tester_agent_id,
            "status": "blocked",
            "summary": "Final tester output did not match test_result schema after one repair.",
            "remaining_work": ["schema_validation_failed"],
            "confidence": "low",
        }
        payload = self._invoke_or_mock(
            artifact_type="test_result",
            agent_id=final_tester.id,
            prompt=build_final_tester_prompt(final_tester=final_tester, bundle=bundle),
            mock_payload={
                "artifact_type": "test_result",
                "round": bundle.round,
                "tester_agent_id": final_tester_agent_id,
                "status": _aggregate_test_status(bundle),
                "summary": _aggregate_test_summary(bundle),
                "evidence": [ref for item in bundle.items for ref in item.refs],
                "issues": [],
                "remaining_work": _aggregate_remaining_work(bundle),
                "confidence": "medium",
                "check_commands": [],
                "check_outputs_ref": None,
            },
            emit=emit,
            agent_workflow=agent_workflow,
            runtime_settings=runtime_settings,
            model_factory=model_factory,
            budget_broker=budget_broker,
            action_gateway=action_gateway,
            run_id=run_id,
            fallback_payload=fallback,
        )
        payload = _with_forced_fields(
            payload,
            {
                "artifact_type": "test_result",
                "round": bundle.round,
                "tester_agent_id": final_tester_agent_id,
            },
        )
        artifact = self._validate_payload(
            payload,
            artifact_type="test_result",
            agent_id=final_tester.id,
            emit=emit,
            fallback_payload=fallback,
        )
        return FinalTestRecord(
            round=bundle.round,
            final_tester_agent_id=final_tester_agent_id,
            status=artifact["status"],
            summary=artifact["summary"],
            final_test_result_ref=_artifact_ref("test_result", "final", final_tester_agent_id),
            artifact_payload=artifact,
        )


class SynthesizerEngine(ModelBackedEngine):
    id = "synthesizer-engine"

    def run_execution(
        self,
        *,
        agent: AgentWorkflowAgent,
        item: "WorkItem",
        envelope: "AgentTaskEnvelope",
        model: Any | None = None,
        emit: Any | None = None,
    ) -> "ExecutionRecord":
        placeholder_workflow = SimpleNamespace(id="synthesizer-engine-run")
        return self.run_synthesis(
            agent_workflow=placeholder_workflow,
            agent=agent,
            item=item,
            envelope=envelope,
            model=model,
            emit=emit,
        )

    def run_synthesis(
        self,
        *,
        agent_workflow: "AgentWorkflowSpec",
        agent: AgentWorkflowAgent,
        item: "WorkItem",
        envelope: "AgentTaskEnvelope",
        model: Any | None = None,
        runtime_settings: Any | None = None,
        model_factory: ModelFactory = create_chat_model,
        budget_broker: BudgetBroker | None = None,
        action_gateway: ActionGateway | None = None,
        run_id: str | None = None,
        emit: EmitEvent | None = None,
    ) -> "ExecutionRecord":
        from coder_workbench.agent_graph.prompts import build_synthesis_prompt
        from coder_workbench.agent_graph.schema import ExecutionRecord
        from coder_workbench.agent_graph.synthesis import build_synthesis_artifact

        fallback = _blocked_synthesis_payload(item, envelope.round)
        payload = self._invoke_or_mock(
            artifact_type="synthesis_artifact",
            agent_id=agent.id,
            prompt=build_synthesis_prompt(agent=agent, item=item, envelope=envelope),
            mock_payload=build_synthesis_artifact(item=item, envelope=envelope, agent_id=agent.id),
            emit=emit,
            agent_workflow=agent_workflow,
            runtime_settings=runtime_settings,
            model_factory=model_factory,
            budget_broker=budget_broker,
            action_gateway=action_gateway,
            run_id=run_id,
            model=model,
            fallback_payload=fallback,
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
        )
        payload = _with_forced_fields(
            payload,
            {
                "artifact_type": "synthesis_artifact",
                "round": envelope.round,
                "work_item_id": item.work_item_id,
                "merge_index": item.merge_index,
                "agent_id": item.assignee_agent_id,
            },
        )
        artifact = self._validate_payload(
            payload,
            artifact_type="synthesis_artifact",
            agent_id=agent.id,
            emit=emit,
            fallback_payload=fallback,
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
        )
        return ExecutionRecord(
            artifact_type="synthesis_artifact",
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status=artifact["status"],
            execution_summary=artifact["summary"],
            execution_result_ref=_artifact_ref("synthesis_artifact", item.work_item_id),
            artifact_payload=artifact,
        )


class CodeWorkerEngine:
    id = "code-worker-engine"

    def run_execution(
        self,
        *,
        agent: AgentWorkflowAgent,
        item: "WorkItem",
        envelope: "AgentTaskEnvelope",
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
            prompt=build_worker_execution_prompt(agent=agent, item=item, envelope=envelope),
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


def _artifact_ref(artifact_type: str, *parts: Any) -> str:
    from coder_workbench.agent_graph.artifacts import graph_artifact_id

    return graph_artifact_id(artifact_type, *parts)


def _is_tester(agent: AgentWorkflowAgent) -> bool:
    return agent.role in {"tester", "reviewer"} or any("test" in capability for capability in agent.capabilities)


def _work_artifact_type(agent: AgentWorkflowAgent, primary_planner_id: str) -> str:
    profile = authority_profile_for_agent(agent, primary_planner_id=primary_planner_id)
    if profile.authority == "synthesizer":
        return "synthesis_artifact"
    return "execution_result"


def _mock_planner_order_payload(
    agent_workflow: "AgentWorkflowSpec",
    request: str,
    *,
    round_number: int = 1,
    repo_intelligence: dict[str, Any] | None = None,
) -> dict[str, Any]:
    testers = [agent for agent in agent_workflow.agents if _is_tester(agent)]
    workers = [
        agent
        for agent in agent_workflow.agents
        if agent.id != agent_workflow.primary_planner_id and not _is_tester(agent)
    ]
    tester_ids = [agent.id for agent in testers]
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
                    "tester_agent_ids": tester_ids,
                }
                for index, agent in enumerate(workers, start=1)
            ],
            "final_tester_agent_id": tester_ids[-1] if len(tester_ids) > 1 else None,
        },
    }


def _blocked_synthesis_payload(item: "WorkItem", round_number: int) -> dict[str, Any]:
    return {
        "artifact_type": "synthesis_artifact",
        "round": round_number,
        "work_item_id": item.work_item_id,
        "merge_index": item.merge_index,
        "agent_id": item.assignee_agent_id,
        "status": "blocked",
        "summary": "Synthesizer output did not match synthesis_artifact schema after one repair.",
        "sources": [],
        "deduplicated_source_ids": [],
        "clusters": [],
        "ranked_items": [],
        "compressed_summary": "",
        "index": {},
        "unexpected_issues": ["schema_validation_failed"],
        "needs_planner_decision": True,
        "blocker_type": "schema_validation_failed",
        "planner_question": "Synthesizer output failed schema validation. Should Planner retry, reassign, or ask the user?",
        "candidate_options": [],
        "continue_without_human_possible": False,
    }


def _blocked_test_payload(item: "WorkItem", tester_agent_id: str, round_number: int) -> dict[str, Any]:
    return {
        "artifact_type": "test_result",
        "round": round_number,
        "work_item_id": item.work_item_id,
        "merge_index": item.merge_index,
        "tester_agent_id": tester_agent_id,
        "status": "blocked",
        "summary": "Tester output did not match test_result schema after one repair.",
        "remaining_work": ["schema_validation_failed"],
        "confidence": "low",
    }


def _aggregate_test_status(bundle: "PlannerInputBundle") -> str:
    if any(item.execution_status == "blocked" or item.test_status == "blocked" for item in bundle.items):
        return "blocked"
    if any(item.execution_status == "failed" or item.test_status == "fail" for item in bundle.items):
        return "fail"
    return "pass"


def _aggregate_test_summary(bundle: "PlannerInputBundle") -> str:
    if not bundle.items:
        return "No work items required final aggregation."
    status = _aggregate_test_status(bundle)
    return f"Final tester aggregate status is {status} for {len(bundle.items)} work item(s)."


def _aggregate_remaining_work(bundle: "PlannerInputBundle") -> list[str]:
    return [
        item.test_summary or item.execution_summary or item.task_summary
        for item in bundle.items
        if item.execution_status in {"blocked", "failed"} or item.test_status in {"blocked", "fail"}
    ]


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
