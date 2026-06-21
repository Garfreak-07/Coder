from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from coder_workbench.actions import ActionGateway, ActionSpec, RunContext
from coder_workbench.agent_graph.artifacts import AgentGraphArtifactRecorder, graph_artifact_id
from coder_workbench.agent_graph.agent_run import AgentRun
from coder_workbench.agent_graph.cache import GraphRunCache
from coder_workbench.agent_graph.context import upstream_refs_for_item
from coder_workbench.agent_graph.effects import apply_hidden_effects
from coder_workbench.agent_graph.evaluation import (
    build_agent_evaluation_reports,
    build_skill_evaluation_reports,
)
from coder_workbench.agent_graph.interruption import build_graph_interrupt, should_interrupt_execution
from coder_workbench.agent_graph.memory import PlannerMemoryStore
from coder_workbench.agent_graph.merge import build_planner_input_bundle, build_round_summary
from coder_workbench.agent_graph.round_budget import evaluate_round_budget_preflight
from coder_workbench.agent_graph.scheduler import AgentGraphScheduler, ReadyWave
from coder_workbench.agent_graph.schema import (
    FinalTestRecord,
    ExecutionRecord,
    PlannerInputBundle,
    PlannerOrder,
    PlanRunSummary,
    TestRecord,
    WorkItemOutcome,
)
from coder_workbench.agent_graph.validation import assert_valid_planner_order
from coder_workbench.core import (
    AgentWorkflowSpec,
    AgentWorkflowValidationError,
    compile_runtime_profiles,
    assert_valid_agent_workflow,
)
from coder_workbench.core.authority import authority_profile_for_agent
from coder_workbench.budget import BudgetBroker, BudgetLimit
from coder_workbench.coding import (
    build_debug_finding,
    build_repo_intelligence,
    build_run_coding_eval,
)
from coder_workbench.observability import TraceContext, TraceSpan
from coder_workbench.runtime.state import RunEvent, RunResult, summarize_value
from coder_workbench.runtime_kernel import RunController, RunGuard
from coder_workbench.skills import (
    InstalledSkillStore,
    SkillIndex,
    build_skill_index,
)


@dataclass
class RoundOutcome:
    round: int
    planner_order: PlannerOrder
    planner_input_bundle: PlannerInputBundle
    round_summary: PlanRunSummary
    planner_decision: dict[str, Any]
    interrupted: bool = False


class AgentGraphRuntimeError(ValueError):
    def __init__(self, message: str, *, status_code: str) -> None:
        self.status_code = status_code
        super().__init__(message)


class AgentGraphBlocked(ValueError):
    def __init__(self, message: str, *, status_code: str) -> None:
        self.status_code = status_code
        super().__init__(message)


class AgentGraphRunner:
    """AgentWorkflow runtime boundary.

    This runner deliberately does not compile AgentWorkflowSpec into WorkflowSpec.
    The first implementation uses deterministic mock records while preserving
    the real PlanGraph/cache/task-envelope data flow.
    """

    def __init__(
        self,
        agent_workflow: AgentWorkflowSpec,
        *,
        event_sink: Any | None = None,
        runtime_settings: Any | None = None,
        executor: Any | None = None,
        agent_run: Any | None = None,
    ) -> None:
        self.agent_workflow = agent_workflow
        self.event_sink = event_sink
        self.runtime_settings = runtime_settings
        if executor is not None and agent_run is not None:
            raise ValueError("Pass either executor or agent_run, not both")
        self.agent_run = agent_run or (_LegacyAgentRunAdapter(executor) if executor is not None else AgentRun(
            agent_workflow,
            runtime_settings=runtime_settings,
        ))
        self.budget_broker = BudgetBroker()
        self.action_gateway = ActionGateway(budget_broker=self.budget_broker)

    def run(
        self,
        request: str,
        repo_root: str,
        initial_data: dict[str, Any] | None = None,
        resume_checkpoint: dict[str, Any] | None = None,
        prior_events: list[RunEvent] | None = None,
        resume_after_node: str | None = None,
    ) -> RunResult:
        events = list(prior_events or [])
        data = dict(initial_data or {})
        artifacts: dict[str, Any] = {}
        blocked_node_id = None
        result_resume_checkpoint = None
        status = "completed"
        status_reason = None
        status_code = None
        run_id = str(data.get("run_id") or self.agent_workflow.id)
        data["run_id"] = run_id
        trace = TraceContext(trace_id=str(data.get("trace_id") or "") or None)
        run_span = trace.start_span(
            name=f"run:{self.agent_workflow.id}",
            kind="run",
            workflow_id=self.agent_workflow.id,
            run_id=run_id,
        )
        data["trace_id"] = trace.trace_id
        controller: RunController | None = None
        self.budget_broker = BudgetBroker(_budget_limit_from_data(data))
        self.action_gateway = ActionGateway(budget_broker=self.budget_broker)
        if hasattr(self.agent_run, "budget_broker"):
            self.agent_run.budget_broker = self.budget_broker
        if hasattr(self.agent_run, "action_gateway"):
            self.agent_run.action_gateway = self.action_gateway
        if hasattr(self.agent_run, "run_id"):
            self.agent_run.run_id = run_id
        if hasattr(self.agent_run, "initial_data"):
            self.agent_run.initial_data = data

        def emit(event_type: str, message: str, **payload: Any) -> None:
            span = payload.pop("_span", None) or run_span
            if isinstance(span, TraceSpan):
                payload = {**span.event_payload(), **payload}
            event = RunEvent(type=event_type, message=message, payload=payload)
            events.append(event)
            if self.event_sink:
                self.event_sink(event)

        def finalize_result(
            *,
            final_status: str,
            blocked_node_id: str | None = None,
            resume_checkpoint: dict[str, Any] | None = None,
            status_reason: str | None = None,
            status_code: str | None = None,
        ) -> RunResult:
            trace_status = "ok" if final_status == "completed" else final_status
            trace.finish_span(run_span, trace_status)  # type: ignore[arg-type]
            data["trace_spans"] = trace.spans_payload()
            data["budget_usage"] = self.budget_broker.usage(run_id).__dict__
            data["budget_reservations"] = self.budget_broker.reservations(run_id)
            data["budget_diagnostics"] = self.budget_broker.diagnostics(run_id)
            if controller is not None:
                data["run_controller"] = controller.diagnostics()
            return self._result(
                status=final_status,
                data=data,
                artifacts=artifacts,
                events=events,
                blocked_node_id=blocked_node_id,
                resume_checkpoint=resume_checkpoint,
                status_reason=status_reason,
                status_code=status_code,
            )

        recorder = AgentGraphArtifactRecorder(artifacts, emit)

        try:
            if resume_after_node:
                raise ValueError("AgentGraphRunner resume_after_node is not supported")

            assert_valid_agent_workflow(self.agent_workflow)
            workflow_payload = self.agent_workflow.model_dump(mode="json", by_alias=True, exclude_none=True)
            data["agent_workflow"] = workflow_payload
            data["runtime_profiles"] = [
                profile.model_dump(mode="json")
                for profile in compile_runtime_profiles(self.agent_workflow)
            ]
            skill_index = _skill_index_from_data_or_repo(data, repo_root)
            data["skill_index"] = skill_index.model_dump(mode="json")
            skill_store_root = _skill_store_root_from_data_or_repo(data, repo_root)
            data["skill_store_root"] = str(skill_store_root)
            repo_intelligence = _repo_intelligence_from_data_or_repo(data, repo_root)
            data["repo_intelligence"] = repo_intelligence

            emit(
                "agent_graph.run.started",
                f"Agent graph {self.agent_workflow.id} started",
                workflow_id=self.agent_workflow.id,
                repo_root=repo_root,
                request=request,
            )
            emit(
                "skill.index.available",
                "Installed SkillIndex loaded",
                skills=len(skill_index.skills),
                enabled_skills=len(skill_index.enabled()),
            )
            emit(
                "repo_intelligence.available",
                "Repository intelligence loaded",
                languages=repo_intelligence.get("repo_index", {}).get("languages", []),
                frameworks=repo_intelligence.get("repo_index", {}).get("frameworks", []),
                test_commands=len(repo_intelligence.get("command_discovery", {}).get("test_commands", [])),
                symbol_files=len(repo_intelligence.get("symbol_index", {}).get("files", [])),
            )
            max_rounds = _max_auto_rounds_from_workflow_or_data(self.agent_workflow, data)
            controller = RunController(guard=_run_guard_from_data(data, max_rounds=max_rounds))
            previous_bundle: PlannerInputBundle | None = None
            previous_round_summary: dict[str, Any] | None = None
            planner_human_response = data.get("planner_human_response") if isinstance(data.get("planner_human_response"), dict) else None
            start_round = 1
            round_request = request

            if data.get("resume_mode") == "planner_response":
                previous_bundle = self._planner_input_bundle_from_data(data.get("planner_input_bundle"))
                if previous_bundle is None:
                    raise ValueError("resume_mode=planner_response requires planner_input_bundle")
                previous_round_summary = self._round_summary_from_data(data.get("round_summary"))
                resume_decision = self.agent_run.run_planner_decision(
                    bundle=previous_bundle,
                    planner_human_response=planner_human_response,
                    emit=emit,
                )
                resume_decision_ref = graph_artifact_id("planner_decision", "resume", "round", previous_bundle.round)
                data["planner_decision"] = recorder.record(
                    resume_decision_ref,
                    resume_decision,
                    expected_type="planner_decision",
                )
                emit(
                    "planner.decision.produced",
                    "Planner decision produced",
                    artifact_type="planner_decision",
                    artifact_id=resume_decision_ref,
                    round=previous_bundle.round,
                    next_action=data["planner_decision"]["next_action"],
                )
                data.pop("resume_mode", None)
                controller.record_round(round_number=previous_bundle.round)
                controller_decision = controller.evaluate_planner_decision(
                    data["planner_decision"],
                    round_number=previous_bundle.round,
                )
                action = controller_decision.action
                if action == "ask_human":
                    status, status_reason, status_code, blocked_node_id, result_resume_checkpoint = self._block_for_planner_human(
                        data=data,
                        decision=data["planner_decision"],
                        emit=emit,
                        round_number=previous_bundle.round,
                    )
                    return finalize_result(
                        final_status=status,
                        blocked_node_id=blocked_node_id,
                        resume_checkpoint=result_resume_checkpoint,
                        status_reason=status_reason,
                        status_code=status_code,
                    )
                if action == "blocked":
                    status, status_reason, status_code, blocked_node_id, result_resume_checkpoint = self._block_for_controller(
                        data=data,
                        decision=controller_decision,
                        emit=emit,
                    )
                    return finalize_result(
                        final_status=status,
                        blocked_node_id=blocked_node_id,
                        resume_checkpoint=result_resume_checkpoint,
                        status_reason=status_reason,
                        status_code=status_code,
                    )
                if action in {"finish", "stop"}:
                    emit("agent_graph.run.completed", f"Agent graph {self.agent_workflow.id} completed")
                    return finalize_result(final_status="completed")
                round_request = data["planner_decision"].get("next_round_goal") or request
                start_round = previous_bundle.round + 1

            for round_number in range(start_round, max_rounds + 1):
                outcome = self._run_round(
                    round_number=round_number,
                    request=round_request,
                    repo_root=repo_root,
                    data=data,
                    recorder=recorder,
                    emit=emit,
                    previous_bundle=previous_bundle,
                    previous_round_summary=previous_round_summary,
                    planner_human_response=planner_human_response if round_number == start_round else None,
                    skill_index=skill_index,
                    skill_store_root=skill_store_root,
                    trace_context=trace,
                    parent_span=run_span,
                    controller=controller,
                )
                estimated_tokens_used = _estimated_tokens_used(data)
                controller.record_round(
                    outcome,
                    agent_calls=len(outcome.planner_input_bundle.items),
                    tool_calls=len(outcome.planner_input_bundle.effects),
                    estimated_tokens=max(0, estimated_tokens_used - controller.estimated_tokens),
                )
                controller_decision = controller.evaluate_planner_decision(
                    outcome.planner_decision,
                    round_number=round_number,
                )
                action = controller_decision.action
                if action == "continue":
                    previous_bundle = outcome.planner_input_bundle
                    previous_round_summary = outcome.round_summary.model_dump(mode="json")
                    round_request = outcome.planner_decision.get("next_round_goal") or request
                    planner_human_response = None
                    continue
                if action == "ask_human":
                    status, status_reason, status_code, blocked_node_id, result_resume_checkpoint = self._block_for_planner_human(
                        data=data,
                        decision=outcome.planner_decision,
                        emit=emit,
                        round_number=round_number,
                    )
                    break
                if action == "blocked":
                    status, status_reason, status_code, blocked_node_id, result_resume_checkpoint = self._block_for_controller(
                        data=data,
                        decision=controller_decision,
                        emit=emit,
                    )
                    break
                emit("agent_graph.run.completed", f"Agent graph {self.agent_workflow.id} completed")
                status = "completed"
                status_reason = None
                status_code = None
                break
            else:
                prompt = "Agent graph stopped because max_auto_rounds was reached."
                data["planner_human_prompt"] = prompt
                emit(
                    "agent_graph.run.blocked",
                    "Agent graph blocked after reaching max_auto_rounds",
                    code="max_auto_rounds_reached",
                )
                status = "blocked"
                status_reason = prompt
                status_code = "max_auto_rounds_reached"
                blocked_node_id = self.agent_workflow.primary_planner_id
                result_resume_checkpoint = {"data": data}
        except AgentGraphBlocked as exc:
            status = "blocked"
            status_reason = str(exc)
            status_code = str(getattr(exc, "status_code", "agent_graph_blocked"))
            blocked_node_id = self.agent_workflow.primary_planner_id
            result_resume_checkpoint = {"data": data}
        except Exception as exc:  # pragma: no cover - boundary safety
            status = "failed"
            status_reason = str(exc)
            status_code = str(getattr(exc, "status_code", "agent_graph_runtime_exception"))
            emit("agent_graph.run.failed", f"Agent graph failed: {exc}", error=str(exc))

        return finalize_result(
            final_status=status,
            blocked_node_id=blocked_node_id,
            resume_checkpoint=result_resume_checkpoint,
            status_reason=status_reason,
            status_code=status_code,
        )

    def _run_round(
        self,
        *,
        round_number: int,
        request: str,
        repo_root: str,
        data: dict[str, Any],
        recorder: AgentGraphArtifactRecorder,
        emit: Any,
        previous_bundle: PlannerInputBundle | None,
        previous_round_summary: dict[str, Any] | None,
        planner_human_response: dict[str, Any] | None,
        skill_index: SkillIndex,
        skill_store_root: Path,
        trace_context: TraceContext,
        parent_span: TraceSpan,
        controller: RunController,
    ) -> RoundOutcome:
        round_span = trace_context.start_span(
            name=f"round:{round_number}",
            kind="round",
            parent=parent_span,
            round=round_number,
        )
        emit(
            "agent_graph.round.started",
            f"Agent graph round {round_number} started",
            workflow_id=self.agent_workflow.id,
            round=round_number,
            primary_planner_id=self.agent_workflow.primary_planner_id,
            _span=round_span,
        )

        cache = GraphRunCache(round=round_number, skill_index=skill_index.model_dump(mode="json"))
        repo_intelligence = _repo_intelligence_from_data_or_repo(data, repo_root)
        planner_order = (
            self._planner_order_from_initial_data(data)
            if round_number == 1 and previous_bundle is None
            else None
        ) or self._create_planner_order(
            request,
            previous_bundle=previous_bundle,
            previous_round_summary=previous_round_summary,
            planner_human_response=planner_human_response,
            skill_index=skill_index,
            repo_intelligence=repo_intelligence,
            round_number=round_number,
            emit=emit,
        )
        try:
            assert_valid_planner_order(self.agent_workflow, planner_order)
        except AgentWorkflowValidationError as exc:
            raise AgentGraphRuntimeError(
                f"PlannerOrder graph validation failed: {exc}",
                status_code="planner_order_validation_failed",
            ) from exc
        planner_order_ref = graph_artifact_id("planner_order", "round", round_number)
        data["planner_order"] = planner_order.model_dump(mode="json", exclude_none=True)
        emit(
            "planner.order.produced",
            "Planner produced a PlanGraph",
            artifact_type="planner_order",
            artifact_id=planner_order_ref,
            round=round_number,
            planner_order=data["planner_order"],
        )
        data["planner_order"] = recorder.record(
            planner_order_ref,
            data["planner_order"],
            expected_type="planner_order",
        )
        plan_cache = cache.cache_planner_order(planner_order, planner_order_ref)
        emit(
            "planner.plan_cached",
            "Planner order stored in the graph run cache",
            round=round_number,
            work_items=len(plan_cache.work_items),
        )
        preflight_payload, preflight_decision = evaluate_round_budget_preflight(
            broker=self.budget_broker,
            controller=controller,
            run_id=str(data.get("run_id") or self.agent_workflow.id),
            planner_order=planner_order,
            estimated_model_calls=_round_preflight_model_calls(
                planner_order,
                runtime_settings=self.runtime_settings,
                data=data,
            ),
            estimated_tool_calls=_round_preflight_tool_calls(data),
            estimated_context_tokens_per_call=_round_preflight_context_tokens_per_call(data),
        )
        data.setdefault("budget_preflight", []).append(preflight_payload)
        data["budget_preflight_latest"] = preflight_payload
        emit(
            "budget.preflight.checked",
            "Round budget preflight checked",
            round=round_number,
            approved=bool(preflight_payload.get("approved")),
            reason=str(preflight_payload.get("reason") or ""),
            estimated_contexts=preflight_payload.get("estimated_contexts"),
            estimated_model_calls=preflight_payload.get("estimated_model_calls"),
            estimated_tool_calls=preflight_payload.get("estimated_tool_calls"),
            remaining=preflight_payload.get("remaining"),
        )
        if preflight_decision.action == "blocked":
            status_code = preflight_decision.status_code or "round_budget_preflight_denied"
            emit(
                "agent_graph.run.blocked",
                "Agent graph blocked by budget preflight",
                code=status_code,
                round=round_number,
                budget_preflight=preflight_payload,
            )
            raise AgentGraphBlocked(preflight_decision.reason, status_code=status_code)

        scheduler = AgentGraphScheduler(
            planner_order.plan_graph.work_items,
            max_concurrency=_max_concurrency_from_data(data),
        )
        while scheduler.has_pending():
            stop_after_current_wave = False
            for blocked in scheduler.block_items_with_failed_upstreams():
                item = blocked.work_item
                scheduler.mark_blocked(item.work_item_id)
                execution = cache.record_execution(
                    ExecutionRecord(
                        work_item_id=item.work_item_id,
                        merge_index=item.merge_index,
                        agent_id=item.assignee_agent_id,
                        status="blocked",
                        execution_summary=f"Blocked by failed upstream work item(s): {', '.join(blocked.blocked_by)}.",
                        execution_result_ref=graph_artifact_id("execution_result", item.work_item_id),
                    )
                )
                self._record_execution_artifact(recorder, cache.round, execution)
                emit(
                    "agent_task.blocked",
                    f"Task {item.work_item_id} blocked by upstream failure",
                    round=round_number,
                    work_item_id=item.work_item_id,
                    blocked_by=blocked.blocked_by,
                )

            wave = scheduler.next_wave()
            if wave.deferred_ready_work_item_ids:
                emit(
                    "resource.deferred",
                    "Ready work items deferred by max_concurrency",
                    round=round_number,
                    wave_index=wave.wave_index,
                    deferred_work_item_ids=wave.deferred_ready_work_item_ids,
                    max_concurrency=scheduler.max_concurrency,
                )
            if not wave.items:
                waiting = scheduler.dependency_waiting_items()
                if waiting:
                    emit(
                        "join.waiting",
                        "Waiting for upstream work items",
                        round=round_number,
                        waiting_work_item_ids=[item.work_item_id for item in waiting],
                    )
                break

            emit(
                "agent_graph.wave.started",
                f"Agent graph wave {wave.wave_index} started",
                round=round_number,
                wave_index=wave.wave_index,
                ready_work_item_ids=wave.ready_work_item_ids,
                work_item_ids=[item.work_item_id for item in wave.items],
                deferred_ready_work_item_ids=wave.deferred_ready_work_item_ids,
                _span=trace_context.start_span(
                    name=f"wave:{round_number}:{wave.wave_index}",
                    kind="wave",
                    parent=round_span,
                    round=round_number,
                    wave_index=wave.wave_index,
                ),
            )
            wave_span = trace_context.spans[-1]
            task_contexts = []
            for item in wave.items:
                scheduler.mark_running(item.work_item_id)
                if item.depends_on:
                    emit(
                        "join.completed",
                        f"All upstream work items completed for {item.work_item_id}",
                        round=round_number,
                        work_item_id=item.work_item_id,
                        depends_on=item.depends_on,
                    )
                envelope = self._start_work_item(
                    cache,
                    item,
                    planner_order_ref,
                    emit,
                    request=request,
                    data=data,
                    skill_index=skill_index,
                    skill_store_root=skill_store_root,
                    run_id=str(data.get("run_id") or ""),
                    repo_root=repo_root,
                    repo_intelligence=repo_intelligence,
                    trace_context=trace_context,
                    parent_span=wave_span,
                )
                task_contexts.append({"item": item, "envelope": envelope})
            outcomes = self._run_wave(wave, task_contexts)
            for outcome in outcomes:
                execution = cache.record_execution(outcome.execution)
                execution_artifact = self._record_execution_artifact(recorder, cache.round, execution)
                if should_interrupt_execution(execution_artifact):
                    interrupt = build_graph_interrupt(
                        round_number=cache.round,
                        artifact=execution_artifact,
                        artifact_ref=execution.execution_result_ref,
                    )
                    recorded_interrupt = cache.record_interrupt(interrupt.model_dump(mode="json"))
                    stop_after_current_wave = True
                    emit(
                        "agent_graph.interrupt.requested",
                        "Worker requested Planner intervention",
                        round=cache.round,
                        work_item_id=recorded_interrupt.work_item_id,
                        merge_index=recorded_interrupt.merge_index,
                        blocker_type=recorded_interrupt.blocker_type,
                        planner_question=recorded_interrupt.planner_question,
                        artifact_ref=recorded_interrupt.artifact_ref,
                    )
                if execution.status == "completed":
                    emit(
                        "agent_task.completed",
                        f"Task {outcome.work_item_id} completed",
                        round=cache.round,
                        work_item_id=outcome.work_item_id,
                        execution_result_ref=execution.execution_result_ref,
                    )
                    scheduler.mark_completed(outcome.work_item_id)
                elif execution.status == "blocked":
                    emit(
                        "agent_task.blocked",
                        f"Task {outcome.work_item_id} blocked",
                        round=cache.round,
                        work_item_id=outcome.work_item_id,
                        execution_result_ref=execution.execution_result_ref,
                    )
                    scheduler.mark_blocked(outcome.work_item_id)
                else:
                    emit(
                        "agent_task.failed",
                        f"Task {outcome.work_item_id} failed",
                        round=cache.round,
                        work_item_id=outcome.work_item_id,
                        execution_result_ref=execution.execution_result_ref,
                    )
                    scheduler.mark_failed(outcome.work_item_id)
                for test in outcome.tests:
                    test_record = cache.record_test(test)
                    self._record_test_artifact(recorder, cache.round, test_record)
                    emit(
                        "test.local.completed",
                        f"Local test for {outcome.work_item_id} completed",
                        round=cache.round,
                        work_item_id=outcome.work_item_id,
                        tester_agent_id=test_record.tester_agent_id,
                        test_result_ref=test_record.test_result_ref,
                    )
            emit(
                "agent_graph.wave.completed",
                f"Agent graph wave {wave.wave_index} completed",
                round=round_number,
                wave_index=wave.wave_index,
                completed_work_item_ids=[
                    outcome.work_item_id
                    for outcome in outcomes
                    if outcome.execution.status == "completed"
                ],
                failed_work_item_ids=[
                    outcome.work_item_id for outcome in outcomes if outcome.execution.status == "failed"
                ],
                blocked_work_item_ids=[
                    outcome.work_item_id for outcome in outcomes if outcome.execution.status == "blocked"
                ],
            )
            if stop_after_current_wave:
                emit(
                    "agent_graph.interrupt.captured",
                    "Agent graph stopped scheduling new waves for Planner intervention",
                    round=cache.round,
                )
                break

        data["scheduler_status"] = dict(scheduler.status_by_id)
        hidden_effects = apply_hidden_effects(
            agent_workflow=self.agent_workflow,
            cache=cache,
            repo_root=repo_root,
            scopes=_scopes_from_data(data),
            data=data,
            action_gateway=self.action_gateway,
        )
        if hidden_effects:
            self._emit_hidden_effect_outputs(cache, hidden_effects, emit)
            self._record_hidden_effect_artifacts(cache, recorder, hidden_effects)
        debug_findings = self._record_debug_findings(cache, recorder, repo_root, emit)
        if debug_findings:
            data["debug_findings"] = debug_findings
        if cache.hidden_effects:
            data["hidden_effects"] = cache.hidden_effects

        final_tester_agent_id = planner_order.plan_graph.final_tester_agent_id
        if final_tester_agent_id and not cache.interrupts:
            pre_final_bundle = build_planner_input_bundle(cache)
            final_test = cache.record_final_test(
                self.agent_run.run_final_test(
                    bundle=pre_final_bundle,
                    final_tester_agent_id=final_tester_agent_id,
                    emit=emit,
                )
            )
            self._record_final_test_artifact(recorder, final_test)
            emit(
                "test.final.completed",
                "Final tester aggregation completed",
                round=cache.round,
                final_tester_agent_id=final_test.final_tester_agent_id,
                test_result_ref=final_test.final_test_result_ref,
                status=final_test.status,
            )
        data["graph_run_cache"] = cache.as_runtime_payload()
        data.setdefault("token_ledger", []).extend(cache.token_ledger)

        planner_input_bundle = build_planner_input_bundle(cache)
        planner_input_bundle_ref = graph_artifact_id("planner_input_bundle", "round", cache.round)
        data["planner_input_bundle"] = recorder.record(
            planner_input_bundle_ref,
            planner_input_bundle.model_dump(mode="json", exclude_none=True),
        )
        emit(
            "planner.input_bundle.created",
            "Compact PlannerInputBundle created",
            artifact_type="planner_input_bundle",
            artifact_id=planner_input_bundle_ref,
            round=round_number,
            items=len(planner_input_bundle.items),
            plan_status=planner_input_bundle.plan_status,
        )

        round_summary = build_round_summary(cache)
        round_summary_ref = graph_artifact_id("round_summary", "round", cache.round)
        data["round_summary"] = recorder.record(
            round_summary_ref,
            round_summary.model_dump(mode="json"),
            expected_type="round_summary",
        )
        emit(
            "round_summary.created",
            "Round summary created",
            artifact_type="round_summary",
            artifact_id=round_summary_ref,
            round=round_number,
            plan_status=round_summary.plan_status,
        )

        planner_decision = (
            self._planner_decision_from_initial_data(data)
            if round_number == 1 and previous_bundle is None
            else None
        ) or self.agent_run.run_planner_decision(
            bundle=planner_input_bundle,
            planner_human_response=planner_human_response,
            emit=emit,
        )
        planner_decision_ref = graph_artifact_id("planner_decision", "round", round_number)
        data["planner_decision"] = recorder.record(
            planner_decision_ref,
            planner_decision,
            expected_type="planner_decision",
        )
        emit(
            "planner.decision.produced",
            "Planner decision produced",
            artifact_type="planner_decision",
            artifact_id=planner_decision_ref,
            round=round_number,
            next_action=data["planner_decision"]["next_action"],
        )
        try:
            memory = PlannerMemoryStore(repo_root).record_round(
                workflow_id=self.agent_workflow.id,
                bundle=planner_input_bundle,
                round_summary=round_summary,
                planner_decision=data["planner_decision"],
            )
            data["planner_memory"] = {
                "workflow_id": memory.workflow_id,
                "updated_at": memory.updated_at,
                "planner_notes": len(memory.planner_notes),
                "common_blockers": len(memory.common_blockers),
            }
        except Exception as exc:  # pragma: no cover - memory should not block runtime
            data["planner_memory_error"] = str(exc)

        round_entry = {
            "round": round_number,
            "planner_order": planner_order_ref,
            "planner_input_bundle": planner_input_bundle_ref,
            "round_summary": round_summary_ref,
            "planner_decision": planner_decision_ref,
        }
        data.setdefault("rounds", []).append(round_entry)

        return RoundOutcome(
            round=round_number,
            planner_order=planner_order,
            planner_input_bundle=planner_input_bundle,
            round_summary=round_summary,
            planner_decision=data["planner_decision"],
            interrupted=bool(planner_input_bundle.interrupts),
        )

    def _result(
        self,
        *,
        status: str,
        data: dict[str, Any],
        artifacts: dict[str, Any],
        events: list[RunEvent],
        blocked_node_id: str | None = None,
        resume_checkpoint: dict[str, Any] | None = None,
        status_reason: str | None = None,
        status_code: str | None = None,
    ) -> RunResult:
        result_data = self._data_with_evaluation_reports(data, events)
        return RunResult(
            status=status,
            data=result_data,
            summaries={key: summarize_value(value) for key, value in result_data.items()},
            artifacts=artifacts,
            events=events,
            estimated_tokens_used=_estimated_tokens_used(data),
            agent_calls=0,
            tool_calls=0,
            blocked_node_id=blocked_node_id,
            resume_checkpoint=resume_checkpoint,
            status_reason=status_reason,
            status_code=status_code,
        )

    def _data_with_evaluation_reports(self, data: dict[str, Any], events: list[RunEvent]) -> dict[str, Any]:
        if "agent_evaluation_reports" in data and "skill_evaluation_reports" in data:
            return data
        output = dict(data)
        graph_run_cache = output.get("graph_run_cache")
        token_ledger = output.get("token_ledger")
        if not isinstance(graph_run_cache, dict):
            return output
        ledger = token_ledger if isinstance(token_ledger, list) else []
        output["agent_evaluation_reports"] = [
            report.model_dump(mode="json")
            for report in build_agent_evaluation_reports(
                workflow=self.agent_workflow,
                graph_run_cache=graph_run_cache,
                events=events,
                token_ledger=ledger,
            )
        ]
        output["skill_evaluation_reports"] = [
            report.model_dump(mode="json")
            for report in build_skill_evaluation_reports(
                graph_run_cache=graph_run_cache,
                token_ledger=ledger,
            )
        ]
        output.setdefault("coding_eval", build_run_coding_eval(output, events))
        return output

    def _block_for_planner_human(
        self,
        *,
        data: dict[str, Any],
        decision: dict[str, Any],
        emit: Any,
        round_number: int,
    ) -> tuple[str, str, str, str, dict[str, Any]]:
        prompt = (
            decision.get("human_message")
            or decision.get("reason")
            or "Planner needs user input."
        )
        data["planner_human_prompt"] = prompt
        emit(
            "planner.human_prompt",
            "Planner requested human input",
            round=round_number,
            prompt=prompt,
            status_code="planner_ask_human",
        )
        emit("agent_graph.run.blocked", "Agent graph blocked for Planner human prompt", code="planner_ask_human")
        return (
            "blocked",
            str(prompt),
            "planner_ask_human",
            self.agent_workflow.primary_planner_id,
            {"data": data},
        )

    def _block_for_controller(
        self,
        *,
        data: dict[str, Any],
        decision: Any,
        emit: Any,
    ) -> tuple[str, str, str, str, dict[str, Any]]:
        prompt = decision.reason or "RunController blocked the run."
        data["planner_human_prompt"] = prompt
        status_code = decision.status_code or "run_controller_blocked"
        emit(
            "agent_graph.run.blocked",
            "Agent graph blocked by RunController",
            code=status_code,
        )
        return (
            "blocked",
            str(prompt),
            status_code,
            self.agent_workflow.primary_planner_id,
            {"data": data},
        )

    def _start_work_item(
        self,
        cache: GraphRunCache,
        item: Any,
        planner_order_ref: str,
        emit: Any,
        request: str,
        data: dict[str, Any],
        skill_index: SkillIndex,
        skill_store_root: Path,
        run_id: str,
        repo_root: str,
        repo_intelligence: dict[str, Any],
        trace_context: TraceContext,
        parent_span: TraceSpan,
    ) -> Any:
        upstream_refs = upstream_refs_for_item(cache, item)
        agent_span = trace_context.start_span(
            name=f"agent_run:{item.work_item_id}",
            kind="agent_run",
            parent=parent_span,
            round=cache.round,
            work_item_id=item.work_item_id,
            agent_id=item.assignee_agent_id,
        )
        action_span = trace_context.start_span(
            name=f"action:build_context:{item.work_item_id}",
            kind="action",
            parent=agent_span,
            round=cache.round,
            work_item_id=item.work_item_id,
            action_type="build_context",
        )
        emit(
            "action.started",
            "ActionGateway build_context started",
            round=cache.round,
            work_item_id=item.work_item_id,
            action_type="build_context",
            _span=action_span,
        )
        action_result = self.action_gateway.run(
            ActionSpec(
                action_id=f"build_context:{cache.round}:{item.work_item_id}",
                action_type="build_context",
            ),
            run_context=RunContext(
                run_id=run_id or self.agent_workflow.id,
                repo_root=repo_root,
                scopes=_scopes_from_data(data),
                data=data,
                cache=cache,
                item=item,
                planner_order_ref=planner_order_ref,
                upstream_refs=upstream_refs,
                user_request=request,
                role=self._agent_role(item.assignee_agent_id),
                skill_index=skill_index,
                skill_store_root=skill_store_root,
                repo_intelligence=repo_intelligence,
                artifact_type=self._work_artifact_type(item.assignee_agent_id),
                emit=emit,
            ),
        )
        if action_result.status != "ok":
            trace_context.finish_span(action_span, "blocked" if action_result.status == "blocked" else "failed")
            emit(
                "action.blocked" if action_result.status == "blocked" else "action.failed",
                action_result.summary,
                round=cache.round,
                work_item_id=item.work_item_id,
                action_type="build_context",
                status=action_result.status,
                error_code=action_result.error_code,
                _span=action_span,
            )
            raise AgentGraphRuntimeError(
                action_result.summary or "build_context action failed",
                status_code=action_result.error_code or "build_context_failed",
            )
        trace_context.finish_span(action_span, "ok")
        emit(
            "action.completed",
            "ActionGateway build_context completed",
            round=cache.round,
            work_item_id=item.work_item_id,
            action_type="build_context",
            status=action_result.status,
            token_used=action_result.token_used,
            _span=action_span,
        )
        context = action_result.payload["context"]
        envelope = context.envelope
        route = context.skill_route
        packet = context.context_packet
        ledger_entry = context.token_ledger_entry
        coding_packet = context.coding_context_packet
        emit(
            "skill.route.selected",
            "ExtensionRouter selected skills for work item",
            round=cache.round,
            work_item_id=item.work_item_id,
            assigned_agent_id=item.assignee_agent_id,
            allowed_skill_ids=route.allowed_skill_ids,
            omitted_skill_ids=route.omitted_skill_ids,
            estimated_skill_tokens=route.estimated_skill_tokens,
            scores=route.scores,
            _span=action_span,
        )
        emit(
            "agent.context_packet_v2",
            "ContextPacketV2 prepared for work item",
            round=cache.round,
            work_item_id=item.work_item_id,
            packet=packet.model_dump(mode="json"),
            _span=action_span,
        )
        emit(
            "agent.coding_context_packet",
            "CodingContextPacket prepared for work item",
            round=cache.round,
            work_item_id=item.work_item_id,
            packet=coding_packet.model_dump(mode="json"),
            _span=action_span,
        )
        emit(
            "token.ledger.entry",
            "Token ledger entry recorded",
            round=cache.round,
            work_item_id=item.work_item_id,
            entry=ledger_entry.model_dump(mode="json"),
            _span=action_span,
        )
        emit(
            "agent_task.ready",
            f"Task {item.work_item_id} is ready",
            round=cache.round,
            work_item_id=item.work_item_id,
            assigned_agent_id=item.assignee_agent_id,
            merge_index=item.merge_index,
            _span=agent_span,
        )
        emit(
            "agent_task.started",
            f"Task {item.work_item_id} started",
            round=cache.round,
            work_item_id=item.work_item_id,
            envelope=envelope.model_dump(mode="json"),
            _span=agent_span,
        )
        return envelope

    def _agent_role(self, agent_id: str) -> str:
        for agent in self.agent_workflow.agents:
            if agent.id == agent_id:
                return agent.role
        return ""

    def _work_artifact_type(self, agent_id: str) -> str:
        for agent in self.agent_workflow.agents:
            if agent.id == agent_id:
                profile = authority_profile_for_agent(agent, primary_planner_id=self.agent_workflow.primary_planner_id)
                return "synthesis_artifact" if profile.authority == "synthesizer" else "execution_result"
        return "execution_result"

    def _run_wave(self, wave: ReadyWave, task_contexts: list[dict[str, Any]]) -> list[WorkItemOutcome]:
        outcomes: list[WorkItemOutcome] = []
        if not wave.items:
            return outcomes
        with ThreadPoolExecutor(max_workers=max(1, len(wave.items))) as pool:
            futures = {pool.submit(self._build_work_item_outcome, context): context for context in task_contexts}
            for future in as_completed(futures):
                item = futures[future]["item"]
                try:
                    outcomes.append(future.result())
                except Exception as exc:  # pragma: no cover - defensive boundary
                    outcomes.append(
                        WorkItemOutcome(
                            work_item_id=item.work_item_id,
                            merge_index=item.merge_index,
                            execution=ExecutionRecord(
                                work_item_id=item.work_item_id,
                                merge_index=item.merge_index,
                                agent_id=item.assignee_agent_id,
                                status="failed",
                                execution_summary=f"Work item failed: {exc}",
                                execution_result_ref=graph_artifact_id("execution_result", item.work_item_id),
                            ),
                            tests=[],
                        )
                    )
        return outcomes

    def _build_work_item_outcome(self, context: dict[str, Any]) -> WorkItemOutcome:
        item = context["item"]
        envelope = context["envelope"]
        execution = self.agent_run.run_execution(item=item, envelope=envelope)
        tests = []
        if execution.status == "completed":
            execution_artifact = dict(
                execution.artifact_payload
                or {
                    "artifact_type": execution.artifact_type,
                    "round": envelope.round,
                    "work_item_id": execution.work_item_id,
                    "merge_index": execution.merge_index,
                    "agent_id": execution.agent_id,
                    "status": execution.status,
                    "summary": execution.execution_summary,
                }
            )
            execution_artifact.setdefault("artifact_id", execution.execution_result_ref)
            tests = [
                self.agent_run.run_test(
                    item=item,
                    execution_artifact=execution_artifact,
                    tester_agent_id=tester_agent_id,
                )
                for tester_agent_id in item.tester_agent_ids
            ]
        return WorkItemOutcome(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            execution=execution,
            tests=tests,
        )

    def _record_execution_artifact(
        self,
        recorder: AgentGraphArtifactRecorder,
        round_number: int,
        execution: ExecutionRecord,
    ) -> dict[str, Any]:
        artifact_type = execution.artifact_type
        payload = execution.artifact_payload or {
            "artifact_type": artifact_type,
            "round": round_number,
            "work_item_id": execution.work_item_id,
            "merge_index": execution.merge_index,
            "agent_id": execution.agent_id,
            "status": execution.status,
            "summary": execution.execution_summary,
        }
        artifact_type = str(payload.get("artifact_type") or artifact_type)
        return recorder.record(
            execution.execution_result_ref,
            payload,
            expected_type=artifact_type,
        )

    def _record_test_artifact(
        self,
        recorder: AgentGraphArtifactRecorder,
        round_number: int,
        test: TestRecord,
    ) -> dict[str, Any]:
        payload = test.artifact_payload or {
            "artifact_type": "test_result",
            "round": round_number,
            "work_item_id": test.work_item_id,
            "merge_index": test.merge_index,
            "tester_agent_id": test.tester_agent_id,
            "status": test.status,
            "summary": test.test_summary,
        }
        return recorder.record(
            test.test_result_ref or graph_artifact_id("test_result", test.work_item_id, test.tester_agent_id),
            payload,
            expected_type="test_result",
        )

    def _record_final_test_artifact(
        self,
        recorder: AgentGraphArtifactRecorder,
        final_test: FinalTestRecord,
    ) -> dict[str, Any] | None:
        if not final_test.final_test_result_ref:
            return None
        payload = final_test.artifact_payload or {
            "artifact_type": "test_result",
            "round": final_test.round,
            "tester_agent_id": final_test.final_tester_agent_id,
            "status": final_test.status,
            "summary": final_test.summary,
        }
        return recorder.record(
            final_test.final_test_result_ref,
            payload,
            expected_type="test_result",
        )

    def _record_debug_findings(
        self,
        cache: GraphRunCache,
        recorder: AgentGraphArtifactRecorder,
        repo_root: str,
        emit: Any,
    ) -> list[dict[str, Any]]:
        findings: list[dict[str, Any]] = []
        for records in cache.test_cache.values():
            for test in records:
                if test.status != "fail":
                    continue
                artifact = test.artifact_payload or {}
                commands = artifact.get("check_commands") if isinstance(artifact.get("check_commands"), list) else []
                first_command = commands[0] if commands else {}
                command = first_command.get("command") if isinstance(first_command, dict) else first_command
                finding = build_debug_finding(
                    {
                        "artifact_type": "check_result",
                        "command": str(command or ""),
                        "status": "fail",
                        "summary": test.test_summary,
                        "output": test.test_summary,
                        "output_ref": test.test_result_ref or "",
                    },
                    work_item_id=test.work_item_id,
                    repo_root=repo_root,
                )
                findings.append(self._record_debug_finding(cache, recorder, finding.model_dump(mode="json"), emit))

        for effect in list(cache.hidden_effects):
            if effect.get("effect_type") != "optional_check_command":
                continue
            if effect.get("status") not in {"failed", "check_requires_planner_confirmation"}:
                continue
            output_ref = str(effect.get("output_ref") or "")
            output = cache.hidden_effect_outputs.get(output_ref, {}) if output_ref else {}
            status = "blocked" if effect.get("status") == "check_requires_planner_confirmation" else "fail"
            finding = build_debug_finding(
                {
                    "artifact_type": "check_result",
                    "command": str(effect.get("command") or ""),
                    "status": status,
                    "summary": str(effect.get("reason") or ""),
                    "output": str(output.get("output") or effect.get("reason") or ""),
                    "output_ref": output_ref,
                },
                work_item_id=str(effect.get("work_item_id") or ""),
                repo_root=repo_root,
            )
            findings.append(self._record_debug_finding(cache, recorder, finding.model_dump(mode="json"), emit))
        return findings

    def _record_debug_finding(
        self,
        cache: GraphRunCache,
        recorder: AgentGraphArtifactRecorder,
        finding: dict[str, Any],
        emit: Any,
    ) -> dict[str, Any]:
        ref = graph_artifact_id(
            "debug_finding",
            finding.get("work_item_id") or "round",
            len(cache.hidden_effects) + 1,
        )
        artifact = recorder.record(ref, finding)
        record = {
            "effect_type": "debug_finding",
            "status": "created",
            "work_item_id": finding.get("work_item_id"),
            "debug_finding_ref": ref,
            "failure_summary": finding.get("failure_summary"),
            "likely_files": finding.get("likely_files", []),
            "raw_output_ref": finding.get("raw_output_ref"),
        }
        cache.record_hidden_effect(record)
        emit(
            "debug.finding.created",
            "DebugFindingArtifact created",
            artifact_id=ref,
            work_item_id=finding.get("work_item_id"),
            failure_summary=finding.get("failure_summary"),
            likely_files=finding.get("likely_files", []),
        )
        return artifact

    def _record_hidden_effect_artifacts(
        self,
        cache: GraphRunCache,
        recorder: AgentGraphArtifactRecorder,
        effects: list[dict[str, Any]],
    ) -> None:
        for effect in effects:
            artifact_ref = str(effect.get("artifact_ref") or "")
            if not artifact_ref:
                continue
            output_ref = str(effect.get("output_ref") or effect.get("patch_ref") or "")
            output = cache.hidden_effect_outputs.get(output_ref, {}) if output_ref else {}
            recorder.record(artifact_ref, _hidden_effect_artifact_payload(effect, output))

    def _emit_hidden_effect_outputs(
        self,
        cache: GraphRunCache,
        effects: list[dict[str, Any]],
        emit: Any,
    ) -> None:
        effect_by_ref = {
            str(effect.get("output_ref") or effect.get("patch_ref")): effect
            for effect in effects
            if effect.get("output_ref") or effect.get("patch_ref")
        }
        for output_ref, output in cache.hidden_effect_outputs.items():
            effect = effect_by_ref.get(output_ref, {})
            tool = "propose_patch" if effect.get("effect_type") == "modify_files" else "run_check"
            emit(
                "tool.result",
                f"Hidden effect output {output_ref} recorded",
                tool=tool,
                tool_result_id=output_ref,
                result=output,
                result_summary=summarize_value(output),
                result_status=output.get("status") if isinstance(output, dict) else effect.get("status"),
                result_keys=sorted(output.keys()) if isinstance(output, dict) else None,
                result_size_chars=len(str(output)),
            )

    def _create_planner_order(
        self,
        request: str,
        *,
        previous_bundle: PlannerInputBundle | None,
        previous_round_summary: dict[str, Any] | None,
        planner_human_response: dict[str, Any] | None,
        skill_index: SkillIndex,
        repo_intelligence: dict[str, Any],
        round_number: int,
        emit: Any,
    ) -> PlannerOrder:
        return self.agent_run.run_planner_order(
            request,
            previous_bundle=previous_bundle,
            previous_round_summary=previous_round_summary,
            planner_human_response=planner_human_response,
            skill_index=skill_index,
            repo_intelligence=repo_intelligence,
            round_number=round_number,
            emit=emit,
        )

    def _planner_order_from_initial_data(self, data: dict[str, Any]) -> PlannerOrder | None:
        value = data.get("planner_order")
        if not isinstance(value, dict):
            return None
        payload = {
            key: value[key]
            for key in ("artifact_type", "round", "round_goal", "plan_graph")
            if key in value
        }
        return PlannerOrder.model_validate(payload)

    def _planner_input_bundle_from_data(self, value: Any) -> PlannerInputBundle | None:
        if not isinstance(value, dict):
            return None
        payload = {
            key: value[key]
            for key in PlannerInputBundle.model_fields
            if key in value
        }
        return PlannerInputBundle.model_validate(payload)

    def _round_summary_from_data(self, value: Any) -> dict[str, Any] | None:
        if not isinstance(value, dict):
            return None
        return {
            key: value[key]
            for key in PlanRunSummary.model_fields
            if key in value
        }

    def _planner_decision_from_initial_data(self, data: dict[str, Any]) -> dict[str, Any] | None:
        value = data.get("planner_decision")
        if not isinstance(value, dict):
            return None
        action = value.get("next_action")
        if action not in {"continue", "ask_human", "finish", "stop"}:
            raise ValueError("planner_decision.next_action must be continue, ask_human, finish, or stop")
        payload = {
            "artifact_type": "planner_decision",
            "round": int(value.get("round") or 1),
            "task_done": bool(value.get("task_done", action in {"finish", "stop"})),
            "next_action": action,
            "reason": str(value.get("reason") or ""),
        }
        if value.get("human_message") is not None:
            payload["human_message"] = str(value["human_message"])
        return payload


def _hidden_effect_artifact_payload(effect: dict[str, Any], output: dict[str, Any]) -> dict[str, Any]:
    action_type = str(effect.get("action_type") or "")
    if action_type == "propose_patch":
        return {
            "artifact_type": "patch_preview",
            "effect": effect,
            "status": output.get("status") or effect.get("status"),
            "summary": output.get("message") or effect.get("reason") or "",
            "preview": output,
        }
    if action_type == "apply_patch_sandbox":
        return {
            "artifact_type": "sandbox_apply",
            "effect": effect,
            "status": output.get("status") or effect.get("status"),
            "summary": output.get("message") or effect.get("reason") or "",
            "result": output,
        }
    if action_type == "run_command_sandbox":
        passed = bool(effect.get("passed"))
        status = "blocked" if effect.get("status") == "check_requires_planner_confirmation" else "pass" if passed else "fail"
        return {
            "artifact_type": "check_result",
            "effect": effect,
            "command": effect.get("command"),
            "status": status,
            "summary": effect.get("reason") or output.get("message") or output.get("output") or "",
            "output": output.get("output") or "",
            "output_ref": effect.get("output_ref"),
            "returncode": effect.get("returncode"),
        }
    return {
        "artifact_type": "hidden_effect",
        "effect": effect,
        "output": output,
    }


class _LegacyAgentRunAdapter:
    def __init__(self, legacy_runner: Any) -> None:
        self.legacy_runner = legacy_runner

    def run_planner_order(self, request: str, **kwargs: Any) -> PlannerOrder:
        core_kwargs = {
            "previous_bundle": kwargs.get("previous_bundle"),
            "previous_round_summary": kwargs.get("previous_round_summary"),
            "planner_human_response": kwargs.get("planner_human_response"),
            "round_number": kwargs.get("round_number", 1),
            "emit": kwargs.get("emit"),
        }
        try:
            return self.legacy_runner.create_planner_order(request, **kwargs)
        except TypeError:
            try:
                return self.legacy_runner.create_planner_order(request, **core_kwargs)
            except TypeError:
                return self.legacy_runner.create_planner_order(request, emit=kwargs.get("emit"))

    def run_execution(self, **kwargs: Any) -> ExecutionRecord:
        payload = {
            "item": kwargs["item"],
            "envelope": kwargs["envelope"],
        }
        if kwargs.get("emit") is not None:
            payload["emit"] = kwargs["emit"]
        return self.legacy_runner.create_execution_result(**payload)

    def run_test(self, **kwargs: Any) -> TestRecord:
        payload = {
            "item": kwargs["item"],
            "execution_artifact": kwargs["execution_artifact"],
            "tester_agent_id": kwargs["tester_agent_id"],
        }
        if kwargs.get("emit") is not None:
            payload["emit"] = kwargs["emit"]
        return self.legacy_runner.create_test_result(**payload)

    def run_final_test(self, **kwargs: Any) -> FinalTestRecord:
        payload = {
            "bundle": kwargs["bundle"],
            "final_tester_agent_id": kwargs["final_tester_agent_id"],
        }
        if kwargs.get("emit") is not None:
            payload["emit"] = kwargs["emit"]
        return self.legacy_runner.create_final_test_result(**payload)

    def run_planner_decision(self, **kwargs: Any) -> dict[str, Any]:
        payload = {
            "bundle": kwargs["bundle"],
            "planner_human_response": kwargs.get("planner_human_response"),
        }
        if kwargs.get("emit") is not None:
            payload["emit"] = kwargs["emit"]
        return self.legacy_runner.create_planner_decision(**payload)


def _max_concurrency_from_data(data: dict[str, Any]) -> int:
    value = data.get("max_concurrency")
    try:
        return max(1, int(value))
    except (TypeError, ValueError):
        return 4


def _max_auto_rounds_from_workflow_or_data(agent_workflow: AgentWorkflowSpec, data: dict[str, Any]) -> int:
    value = data.get("max_auto_rounds")
    if value is None:
        value = agent_workflow.loop_policy.max_auto_rounds
    try:
        return max(1, int(value))
    except (TypeError, ValueError):
        return 3


def _run_guard_from_data(data: dict[str, Any], *, max_rounds: int) -> RunGuard:
    guard_data = data.get("run_guard")
    values = dict(guard_data) if isinstance(guard_data, dict) else {}
    values.setdefault("max_rounds", max_rounds)
    return RunGuard.model_validate(values)


def _budget_limit_from_data(data: dict[str, Any]) -> BudgetLimit:
    budget_data = data.get("budget_limit")
    if isinstance(budget_data, dict):
        return BudgetLimit.model_validate(budget_data)
    return BudgetLimit()


def _round_preflight_model_calls(planner_order: PlannerOrder, *, runtime_settings: Any | None, data: dict[str, Any]) -> int:
    override = data.get("budget_preflight_model_calls")
    if override is not None:
        try:
            return max(0, int(override))
        except (TypeError, ValueError):
            return 0
    if not _live_model_enabled(runtime_settings):
        return 0
    work_items = list(planner_order.plan_graph.work_items)
    calls = len(work_items)
    calls += sum(len(item.tester_agent_ids) for item in work_items)
    if planner_order.plan_graph.final_tester_agent_id:
        calls += 1
    if not _replay_planner_decision_available(data):
        calls += 1
    return calls


def _round_preflight_tool_calls(data: dict[str, Any]) -> int:
    value = data.get("budget_preflight_tool_calls")
    try:
        return max(0, int(value or 0))
    except (TypeError, ValueError):
        return 0


def _round_preflight_context_tokens_per_call(data: dict[str, Any]) -> int:
    value = data.get("budget_preflight_context_tokens_per_call")
    try:
        return max(0, int(value or 0))
    except (TypeError, ValueError):
        return 0


def _replay_planner_decision_available(data: dict[str, Any]) -> bool:
    return str(data.get("planner_mode") or "").strip().lower() == "replay" and isinstance(data.get("planner_decision"), dict)


def _live_model_enabled(runtime_settings: Any | None) -> bool:
    try:
        if runtime_settings is not None:
            from coder_workbench.config import RuntimeConfig
            from coder_workbench.server.settings import resolve_settings_config

            values = resolve_settings_config(runtime_settings, None, None)
            return RuntimeConfig(
                provider=str(values["provider"]),
                model=str(values["model"]),
                api_key=values["api_key"],
                base_url=values["base_url"],
            ).has_llm_credentials
        from coder_workbench.config import load_runtime_config

        return load_runtime_config().has_llm_credentials
    except Exception:
        return False


def _scopes_from_data(data: dict[str, Any]) -> list[str]:
    scopes = data.get("scopes")
    if isinstance(scopes, list):
        return [str(scope) for scope in scopes if str(scope).strip()]
    if isinstance(scopes, str) and scopes.strip():
        return [scopes.strip()]
    return []


def _skill_index_from_data_or_repo(data: dict[str, Any], repo_root: str) -> SkillIndex:
    value = data.get("skill_index")
    if isinstance(value, dict):
        return SkillIndex.model_validate(value)
    if isinstance(value, list):
        return SkillIndex.model_validate({"skills": value})
    try:
        return build_skill_index(InstalledSkillStore(Path(repo_root) / ".coder").list_installed())
    except Exception:
        return SkillIndex()


def _skill_store_root_from_data_or_repo(data: dict[str, Any], repo_root: str) -> Path:
    value = data.get("skill_store_root")
    if isinstance(value, str) and value.strip():
        return Path(value)
    return Path(repo_root) / ".coder"


def _repo_intelligence_from_data_or_repo(data: dict[str, Any], repo_root: str) -> dict[str, Any]:
    value = data.get("repo_intelligence")
    if isinstance(value, dict):
        return value
    try:
        return build_repo_intelligence(repo_root)
    except Exception as exc:
        return {
            "repo_index": {
                "artifact_type": "repo_index",
                "languages": [],
                "frameworks": [],
                "source_dirs": [],
                "test_dirs": [],
                "important_files": [],
                "risk_files": [".env", ".git", ".coder"],
                "package_managers": [],
                "file_count": 0,
                "confidence": "low",
            },
            "command_discovery": {
                "artifact_type": "command_discovery",
                "test_commands": [],
                "build_commands": [],
                "lint_commands": [],
                "confidence": "low",
            },
            "risk_map": {
                "artifact_type": "risk_map",
                "risk_files": [".env", ".git", ".coder"],
                "items": [],
                "confidence": "low",
            },
            "symbol_index": {
                "artifact_type": "symbol_index",
                "files": [],
                "parser": "regex_fallback",
                "languages": [],
                "confidence": "low",
            },
            "error": str(exc),
        }


def _estimated_tokens_used(data: dict[str, Any]) -> int:
    entries = data.get("token_ledger")
    if not isinstance(entries, list):
        return 0
    total = 0
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        total += int(entry.get("estimated_input_tokens") or 0)
        total += int(entry.get("estimated_output_tokens") or 0)
    return total
