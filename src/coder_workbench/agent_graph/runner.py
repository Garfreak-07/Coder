from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from uuid import uuid4

from coder_workbench.actions import (
    ActionGateway,
    ActionSpec,
    RunContext,
    action_completed_payload,
    action_started_payload,
)
from coder_workbench.agent_graph.artifacts import AgentGraphArtifactRecorder, graph_artifact_id
from coder_workbench.agent_graph.agent_run import AgentRun
from coder_workbench.agent_graph.round_working_set import RoundWorkingSet
from coder_workbench.agent_graph.context import upstream_refs_for_item
from coder_workbench.agent_graph.effects import apply_hidden_effects
from coder_workbench.agent_graph.evaluation import (
    build_agent_evaluation_reports,
    build_skill_evaluation_reports,
)
from coder_workbench.agent_graph.final_report import build_final_report
from coder_workbench.agent_graph.interruption import build_graph_interrupt, should_interrupt_execution
from coder_workbench.agent_graph.merge import build_planner_input_bundle, build_round_summary
from coder_workbench.agent_graph.planner_strategy import POLICY_BLOCKER_TYPES
from coder_workbench.agent_graph.round_budget import evaluate_round_budget_preflight
from coder_workbench.agent_graph.scheduler import AgentGraphScheduler
from coder_workbench.agent_graph.schema import (
    ExecutionRecord,
    PlannerInputBundle,
    PlannerOrder,
    PlanRunSummary,
    WorkItemOutcome,
)
from coder_workbench.agent_graph.validation import assert_valid_planner_order
from coder_workbench.agent_graph.wave_executor import WaveExecutor, WorkItemRuntimePolicy
from coder_workbench.core import (
    AgentWorkflowSpec,
    AgentWorkflowValidationError,
    compile_runtime_profiles,
    assert_valid_agent_workflow,
)
from coder_workbench.core.artifacts import artifact_summary, validate_artifact
from coder_workbench.budget import BudgetBroker, BudgetLimit
from coder_workbench.coding import (
    build_debug_finding,
    build_repo_intelligence,
    build_run_coding_eval,
)
from coder_workbench.memory import MemoryService
from coder_workbench.observability import TraceContext, TraceSpan
from coder_workbench.runtime.state import RunEvent, RunResult, summarize_value
from coder_workbench.runtime_kernel import RunCancelled, RunControl, RunController, RunGuard
from coder_workbench.runtime_state import SharedRunState, StateUpdate, apply_state_update
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

    This runner executes AgentWorkflowSpec directly through AgentGraph data
    flow: PlannerOrder, RoundWorkingSet, AgentRun, and PlannerDecision.
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
        self.agent_run = agent_run or (_ExecutorAdapter(executor) if executor is not None else AgentRun(
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
        run_control: RunControl | None = None,
    ) -> RunResult:
        checkpoint_data = resume_checkpoint.get("data") if isinstance(resume_checkpoint, dict) else None
        data = dict(checkpoint_data) if isinstance(checkpoint_data, dict) else {}
        data.update(dict(initial_data or {}))
        events = list(prior_events or [])
        artifacts: dict[str, Any] = {}
        blocked_node_id = None
        result_resume_checkpoint = None
        status = "completed"
        status_reason = None
        status_code = None
        run_id = str(data.get("run_id") or self.agent_workflow.id)
        data["run_id"] = run_id
        data["repo_root"] = repo_root
        trace = TraceContext(trace_id=str(data.get("trace_id") or "") or None)
        run_span = trace.start_span(
            name=f"run:{self.agent_workflow.id}",
            kind="run",
            workflow_id=self.agent_workflow.id,
            run_id=run_id,
        )
        data["trace_id"] = trace.trace_id
        controller: RunController | None = None
        active_run_control = run_control or RunControl()
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

        shared_run_state = _shared_run_state_from_data(
            data,
            run_id=run_id,
            workflow_id=self.agent_workflow.id,
            request=request,
        )
        data["shared_run_state"] = shared_run_state.model_dump(mode="json")

        def record_state_update(
            channel: str,
            payload: dict[str, Any],
            *,
            source: str = "agent_graph_runner",
        ) -> SharedRunState:
            nonlocal shared_run_state
            update = StateUpdate(
                update_id=str(uuid4()),
                run_id=run_id,
                source=source,
                channel=channel,  # type: ignore[arg-type]
                payload=payload,
            )
            shared_run_state = apply_state_update(shared_run_state, update)
            data["shared_run_state"] = shared_run_state.model_dump(mode="json")
            summaries = data.setdefault("shared_run_state_update_summaries", [])
            if isinstance(summaries, list):
                summaries.append(
                    {
                        "update_id": update.update_id,
                        "channel": update.channel,
                        "source": update.source,
                        "keys": sorted(payload.keys()),
                    }
                )
            return shared_run_state

        def emit(event_type: str, message: str, **payload: Any) -> None:
            include_trace = bool(payload.pop("_trace", True))
            span = payload.pop("_span", None) or (run_span if include_trace else None)
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
            data["run_control"] = active_run_control.diagnostics()
            final_report_ref = graph_artifact_id("final_report")
            final_report = build_final_report(
                status=final_status,
                data=data,
                artifacts=artifacts,
                events=events,
                status_reason=status_reason,
                status_code=status_code,
            )
            data["final_report"] = recorder.record(
                final_report_ref,
                final_report,
                expected_type="final_report",
            )
            record_state_update("artifacts", _artifact_ref_payload(final_report_ref, data["final_report"]))
            record_state_update("final_report", {"artifact_id": final_report_ref})
            record_state_update(
                "control",
                {
                    "status": final_status,
                    "blocked_recovery_used": bool(data.get("blocked_recovery_used")),
                },
            )
            emit(
                "final_report.created",
                "Final report created",
                artifact_type="final_report",
                artifact_id=final_report_ref,
                status=data["final_report"]["status"],
                summary=data["final_report"]["summary"],
                evidence_count=len(data["final_report"].get("evidence_refs") or []),
                _trace=False,
            )
            if resume_checkpoint is not None:
                resume_checkpoint = _normalize_resume_checkpoint(
                    resume_checkpoint,
                    data=data,
                    events=events,
                    status_code=status_code,
                    phase=status_code or final_status,
                )
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

        def finish_values(
            planner_decision: dict[str, Any],
            controller_decision: Any,
        ) -> tuple[str, str | None, str | None, str | None]:
            final_status = (
                getattr(controller_decision, "final_status", None)
                or planner_decision.get("final_status")
                or "completed"
            )
            if final_status not in {"completed", "blocked", "failed", "cancelled"}:
                final_status = "completed"
            reason = getattr(controller_decision, "reason", None) or planner_decision.get("reason")
            status_code_value = getattr(controller_decision, "status_code", None)
            if final_status == "completed":
                return final_status, None, None, None
            return (
                final_status,
                str(reason or final_status),
                str(status_code_value or f"planner_{final_status}"),
                self.agent_workflow.primary_planner_id if final_status == "blocked" else None,
            )

        recorder = AgentGraphArtifactRecorder(artifacts, emit)

        try:
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
            record_state_update(
                "control",
                {
                    "status": "running",
                    "round": 0,
                    "blocked_recovery_used": bool(data.get("blocked_recovery_used")),
                },
            )
            active_run_control.checkpoint("run_started", emit)
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
            start_round = 1
            round_request = request

            blocked_recovery_used = bool(data.get("blocked_recovery_used"))
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
                    skill_index=skill_index,
                    skill_store_root=skill_store_root,
                    trace_context=trace,
                    parent_span=run_span,
                    controller=controller,
                    run_control=active_run_control,
                    record_state_update=record_state_update,
                )
                estimated_tokens_used = _estimated_tokens_used(data)
                controller.record_round(
                    outcome,
                    agent_calls=len(outcome.planner_input_bundle.items),
                    tool_calls=len(outcome.planner_input_bundle.effects),
                    estimated_tokens=max(0, estimated_tokens_used - controller.estimated_tokens),
                )
                round_has_blocked = any(
                    item.execution_status == "blocked"
                    for item in outcome.planner_input_bundle.items
                )
                blocked_replan_once = _blocked_recommends_replan_once(outcome, artifacts)
                blocked_progress_stop_reason = _blocked_progress_stop_reason(
                    outcome,
                    previous_bundle=previous_bundle,
                    artifacts=artifacts,
                )
                if round_has_blocked and outcome.planner_decision.get("next_action") == "continue":
                    if blocked_progress_stop_reason is None and blocked_replan_once and not blocked_recovery_used:
                        blocked_recovery_used = True
                        data["blocked_recovery_used"] = True
                        record_state_update(
                            "control",
                            {
                                "round": round_number,
                                "blocked_recovery_used": True,
                            },
                        )
                    else:
                        reason = blocked_progress_stop_reason or _blocked_recovery_summary(outcome)
                        forced_ref = graph_artifact_id("planner_decision", "blocked_recovery", "round", round_number)
                        forced_decision = recorder.record(
                            forced_ref,
                            {
                                "artifact_type": "planner_decision",
                                "round": round_number,
                                "task_done": False,
                                "next_action": "finish",
                                "final_status": "blocked",
                                "reason": reason,
                                "remaining_auto_rounds": 0,
                            },
                            expected_type="planner_decision",
                        )
                        data["planner_decision"] = forced_decision
                        record_state_update("artifacts", _artifact_ref_payload(forced_ref, forced_decision))
                        record_state_update("planner", {"planner_decision_ref": forced_ref})
                        if data.get("rounds") and isinstance(data["rounds"], list):
                            data["rounds"][-1]["planner_decision"] = forced_ref
                        emit(
                            "planner.decision.produced",
                            "Planner decision forced by blocked recovery policy",
                            artifact_type="planner_decision",
                            artifact_id=forced_ref,
                            round=round_number,
                            next_action="finish",
                            final_status="blocked",
                        )
                        outcome = RoundOutcome(
                            round=outcome.round,
                            planner_order=outcome.planner_order,
                            planner_input_bundle=outcome.planner_input_bundle,
                            round_summary=outcome.round_summary,
                            planner_decision=forced_decision,
                            interrupted=outcome.interrupted,
                        )
                elif round_has_blocked and blocked_replan_once and not blocked_recovery_used:
                    blocked_recovery_used = True
                    data["blocked_recovery_used"] = True
                    record_state_update(
                        "control",
                        {
                            "round": round_number,
                            "blocked_recovery_used": True,
                        },
                    )
                if round_has_blocked and blocked_replan_once and blocked_recovery_used:
                    data["blocked_recovery_used"] = True
                if (
                    round_has_blocked
                    and not blocked_replan_once
                    and outcome.planner_decision.get("next_action") == "continue"
                ):
                    reason = _blocked_recovery_summary(outcome)
                    forced_ref = graph_artifact_id("planner_decision", "blocked_finish", "round", round_number)
                    forced_decision = recorder.record(
                        forced_ref,
                        {
                            "artifact_type": "planner_decision",
                            "round": round_number,
                            "task_done": False,
                            "next_action": "finish",
                            "final_status": "blocked",
                            "reason": reason,
                            "remaining_auto_rounds": 0,
                        },
                        expected_type="planner_decision",
                    )
                    data["planner_decision"] = forced_decision
                    record_state_update("artifacts", _artifact_ref_payload(forced_ref, forced_decision))
                    record_state_update("planner", {"planner_decision_ref": forced_ref})
                    if data.get("rounds") and isinstance(data["rounds"], list):
                        data["rounds"][-1]["planner_decision"] = forced_ref
                    outcome = RoundOutcome(
                        round=outcome.round,
                        planner_order=outcome.planner_order,
                        planner_input_bundle=outcome.planner_input_bundle,
                        round_summary=outcome.round_summary,
                        planner_decision=forced_decision,
                        interrupted=outcome.interrupted,
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
                    continue
                if action == "blocked":
                    status, status_reason, status_code, blocked_node_id, result_resume_checkpoint = self._block_for_controller(
                        data=data,
                        decision=controller_decision,
                        emit=emit,
                    )
                    break
                status, status_reason, status_code, blocked_node_id = finish_values(
                    outcome.planner_decision,
                    controller_decision,
                )
                emit(
                    f"agent_graph.run.{status}",
                    f"Agent graph {self.agent_workflow.id} {status}",
                )
                break
            else:
                prompt = "Agent graph stopped because max_auto_rounds was reached."
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
        except RunCancelled as exc:
            status = "cancelled"
            status_reason = str(exc)
            status_code = "run_cancelled"
            emit("agent_graph.run.cancelled", f"Agent graph cancelled: {exc}", reason=str(exc))
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
        skill_index: SkillIndex,
        skill_store_root: Path,
        trace_context: TraceContext,
        parent_span: TraceSpan,
        controller: RunController,
        run_control: RunControl,
        record_state_update: Any,
    ) -> RoundOutcome:
        run_control.checkpoint("round_started", emit, round_number=round_number)
        record_state_update(
            "control",
            {
                "status": "running",
                "round": round_number,
                "blocked_recovery_used": bool(data.get("blocked_recovery_used")),
            },
        )
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

        cache = RoundWorkingSet(round=round_number, skill_index=skill_index.model_dump(mode="json"))
        repo_intelligence = _repo_intelligence_from_data_or_repo(data, repo_root)
        planner_order = (
            self._planner_order_from_initial_data(data)
            if round_number == 1 and previous_bundle is None
            else None
        ) or self._create_planner_order(
            request,
            previous_bundle=previous_bundle,
            previous_round_summary=previous_round_summary,
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
        record_state_update("artifacts", _artifact_ref_payload(planner_order_ref, data["planner_order"]))
        record_state_update("planner", {"planner_order_ref": planner_order_ref})
        plan_cache = cache.cache_planner_order(planner_order, planner_order_ref)
        record_state_update(
            "work_items",
            {
                "items": [
                    _work_item_state_payload(
                        work_item_id=item.work_item_id,
                        agent_id=item.assignee_agent_id,
                        status="pending",
                        summary=item.task_summary,
                    )
                    for item in plan_cache.work_items
                ]
            },
        )
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
            for blocked in scheduler.block_items_with_blocked_upstreams():
                item = blocked.work_item
                scheduler.mark_blocked(item.work_item_id)
                summary = f"Blocked by upstream work item(s): {', '.join(blocked.blocked_by)}."
                execution = cache.record_execution(
                    ExecutionRecord(
                        work_item_id=item.work_item_id,
                        merge_index=item.merge_index,
                        agent_id=item.assignee_agent_id,
                        status="blocked",
                        execution_summary=summary,
                        execution_result_ref=graph_artifact_id("execution_result", item.work_item_id),
                        artifact_payload={
                            "artifact_type": "execution_result",
                            "round": cache.round,
                            "work_item_id": item.work_item_id,
                            "merge_index": item.merge_index,
                            "agent_id": item.assignee_agent_id,
                            "status": "blocked",
                            "summary": summary,
                            "unexpected_issues": ["upstream_blocked"],
                            "remaining_work": [summary],
                            "needs_planner_decision": True,
                            "blocker_type": "dependency_missing",
                            "continue_without_human_possible": True,
                            "verification": {
                                "status": "blocked",
                                "checks_run": [],
                                "evidence_refs": [],
                                "confidence": "low",
                                "remaining_work": [summary],
                                "no_check_rationale": None,
                                "repair_attempted": False,
                                "repair_summary": None,
                            },
                        },
                    )
                )
                execution = self._normalize_execution_record(cache, cache.round, execution)
                execution_artifact = self._record_execution_artifact(recorder, cache.round, execution)
                record_state_update(
                    "artifacts",
                    _artifact_ref_payload(execution.execution_result_ref, execution_artifact),
                )
                record_state_update(
                    "work_items",
                    _work_item_state_payload(
                        work_item_id=item.work_item_id,
                        agent_id=item.assignee_agent_id,
                        status="blocked",
                        summary=summary,
                        execution_result_ref=execution.execution_result_ref,
                        blocked_reason=_execution_blocked_reason(execution_artifact, execution),
                    ),
                )
                record_state_update(
                    "messages",
                    _execution_state_message_payload(execution, execution_artifact, target="planner"),
                )
                emit(
                    "agent_task.blocked",
                    f"Task {item.work_item_id} blocked by upstream work",
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

            run_control.checkpoint(
                "wave_started",
                emit,
                round_number=round_number,
                wave_index=wave.wave_index,
                active_work_item_ids=[item.work_item_id for item in wave.items],
            )
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
                run_control.checkpoint(
                    "work_item_ready",
                    emit,
                    round_number=round_number,
                    wave_index=wave.wave_index,
                    active_work_item_ids=[item.work_item_id],
                )
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
                    record_state_update=record_state_update,
                )
                task_contexts.append({"item": item, "envelope": envelope})
            outcomes = WaveExecutor(
                self._build_work_item_outcome,
                runtime_policy=_work_item_runtime_policy_from_data(data),
                emit=emit,
                run_control=run_control,
                enable_retry=_feature_enabled_from_data(data, "CODER_ENABLE_WAVE_RETRY", "enable_wave_retry"),
            ).run_wave(wave, task_contexts)
            for outcome in outcomes:
                execution = cache.record_execution(outcome.execution)
                execution = self._normalize_execution_record(cache, cache.round, execution)
                execution_artifact = self._record_execution_artifact(recorder, cache.round, execution)
                execution_status = "completed" if execution.status == "completed" else "blocked"
                record_state_update(
                    "artifacts",
                    _artifact_ref_payload(execution.execution_result_ref, execution_artifact),
                )
                record_state_update(
                    "work_items",
                    _work_item_state_payload(
                        work_item_id=outcome.work_item_id,
                        agent_id=execution.agent_id,
                        status=execution_status,
                        summary=execution.execution_summary,
                        execution_result_ref=execution.execution_result_ref,
                        blocked_reason=_execution_blocked_reason(execution_artifact, execution)
                        if execution_status == "blocked"
                        else None,
                    ),
                )
                record_state_update(
                    "messages",
                    _execution_state_message_payload(execution, execution_artifact, target="planner"),
                )
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
                        "Executor requested Planner intervention",
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
                        "agent_task.blocked",
                        f"Task {outcome.work_item_id} blocked",
                        round=cache.round,
                        work_item_id=outcome.work_item_id,
                        execution_result_ref=execution.execution_result_ref,
                    )
                    scheduler.mark_blocked(outcome.work_item_id)
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
                blocked_work_item_ids=[
                    outcome.work_item_id for outcome in outcomes if outcome.execution.status != "completed"
                ],
            )
            run_control.checkpoint(
                "wave_completed",
                emit,
                round_number=round_number,
                wave_index=wave.wave_index,
                active_work_item_ids=[],
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

        data["graph_run_cache"] = cache.as_runtime_payload()
        data.setdefault("token_ledger", []).extend(cache.token_ledger)

        planner_input_bundle = build_planner_input_bundle(cache)
        planner_input_bundle_ref = graph_artifact_id("planner_input_bundle", "round", cache.round)
        data["planner_input_bundle"] = recorder.record(
            planner_input_bundle_ref,
            planner_input_bundle.model_dump(mode="json", exclude_none=True),
        )
        record_state_update("artifacts", _artifact_ref_payload(planner_input_bundle_ref, data["planner_input_bundle"]))
        record_state_update(
            "messages",
            _state_message_payload(
                message_id=f"planner_input_bundle:{planner_input_bundle_ref}",
                source_agent_id="agent_graph_runner",
                target="planner",
                kind="planner_input_bundle",
                summary=f"PlannerInputBundle round {round_number}: {planner_input_bundle.plan_status}",
                artifact_refs=[planner_input_bundle_ref],
            ),
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
        record_state_update("artifacts", _artifact_ref_payload(round_summary_ref, data["round_summary"]))
        record_state_update("planner", {"round_summary_ref": round_summary_ref})
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
            emit=emit,
        )
        planner_decision_ref = graph_artifact_id("planner_decision", "round", round_number)
        data["planner_decision"] = recorder.record(
            planner_decision_ref,
            planner_decision,
            expected_type="planner_decision",
        )
        record_state_update("artifacts", _artifact_ref_payload(planner_decision_ref, data["planner_decision"]))
        record_state_update("planner", {"planner_decision_ref": planner_decision_ref})
        record_state_update(
            "messages",
            _state_message_payload(
                message_id=f"planner_decision:{planner_decision_ref}",
                source_agent_id=self.agent_workflow.primary_planner_id,
                target="all",
                kind="planner_decision",
                summary=f"Planner decision: {data['planner_decision']['next_action']}",
                artifact_refs=[planner_decision_ref],
            ),
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
            memory = MemoryService(repo_root).record_planner_round(
                workflow_id=self.agent_workflow.id,
                bundle=planner_input_bundle,
                round_summary=round_summary,
                planner_decision=data["planner_decision"],
            )
            data["planner_memory"] = {
                "workflow_id": memory.workflow_id,
                "updated_at": memory.updated_at,
                "planner_notes": len(memory.planner_notes),
                "successful_assignments": len(memory.successful_assignments),
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

    def _block_for_controller(
        self,
        *,
        data: dict[str, Any],
        decision: Any,
        emit: Any,
    ) -> tuple[str, str, str, str, dict[str, Any]]:
        prompt = decision.reason or "RunController blocked the run."
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
        cache: RoundWorkingSet,
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
        record_state_update: Any,
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
        action_spec = ActionSpec(
            action_id=f"build_context:{cache.round}:{item.work_item_id}",
            action_type="build_context",
        )
        action_run_context = RunContext(
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
        )
        emit(
            "action.started",
            "ActionGateway build_context started",
            **action_started_payload(action_spec, action_run_context),
            round=cache.round,
            work_item_id=item.work_item_id,
            _span=action_span,
        )
        action_result = self.action_gateway.run(
            action_spec,
            run_context=action_run_context,
        )
        if action_result.status != "ok":
            trace_context.finish_span(action_span, "blocked" if action_result.status == "blocked" else "failed")
            emit(
                "action.blocked" if action_result.status == "blocked" else "action.failed",
                action_result.summary,
                **action_completed_payload(action_spec, action_result),
                round=cache.round,
                work_item_id=item.work_item_id,
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
            **action_completed_payload(action_spec, action_result),
            round=cache.round,
            work_item_id=item.work_item_id,
            token_used=action_result.token_used,
            _span=action_span,
        )
        context = action_result.payload["context"]
        envelope = context.envelope
        route = context.skill_route
        packet = context.context_packet
        ledger_entry = context.token_ledger_entry
        coding_packet = context.coding_context_packet
        coding_packet_payload = (
            context.compact_coding_context_packet
            if context.compact_coding_context_packet is not None
            else coding_packet.model_dump(mode="json")
        )
        context_packet_payload = packet.model_dump(mode="json")
        context_packet_id = graph_artifact_id("context_packet_v2", cache.round, item.work_item_id)
        coding_context_packet_id = item.work_item_id
        _record_pending_context_packet(data, context_packet_id, context_packet_payload)
        _record_pending_context_packet(data, coding_context_packet_id, coding_packet_payload)
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
            **_compact_context_packet_event_payload(
                packet_id=context_packet_id,
                packet=context_packet_payload,
            ),
            _span=action_span,
        )
        emit(
            "agent.coding_context_packet",
            "CodingContextPacket prepared for work item",
            round=cache.round,
            work_item_id=item.work_item_id,
            **_compact_context_packet_event_payload(
                packet_id=coding_context_packet_id,
                packet=coding_packet_payload,
            ),
            _span=action_span,
        )
        if context.compaction_result is not None:
            emit(
                "agent.context_compaction.applied",
                "Context compaction evaluated for work item",
                round=cache.round,
                work_item_id=item.work_item_id,
                token_estimate_before=context.compaction_result.token_estimate_before,
                token_estimate_after=context.compaction_result.token_estimate_after,
                externalized_refs=context.compaction_result.externalized_refs,
                warnings=context.compaction_result.warnings,
                _span=action_span,
            )
        emit(
            "token.ledger.entry",
            "Token ledger entry recorded",
            round=cache.round,
            work_item_id=item.work_item_id,
            **_compact_token_ledger_payload(ledger_entry.model_dump(mode="json")),
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
        record_state_update(
            "work_items",
            _work_item_state_payload(
                work_item_id=item.work_item_id,
                agent_id=item.assignee_agent_id,
                status="pending",
                summary=item.task_summary,
            ),
        )
        emit(
            "agent_task.started",
            f"Task {item.work_item_id} started",
            round=cache.round,
            work_item_id=item.work_item_id,
            agent_task_id=graph_artifact_id("agent_task", cache.round, item.work_item_id),
            assigned_agent_id=envelope.assigned_agent_id,
            merge_index=envelope.merge_index,
            planner_order_ref=envelope.planner_order_ref,
            upstream_refs=envelope.upstream_refs,
            allowed_skill_ids=envelope.allowed_skill_ids,
            loaded_skill_refs=envelope.loaded_skill_refs,
            omitted_skill_ids=envelope.omitted_skill_ids,
            _span=agent_span,
        )
        record_state_update(
            "work_items",
            _work_item_state_payload(
                work_item_id=item.work_item_id,
                agent_id=item.assignee_agent_id,
                status="running",
                summary=item.task_summary,
            ),
        )
        return envelope

    def _agent_role(self, agent_id: str) -> str:
        for agent in self.agent_workflow.agents:
            if agent.id == agent_id:
                return agent.role
        return ""

    def _work_artifact_type(self, agent_id: str) -> str:
        return "execution_result"

    def _build_work_item_outcome(self, context: dict[str, Any]) -> WorkItemOutcome:
        item = context["item"]
        envelope = context["envelope"]
        execution = self.agent_run.run_execution(item=item, envelope=envelope)
        return WorkItemOutcome(
            work_item_id=item.work_item_id,
            merge_index=item.merge_index,
            execution=execution,
        )

    def _normalize_execution_record(
        self,
        cache: RoundWorkingSet,
        round_number: int,
        execution: ExecutionRecord,
    ) -> ExecutionRecord:
        from coder_workbench.agent_harness.execution_verification import ensure_execution_verification

        artifact_type = execution.artifact_type
        payload = execution.artifact_payload or {
            "artifact_type": artifact_type,
            "round": round_number,
            "work_item_id": execution.work_item_id,
            "merge_index": execution.merge_index,
            "agent_id": execution.agent_id,
            "status": execution.status,
            "summary": execution.execution_summary,
            "outputs": [execution.execution_result_ref] if execution.status == "completed" else [],
            "unexpected_issues": [] if execution.status == "completed" else ["execution_record_missing_payload"],
            "remaining_work": [] if execution.status == "completed" else [execution.execution_summary],
            "blocker_type": None if execution.status == "completed" else "unknown_error",
            "continue_without_human_possible": None if execution.status == "completed" else True,
            "verification": _fallback_verification(execution),
        }
        if artifact_type == "execution_result":
            payload = ensure_execution_verification(payload)
        payload = dict(payload)
        payload["artifact_id"] = execution.execution_result_ref
        normalized = execution.model_copy(update={"artifact_payload": payload})
        cache.execution_cache[execution.work_item_id] = normalized
        return normalized

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
            "outputs": [execution.execution_result_ref] if execution.status == "completed" else [],
            "unexpected_issues": [] if execution.status == "completed" else ["execution_record_missing_payload"],
            "remaining_work": [] if execution.status == "completed" else [execution.execution_summary],
            "blocker_type": None if execution.status == "completed" else "technical_blocker",
            "continue_without_human_possible": None if execution.status == "completed" else True,
            "verification": _fallback_verification(execution),
        }
        artifact_type = str(payload.get("artifact_type") or artifact_type)
        return recorder.record(
            execution.execution_result_ref,
            payload,
            expected_type=artifact_type,
        )

    def _record_debug_findings(
        self,
        cache: RoundWorkingSet,
        recorder: AgentGraphArtifactRecorder,
        repo_root: str,
        emit: Any,
    ) -> list[dict[str, Any]]:
        findings: list[dict[str, Any]] = []
        for execution in cache.execution_cache.values():
            artifact = execution.artifact_payload or {}
            verification = artifact.get("verification") if isinstance(artifact.get("verification"), dict) else {}
            checks = verification.get("checks_run") if isinstance(verification.get("checks_run"), list) else []
            for check in checks:
                if not isinstance(check, dict) or check.get("status") not in {"fail", "blocked"}:
                    continue
                finding = build_debug_finding(
                    {
                        "artifact_type": "check_result",
                        "command": str(check.get("command") or ""),
                        "status": str(check.get("status") or "fail"),
                        "summary": str(check.get("summary") or execution.execution_summary),
                        "output": str(check.get("summary") or execution.execution_summary),
                        "output_ref": str(check.get("output_ref") or execution.execution_result_ref),
                    },
                    work_item_id=execution.work_item_id,
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
        cache: RoundWorkingSet,
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
        cache: RoundWorkingSet,
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
        cache: RoundWorkingSet,
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
            if effect.get("effect_type") == "modify_files":
                tool = "propose_patch"
            elif effect.get("effect_type") == "runtime_action":
                tool = str(effect.get("operation_id") or effect.get("action_type") or "runtime_action")
            else:
                tool = "run_check"
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
        skill_index: SkillIndex,
        repo_intelligence: dict[str, Any],
        round_number: int,
        emit: Any,
    ) -> PlannerOrder:
        return self.agent_run.run_planner_order(
            request,
            previous_bundle=previous_bundle,
            previous_round_summary=previous_round_summary,
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
        payload = dict(value)
        payload.setdefault("artifact_type", "planner_decision")
        payload.setdefault("round", 1)
        payload.setdefault("reason", "")
        return validate_artifact(payload, expected_type="planner_decision")


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
    if effect.get("effect_type") == "runtime_action":
        return {
            "artifact_type": "runtime_action",
            "effect": effect,
            "status": effect.get("status"),
            "summary": effect.get("reason") or output.get("summary") or "",
            "output": output,
        }
    return {
        "artifact_type": "hidden_effect",
        "effect": effect,
        "output": output,
    }


def _normalize_resume_checkpoint(
    checkpoint: dict[str, Any],
    *,
    data: dict[str, Any],
    events: list[RunEvent],
    status_code: str | None,
    phase: str,
) -> dict[str, Any]:
    payload = dict(checkpoint)
    checkpoint_data = payload.get("data") if isinstance(payload.get("data"), dict) else data
    graph_run_cache = checkpoint_data.get("graph_run_cache") if isinstance(checkpoint_data, dict) else None
    completed, blocked = _checkpoint_work_item_ids(graph_run_cache)
    if isinstance(checkpoint_data, dict):
        checkpoint_data.setdefault("completed_work_item_ids", completed)
        checkpoint_data.setdefault("blocked_work_item_ids", blocked)
    round_number = _checkpoint_round(checkpoint_data, graph_run_cache)
    payload.setdefault("checkpoint_version", 1)
    payload.setdefault("resume_mode", "agent_graph_checkpoint")
    payload["data"] = checkpoint_data
    payload.setdefault("round", round_number)
    payload.setdefault("phase", phase)
    payload.setdefault("planner_input_bundle", _dict_or_empty(checkpoint_data.get("planner_input_bundle") if isinstance(checkpoint_data, dict) else None))
    payload.setdefault("round_summary", _dict_or_empty(checkpoint_data.get("round_summary") if isinstance(checkpoint_data, dict) else None))
    payload.setdefault("planner_decision", _dict_or_empty(checkpoint_data.get("planner_decision") if isinstance(checkpoint_data, dict) else None))
    payload.setdefault("completed_work_item_ids", completed)
    payload.setdefault("blocked_work_item_ids", blocked)
    payload.setdefault("graph_run_cache", graph_run_cache if isinstance(graph_run_cache, dict) else {})
    payload.setdefault("event_cursor", len(events))
    return payload


def _checkpoint_work_item_ids(graph_run_cache: Any) -> tuple[list[str], list[str]]:
    if not isinstance(graph_run_cache, dict):
        return [], []
    completed: list[str] = []
    blocked: list[str] = []
    plan_cache = graph_run_cache.get("plan_cache")
    work_items = plan_cache.get("work_items") if isinstance(plan_cache, dict) else None
    if isinstance(work_items, list):
        for item in work_items:
            if not isinstance(item, dict):
                continue
            work_item_id = str(item.get("work_item_id") or "")
            if not work_item_id:
                continue
            if item.get("status") == "completed":
                completed.append(work_item_id)
            elif item.get("status") in {"blocked", "failed", "cancelled"}:
                blocked.append(work_item_id)
    executions = graph_run_cache.get("execution_cache")
    if isinstance(executions, dict):
        for work_item_id, record in executions.items():
            if not isinstance(record, dict):
                continue
            if record.get("status") == "completed" and str(work_item_id) not in completed:
                completed.append(str(work_item_id))
            elif record.get("status") != "completed" and str(work_item_id) not in blocked:
                blocked.append(str(work_item_id))
    return completed, blocked


def _checkpoint_round(data: Any, graph_run_cache: Any) -> int:
    if isinstance(graph_run_cache, dict):
        try:
            return max(1, int(graph_run_cache.get("round") or 1))
        except (TypeError, ValueError):
            pass
    if isinstance(data, dict):
        for key in ("round", "active_round"):
            try:
                return max(1, int(data.get(key) or 1))
            except (TypeError, ValueError):
                continue
    return 1


def _dict_or_empty(value: Any) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def _shared_run_state_from_data(
    data: dict[str, Any],
    *,
    run_id: str,
    workflow_id: str,
    request: str,
) -> SharedRunState:
    value = data.get("shared_run_state")
    if isinstance(value, dict):
        try:
            state = SharedRunState.model_validate(value)
            if state.run_id == run_id:
                return state
        except Exception:
            pass
    return SharedRunState(run_id=run_id, workflow_id=workflow_id, user_request=request)


def _artifact_ref_payload(artifact_id: str, artifact: dict[str, Any]) -> dict[str, str]:
    summary = artifact.get("summary")
    if not isinstance(summary, str) or not summary.strip():
        summary = summarize_value(artifact_summary(artifact), max_chars=240)
    return {
        "artifact_id": artifact_id,
        "artifact_type": str(artifact.get("artifact_type") or ""),
        "summary": summary,
    }


def _work_item_state_payload(
    *,
    work_item_id: str,
    agent_id: str,
    status: str,
    summary: str = "",
    execution_result_ref: str | None = None,
    blocked_reason: str | None = None,
) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "work_item_id": work_item_id,
        "agent_id": agent_id,
        "status": status,
        "summary": summary,
    }
    if execution_result_ref:
        payload["execution_result_ref"] = execution_result_ref
    if blocked_reason:
        payload["blocked_reason"] = blocked_reason
    return payload


def _state_message_payload(
    *,
    message_id: str,
    source_agent_id: str,
    target: str,
    kind: str,
    summary: str,
    artifact_refs: list[str] | None = None,
    tool_result_refs: list[str] | None = None,
    blob_refs: list[str] | None = None,
) -> dict[str, Any]:
    return {
        "message_id": message_id,
        "source_agent_id": source_agent_id,
        "target": target,
        "kind": kind,
        "summary": summary,
        "artifact_refs": artifact_refs or [],
        "tool_result_refs": tool_result_refs or [],
        "blob_refs": blob_refs or [],
    }


def _execution_state_message_payload(
    execution: ExecutionRecord,
    artifact: dict[str, Any],
    *,
    target: str,
) -> dict[str, Any]:
    blocked = execution.status != "completed"
    return _state_message_payload(
        message_id=f"execution_result:{execution.execution_result_ref}",
        source_agent_id=execution.agent_id,
        target=target,
        kind="execution_blocked" if blocked else "execution_completed",
        summary=str(artifact.get("summary") or execution.execution_summary or execution.status),
        artifact_refs=[execution.execution_result_ref],
    )


def _execution_blocked_reason(execution_artifact: dict[str, Any], execution: ExecutionRecord) -> str:
    return str(
        execution_artifact.get("blocker_reason")
        or execution_artifact.get("summary")
        or execution.execution_summary
        or "Execution blocked."
    )


def _record_pending_context_packet(data: dict[str, Any], packet_id: str, packet: dict[str, Any]) -> None:
    pending = data.setdefault("pending_context_packets", {})
    if isinstance(pending, dict):
        pending[packet_id] = packet


def _compact_context_packet_event_payload(
    *,
    packet_id: str,
    packet: dict[str, Any],
) -> dict[str, Any]:
    return {
        "packet_id": packet_id,
        "summary": _context_packet_summary(packet),
        "size_chars": len(json.dumps(packet, ensure_ascii=False)),
    }


def _compact_token_ledger_payload(entry: dict[str, Any]) -> dict[str, Any]:
    return {
        key: entry.get(key)
        for key in (
            "run_id",
            "agent_id",
            "artifact_type",
            "estimated_input_tokens",
            "estimated_output_tokens",
            "skill_tokens_available",
            "skill_tokens_loaded",
            "upstream_tokens_loaded",
            "omitted_tokens",
            "compression_ratio",
        )
        if key in entry
    }


def _context_packet_summary(packet: dict[str, Any]) -> dict[str, Any]:
    keys = (
        "agent_id",
        "work_item_id",
        "artifact_type",
        "estimated_input_tokens",
        "estimated_omitted_tokens",
        "compression_ratio",
    )
    summary = {key: packet[key] for key in keys if key in packet}
    if "included_skill_ids" in packet:
        summary["included_skill_ids"] = list(packet.get("included_skill_ids") or [])[:8]
    if "included_refs" in packet:
        summary["included_refs"] = list(packet.get("included_refs") or [])[:8]
    if "included_files" in packet:
        summary["included_files"] = list(packet.get("included_files") or [])[:8]
    return summary


def _fallback_verification(execution: ExecutionRecord) -> dict[str, Any]:
    if execution.status == "completed":
        return {
            "status": "skipped",
            "checks_run": [],
            "evidence_refs": [execution.execution_result_ref],
            "confidence": "medium",
            "remaining_work": [],
            "no_check_rationale": "ExecutionRecord did not include an artifact payload with explicit verification.",
            "repair_attempted": False,
            "repair_summary": None,
        }
    return {
        "status": "blocked",
        "checks_run": [],
        "evidence_refs": [execution.execution_result_ref],
        "confidence": "low",
        "remaining_work": [execution.execution_summary],
        "no_check_rationale": None,
        "repair_attempted": False,
        "repair_summary": None,
    }


class _ExecutorAdapter:
    def __init__(self, executor: Any) -> None:
        self.executor = executor

    def run_planner_order(self, request: str, **kwargs: Any) -> PlannerOrder:
        core_kwargs = {
            "previous_bundle": kwargs.get("previous_bundle"),
            "previous_round_summary": kwargs.get("previous_round_summary"),
            "round_number": kwargs.get("round_number", 1),
            "emit": kwargs.get("emit"),
        }
        try:
            return self.executor.create_planner_order(request, **kwargs)
        except TypeError:
            try:
                return self.executor.create_planner_order(request, **core_kwargs)
            except TypeError:
                return self.executor.create_planner_order(request, emit=kwargs.get("emit"))

    def run_execution(self, **kwargs: Any) -> ExecutionRecord:
        payload = {
            "item": kwargs["item"],
            "envelope": kwargs["envelope"],
        }
        if kwargs.get("emit") is not None:
            payload["emit"] = kwargs["emit"]
        return self.executor.create_execution_result(**payload)

    def run_planner_decision(self, **kwargs: Any) -> dict[str, Any]:
        payload = {
            "bundle": kwargs["bundle"],
        }
        if kwargs.get("emit") is not None:
            payload["emit"] = kwargs["emit"]
        return self.executor.create_planner_decision(**payload)


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


def _blocked_recovery_summary(outcome: RoundOutcome) -> str:
    blocked_items = [
        item
        for item in outcome.planner_input_bundle.items
        if item.execution_status == "blocked"
    ]
    if not blocked_items:
        return "Planner received blocked execution results."
    details = "; ".join(
        f"{item.work_item_id}: {item.execution_summary or item.verification_summary or 'blocked'}"
        for item in blocked_items
    )
    return f"Blocked recovery was exhausted. Blocked WorkItems: {details}"


def _blocked_progress_stop_reason(
    outcome: RoundOutcome,
    *,
    previous_bundle: PlannerInputBundle | None,
    artifacts: dict[str, Any],
) -> str | None:
    policy_blocked = _policy_blocked_summary(outcome.planner_input_bundle, artifacts)
    if policy_blocked:
        return policy_blocked
    if previous_bundle is None:
        return None
    current_signature = _blocked_blocker_key(outcome.planner_input_bundle, artifacts)
    previous_signature = _blocked_blocker_key(previous_bundle, artifacts)
    if current_signature and current_signature == previous_signature:
        return f"Blocked recovery stopped after the same blocker repeated: {current_signature}."
    current_refs = _progress_evidence_refs(outcome.planner_input_bundle, artifacts)
    previous_refs = _progress_evidence_refs(previous_bundle, artifacts)
    if not current_refs:
        return "Blocked recovery stopped because the blocked round produced no diff or evidence refs."
    if current_refs.issubset(previous_refs):
        return "Blocked recovery stopped because this round produced no new diff or evidence refs."
    return None


def _blocked_recommends_replan_once(outcome: RoundOutcome, artifacts: dict[str, Any]) -> bool:
    for item in outcome.planner_input_bundle.items:
        if item.execution_status != "blocked" and item.verification_status not in {"fail", "blocked"}:
            continue
        for ref in item.refs:
            artifact = artifacts.get(ref)
            if not isinstance(artifact, dict):
                continue
            if _artifact_policy_blocked(artifact):
                continue
            if artifact.get("planner_recommendation") == "replan_once":
                return True
    return any(
        interrupt.continue_without_human_possible is True
        and interrupt.blocker_type not in POLICY_BLOCKER_TYPES
        for interrupt in outcome.planner_input_bundle.interrupts
    )


def _policy_blocked_summary(bundle: PlannerInputBundle, artifacts: dict[str, Any]) -> str | None:
    for interrupt in bundle.interrupts:
        if interrupt.blocker_type in POLICY_BLOCKER_TYPES:
            artifact = artifacts.get(interrupt.artifact_ref)
            if isinstance(artifact, dict) and not _artifact_policy_blocked(artifact):
                continue
            return f"Sandbox or security policy blocked progress: {interrupt.reason}"
    for item in bundle.items:
        for artifact in _artifacts_for_item(item.refs, artifacts):
            if _artifact_policy_blocked(artifact):
                blocker = str(artifact.get("blocker_type") or "policy_blocked")
                reason = str(artifact.get("blocker_reason") or artifact.get("summary") or "policy boundary")
                return f"Sandbox or security policy blocked progress: {blocker}: {reason}"
    return None


def _blocked_blocker_key(bundle: PlannerInputBundle, artifacts: dict[str, Any]) -> tuple[str, ...]:
    signatures: list[str] = []
    for item in bundle.items:
        if item.execution_status != "blocked" and item.verification_status not in {"fail", "blocked"}:
            continue
        item_signatures: list[str] = []
        for artifact in _artifacts_for_item(item.refs, artifacts):
            fingerprint = str(artifact.get("blocker_fingerprint") or "").strip()
            if fingerprint:
                item_signatures.append(fingerprint)
                continue
            blocker_type = str(artifact.get("blocker_type") or "").strip()
            reason = str(artifact.get("blocker_reason") or artifact.get("summary") or "").strip()
            if blocker_type or reason:
                item_signatures.append(f"{blocker_type}:{reason}")
        if item_signatures:
            signatures.append("|".join(sorted(set(item_signatures))))
        else:
            signatures.append(item.execution_summary or item.verification_summary)
    return tuple(sorted(signatures))


def _progress_evidence_refs(bundle: PlannerInputBundle, artifacts: dict[str, Any]) -> set[str]:
    refs: set[str] = set()
    for item in bundle.items:
        for artifact in _artifacts_for_item(item.refs, artifacts):
            refs.update(_artifact_progress_refs(artifact))
    return refs


def _artifact_policy_blocked(artifact: dict[str, Any]) -> bool:
    if artifact.get("planner_recommendation") == "replan_once" and artifact.get("continue_without_human_possible") is True:
        return False
    if artifact.get("blocker_type") in POLICY_BLOCKER_TYPES:
        return True
    boundary = artifact.get("constraint_boundary")
    return isinstance(boundary, dict) and (
        boundary.get("requires_out_of_scope_write") is True
        or boundary.get("requires_destructive_action") is True
    )


def _artifact_progress_refs(artifact: dict[str, Any]) -> set[str]:
    refs: set[str] = set()
    for key in ("evidence_refs", "patch_refs", "outputs"):
        refs.update(_string_refs(artifact.get(key)))
    verification = artifact.get("verification") if isinstance(artifact.get("verification"), dict) else {}
    refs.update(_string_refs(verification.get("evidence_refs")))
    for check in verification.get("checks_run") or []:
        if not isinstance(check, dict):
            continue
        refs.update(_string_refs(check.get("evidence_refs")))
        output_ref = check.get("output_ref")
        if isinstance(output_ref, str) and output_ref.strip():
            refs.add(output_ref)
    return refs


def _artifacts_for_item(refs: list[str], artifacts: dict[str, Any]) -> list[dict[str, Any]]:
    return [artifact for ref in refs if isinstance((artifact := artifacts.get(ref)), dict)]


def _string_refs(value: Any) -> set[str]:
    if isinstance(value, str):
        return {value.strip()} if value.strip() else set()
    if isinstance(value, list):
        return {str(item).strip() for item in value if str(item).strip()}
    return set()


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


def _work_item_runtime_policy_from_data(data: dict[str, Any]) -> WorkItemRuntimePolicy:
    raw = data.get("work_item_runtime_policy")
    values = dict(raw) if isinstance(raw, dict) else {}
    return WorkItemRuntimePolicy(
        timeout_seconds=_float_value(values.get("timeout_seconds"), 600),
        max_retries=_int_value(values.get("max_retries"), 0),
        retry_on_status_codes=[
            str(item)
            for item in values.get("retry_on_status_codes", [])
            if str(item).strip()
        ]
        if isinstance(values.get("retry_on_status_codes"), list)
        else [],
        allow_partial_result=bool(values.get("allow_partial_result", True)),
    )


def _feature_enabled_from_data(data: dict[str, Any], env_name: str, data_key: str) -> bool:
    if data.get(data_key) is not None:
        return bool(data.get(data_key))
    import os

    return str(os.getenv(env_name) or "").strip().lower() in {"1", "true", "yes", "on"}


def _float_value(value: Any, default: float) -> float:
    try:
        return max(0.001, float(value))
    except (TypeError, ValueError):
        return default


def _int_value(value: Any, default: int) -> int:
    try:
        return max(0, int(value))
    except (TypeError, ValueError):
        return default


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
