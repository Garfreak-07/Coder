from __future__ import annotations

from typing import Any, Callable, Protocol

from pydantic import ValidationError

from coder_workbench.agent_graph.prompts import (
    build_planner_decision_prompt,
    build_planner_order_prompt,
    build_tester_prompt,
    build_worker_execution_prompt,
    schema_notes_for_artifact,
)
from coder_workbench.agent_graph.repair import build_repair_prompt, parse_json_object
from coder_workbench.agent_graph.schema import (
    AgentTaskEnvelope,
    ExecutionRecord,
    PlannerInputBundle,
    PlannerOrder,
    TestRecord,
    WorkItem,
)
from coder_workbench.config import RuntimeConfig, load_runtime_config
from coder_workbench.core import AgentWorkflowAgent, AgentWorkflowSpec
from coder_workbench.core.artifacts import ArtifactValidationError, validate_artifact
from coder_workbench.llm import create_chat_model


EmitEvent = Callable[..., None]
ModelFactory = Callable[[RuntimeConfig], Any]


class AgentGraphExecutorError(ValueError):
    def __init__(self, message: str, *, status_code: str) -> None:
        self.status_code = status_code
        super().__init__(message)


class AgentGraphExecutorProtocol(Protocol):
    def create_planner_order(self, request: str, *, emit: EmitEvent | None = None) -> PlannerOrder:
        ...

    def create_execution_result(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        emit: EmitEvent | None = None,
    ) -> ExecutionRecord:
        ...

    def create_test_result(
        self,
        *,
        item: WorkItem,
        execution_artifact: dict[str, Any],
        tester_agent_id: str,
        emit: EmitEvent | None = None,
    ) -> TestRecord:
        ...

    def create_planner_decision(
        self,
        *,
        bundle: PlannerInputBundle,
        planner_human_response: dict[str, Any] | None = None,
        emit: EmitEvent | None = None,
    ) -> dict[str, Any]:
        ...


class AgentGraphExecutor:
    def __init__(
        self,
        agent_workflow: AgentWorkflowSpec,
        *,
        runtime_settings: Any | None = None,
        model_factory: ModelFactory = create_chat_model,
    ) -> None:
        self.agent_workflow = agent_workflow
        self.runtime_settings = runtime_settings
        self.model_factory = model_factory

    def create_planner_order(self, request: str, *, emit: EmitEvent | None = None) -> PlannerOrder:
        payload = self._invoke_or_mock(
            artifact_type="planner_order",
            agent_id=self.agent_workflow.primary_planner_id,
            prompt=build_planner_order_prompt(request=request, agent_workflow=self.agent_workflow),
            mock_payload=self._mock_planner_order_payload(request),
            emit=emit,
            failure_status_code="planner_order_schema_failed",
        )
        try:
            return PlannerOrder.model_validate(_planner_order_payload(payload))
        except ValidationError as exc:
            raise AgentGraphExecutorError(
                f"planner_order failed AgentGraph schema validation: {exc}",
                status_code="planner_order_schema_failed",
            ) from exc

    def create_execution_result(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        emit: EmitEvent | None = None,
    ) -> ExecutionRecord:
        agent = self._agent(item.assignee_agent_id)
        payload = self._invoke_or_mock(
            artifact_type="execution_result",
            agent_id=agent.id,
            prompt=build_worker_execution_prompt(agent=agent, item=item, envelope=envelope),
            mock_payload={
                "artifact_type": "execution_result",
                "round": envelope.round,
                "status": "completed",
                "summary": "Mock AgentGraph worker completed a dry-run execution.",
                "changed_files": [],
                "created_files": [],
                "deleted_files": [],
                "patch_refs": [],
                "outputs": envelope.upstream_refs,
                "unexpected_issues": [],
                "out_of_contract": False,
                "needs_planner_decision": False,
                "tester_notes": ["No real file mutation was performed in mock mode."],
            },
            emit=emit,
            fallback_payload=self._blocked_execution_payload(item, envelope.round),
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
        )
        payload = _with_forced_fields(
            payload,
            {
                "artifact_type": "execution_result",
                "round": envelope.round,
                "work_item_id": item.work_item_id,
                "merge_index": item.merge_index,
                "agent_id": item.assignee_agent_id,
            },
        )
        artifact = self._validate_payload(
            payload,
            artifact_type="execution_result",
            agent_id=agent.id,
            emit=emit,
            fallback_payload=self._blocked_execution_payload(item, envelope.round),
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
        )
        return ExecutionRecord(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            agent_id=item.assignee_agent_id,
            status=artifact["status"],
            execution_summary=artifact["summary"],
            execution_result_ref=_artifact_ref("execution_result", item.work_item_id),
            artifact_payload=artifact,
        )

    def create_test_result(
        self,
        *,
        item: WorkItem,
        execution_artifact: dict[str, Any],
        tester_agent_id: str,
        emit: EmitEvent | None = None,
    ) -> TestRecord:
        tester = self._agent(tester_agent_id)
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
            fallback_payload=self._blocked_test_payload(item, tester_agent_id, int(execution_artifact.get("round") or 1)),
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
            fallback_payload=self._blocked_test_payload(item, tester_agent_id, round_number),
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

    def create_planner_decision(
        self,
        *,
        bundle: PlannerInputBundle,
        planner_human_response: dict[str, Any] | None = None,
        emit: EmitEvent | None = None,
    ) -> dict[str, Any]:
        planner = self._agent(self.agent_workflow.primary_planner_id)
        mock_reason = (
            "Planner human response recorded; AgentGraph resume completed."
            if planner_human_response
            else "Mock AgentGraph execution and test artifacts are complete."
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
                "task_done": True,
                "next_action": "finish",
                "risk_level": "low",
                "requires_human_confirmation": False,
                "reason": mock_reason,
                "next_round_goal": "",
                "remaining_auto_rounds": 0,
                "human_message": None,
            },
            emit=emit,
            failure_status_code="planner_decision_schema_failed",
        )

    def _invoke_or_mock(
        self,
        *,
        artifact_type: str,
        agent_id: str,
        prompt: str,
        mock_payload: dict[str, Any],
        emit: EmitEvent | None,
        fallback_payload: dict[str, Any] | None = None,
        failure_status_code: str | None = None,
        work_item_id: str | None = None,
        merge_index: int | None = None,
    ) -> dict[str, Any]:
        model = self._chat_model()
        if model is None:
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
        response = model.invoke(prompt)
        content = getattr(response, "content", str(response))
        payload = parse_json_object(str(content))
        if payload is None:
            payload = {"artifact_type": artifact_type}
            errors = [{"loc": ["response"], "msg": "model output was not a JSON object"}]
        else:
            errors = []

        artifact = self._validate_payload(
            payload,
            artifact_type=artifact_type,
            agent_id=agent_id,
            emit=emit,
            fallback_payload=None,
            failure_status_code=None,
            work_item_id=work_item_id,
            merge_index=merge_index,
            initial_errors=errors,
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

        repaired = self._repair_once(
            model,
            artifact_type=artifact_type,
            agent_id=agent_id,
            invalid_output=str(content),
            emit=emit,
            work_item_id=work_item_id,
            merge_index=merge_index,
        )
        if repaired is not None:
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
        raise AgentGraphExecutorError(
            f"{artifact_type} schema validation failed after one repair",
            status_code=failure_status_code or f"{artifact_type}_schema_failed",
        )

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
                raise AgentGraphExecutorError(
                    f"{artifact_type} failed schema validation",
                    status_code=failure_status_code,
                ) from exc
            return None

    def _repair_once(
        self,
        model: Any,
        *,
        artifact_type: str,
        agent_id: str,
        invalid_output: str,
        emit: EmitEvent | None,
        work_item_id: str | None,
        merge_index: int | None,
    ) -> dict[str, Any] | None:
        self._emit(
            emit,
            "agent_graph.agent_call.repair_started",
            "AgentGraph artifact repair started",
            agent_id=agent_id,
            artifact_type=artifact_type,
            work_item_id=work_item_id,
            merge_index=merge_index,
        )
        prompt = build_repair_prompt(
            expected_type=artifact_type,
            invalid_output=invalid_output,
            errors=[{"loc": ["response"], "msg": "schema validation failed"}],
            schema_notes=schema_notes_for_artifact(artifact_type),
        )
        response = model.invoke(prompt)
        payload = parse_json_object(str(getattr(response, "content", response)))
        if payload is None:
            self._emit(
                emit,
                "agent_graph.agent_call.repair_failed",
                "AgentGraph artifact repair failed",
                agent_id=agent_id,
                artifact_type=artifact_type,
                work_item_id=work_item_id,
                merge_index=merge_index,
            )
            return None
        repaired = self._validate_payload(
            payload,
            artifact_type=artifact_type,
            agent_id=agent_id,
            emit=emit,
            work_item_id=work_item_id,
            merge_index=merge_index,
        )
        if repaired is None:
            self._emit(
                emit,
                "agent_graph.agent_call.repair_failed",
                "AgentGraph artifact repair failed",
                agent_id=agent_id,
                artifact_type=artifact_type,
                work_item_id=work_item_id,
                merge_index=merge_index,
            )
            return None
        self._emit(
            emit,
            "agent_graph.agent_call.repair_completed",
            "AgentGraph artifact repair completed",
            agent_id=agent_id,
            artifact_type=artifact_type,
            work_item_id=work_item_id,
            merge_index=merge_index,
        )
        return repaired

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

    def _mock_planner_order_payload(self, request: str) -> dict[str, Any]:
        testers = [agent for agent in self.agent_workflow.agents if _is_tester(agent)]
        workers = [
            agent
            for agent in self.agent_workflow.agents
            if agent.id != self.agent_workflow.primary_planner_id and not _is_tester(agent)
        ]
        tester_ids = [agent.id for agent in testers]
        return {
            "artifact_type": "planner_order",
            "round": 1,
            "round_goal": request,
            "plan_graph": {
                "work_items": [
                    {
                        "work_item_id": f"{_safe_id(agent.id)}-work",
                        "merge_index": index,
                        "assignee_agent_id": agent.id,
                        "task_summary": f"Mock task for {agent.name or agent.id}.",
                        "depends_on": [],
                        "tester_agent_ids": tester_ids,
                    }
                    for index, agent in enumerate(workers, start=1)
                ],
                "final_tester_agent_id": tester_ids[-1] if len(tester_ids) > 1 else None,
            },
        }

    def _blocked_execution_payload(self, item: WorkItem, round_number: int) -> dict[str, Any]:
        return {
            "artifact_type": "execution_result",
            "round": round_number,
            "work_item_id": item.work_item_id,
            "merge_index": item.merge_index,
            "agent_id": item.assignee_agent_id,
            "status": "blocked",
            "summary": "Agent output did not match execution_result schema after one repair.",
            "unexpected_issues": ["schema_validation_failed"],
            "needs_planner_decision": True,
        }

    def _blocked_test_payload(self, item: WorkItem, tester_agent_id: str, round_number: int) -> dict[str, Any]:
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

    def _agent(self, agent_id: str) -> AgentWorkflowAgent:
        for agent in self.agent_workflow.agents:
            if agent.id == agent_id:
                return agent
        raise KeyError(agent_id)

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


def _safe_id(value: str) -> str:
    safe = "".join(char if char.isalnum() or char == "_" else "_" for char in value.strip())
    return safe or "agent"
