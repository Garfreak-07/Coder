from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import Any

from coder_workbench.agent_graph.artifacts import AgentGraphArtifactRecorder, graph_artifact_id
from coder_workbench.agent_graph.cache import GraphRunCache
from coder_workbench.agent_graph.context import upstream_refs_for_item
from coder_workbench.agent_graph.effects import apply_hidden_effects
from coder_workbench.agent_graph.executor import (
    AgentGraphExecutor,
    AgentGraphExecutorError,
    AgentGraphExecutorProtocol,
)
from coder_workbench.agent_graph.merge import build_planner_input_bundle, build_round_summary
from coder_workbench.agent_graph.scheduler import AgentGraphScheduler, ReadyWave
from coder_workbench.agent_graph.schema import FinalTestRecord, ExecutionRecord, PlannerOrder, TestRecord, WorkItemOutcome
from coder_workbench.agent_graph.validation import assert_valid_planner_order
from coder_workbench.core import (
    AgentWorkflowSpec,
    AgentWorkflowValidationError,
    assert_valid_agent_workflow,
)
from coder_workbench.runtime.state import RunEvent, RunResult, summarize_value


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
        executor: AgentGraphExecutorProtocol | None = None,
    ) -> None:
        self.agent_workflow = agent_workflow
        self.event_sink = event_sink
        self.runtime_settings = runtime_settings
        self.executor = executor or AgentGraphExecutor(
            agent_workflow,
            runtime_settings=runtime_settings,
        )

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

        def emit(event_type: str, message: str, **payload: Any) -> None:
            event = RunEvent(type=event_type, message=message, payload=payload)
            events.append(event)
            if self.event_sink:
                self.event_sink(event)

        recorder = AgentGraphArtifactRecorder(artifacts, emit)

        try:
            if resume_after_node:
                raise ValueError("AgentGraphRunner resume_after_node is not supported")

            assert_valid_agent_workflow(self.agent_workflow)
            workflow_payload = self.agent_workflow.model_dump(mode="json", by_alias=True, exclude_none=True)
            data["agent_workflow"] = workflow_payload

            emit(
                "agent_graph.run.started",
                f"Agent graph {self.agent_workflow.id} started",
                workflow_id=self.agent_workflow.id,
                repo_root=repo_root,
                request=request,
            )
            emit(
                "agent_graph.round.started",
                "Agent graph round 1 started",
                workflow_id=self.agent_workflow.id,
                round=1,
                primary_planner_id=self.agent_workflow.primary_planner_id,
            )

            cache = GraphRunCache(round=1)
            planner_order = self._planner_order_from_initial_data(data) or self.executor.create_planner_order(
                request,
                emit=emit,
            )
            try:
                assert_valid_planner_order(self.agent_workflow, planner_order)
            except AgentWorkflowValidationError as exc:
                raise AgentGraphExecutorError(
                    f"PlannerOrder graph validation failed: {exc}",
                    status_code="planner_order_validation_failed",
                ) from exc
            planner_order_ref = graph_artifact_id("planner_order", "round", 1)
            data["planner_order"] = planner_order.model_dump(mode="json", exclude_none=True)
            emit(
                "planner.order.produced",
                "Planner produced a PlanGraph",
                artifact_type="planner_order",
                artifact_id=planner_order_ref,
                round=1,
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
                round=1,
                work_items=len(plan_cache.work_items),
            )

            scheduler = AgentGraphScheduler(
                planner_order.plan_graph.work_items,
                max_concurrency=_max_concurrency_from_data(data),
            )
            while scheduler.has_pending():
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
                        round=1,
                        work_item_id=item.work_item_id,
                        blocked_by=blocked.blocked_by,
                    )

                wave = scheduler.next_wave()
                if wave.deferred_ready_work_item_ids:
                    emit(
                        "resource.deferred",
                        "Ready work items deferred by max_concurrency",
                        round=1,
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
                            round=1,
                            waiting_work_item_ids=[item.work_item_id for item in waiting],
                        )
                    break

                emit(
                    "agent_graph.wave.started",
                    f"Agent graph wave {wave.wave_index} started",
                    round=1,
                    wave_index=wave.wave_index,
                    ready_work_item_ids=wave.ready_work_item_ids,
                    work_item_ids=[item.work_item_id for item in wave.items],
                    deferred_ready_work_item_ids=wave.deferred_ready_work_item_ids,
                )
                task_contexts = []
                for item in wave.items:
                    scheduler.mark_running(item.work_item_id)
                    if item.depends_on:
                        emit(
                            "join.completed",
                            f"All upstream work items completed for {item.work_item_id}",
                            round=1,
                            work_item_id=item.work_item_id,
                            depends_on=item.depends_on,
                        )
                    envelope = self._start_work_item(cache, item, planner_order_ref, emit)
                    task_contexts.append({"item": item, "envelope": envelope})
                outcomes = self._run_wave(wave, task_contexts)
                for outcome in outcomes:
                    execution = cache.record_execution(outcome.execution)
                    self._record_execution_artifact(recorder, cache.round, execution)
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
                    round=1,
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

            data["scheduler_status"] = dict(scheduler.status_by_id)
            hidden_effects = apply_hidden_effects(
                agent_workflow=self.agent_workflow,
                cache=cache,
                repo_root=repo_root,
                scopes=_scopes_from_data(data),
                data=data,
            )
            if hidden_effects:
                data["hidden_effects"] = hidden_effects
                self._emit_hidden_effect_outputs(cache, hidden_effects, emit)

            final_tester_agent_id = planner_order.plan_graph.final_tester_agent_id
            if final_tester_agent_id:
                pre_final_bundle = build_planner_input_bundle(cache)
                final_test = cache.record_final_test(
                    self.executor.create_final_test_result(
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
                round=1,
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
                round=1,
                plan_status=round_summary.plan_status,
            )

            planner_decision = self._planner_decision_from_initial_data(data) or self.executor.create_planner_decision(
                bundle=planner_input_bundle,
                planner_human_response=data.get("planner_human_response")
                if isinstance(data.get("planner_human_response"), dict)
                else None,
                emit=emit,
            )
            planner_decision_ref = graph_artifact_id("planner_decision", "round", 1)
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
                round=1,
                next_action=data["planner_decision"]["next_action"],
            )
            if data["planner_decision"]["next_action"] == "ask_human":
                prompt = (
                    data["planner_decision"].get("human_message")
                    or data["planner_decision"].get("reason")
                    or "Planner needs user input."
                )
                data["planner_human_prompt"] = prompt
                emit(
                    "planner.human_prompt",
                    "Planner requested human input",
                    round=1,
                    prompt=prompt,
                    status_code="planner_ask_human",
                )
                emit("agent_graph.run.blocked", "Agent graph blocked for Planner human prompt", code="planner_ask_human")
                status = "blocked"
                status_reason = str(prompt)
                status_code = "planner_ask_human"
                blocked_node_id = self.agent_workflow.primary_planner_id
                result_resume_checkpoint = {"data": data}
            else:
                emit("agent_graph.run.completed", f"Agent graph {self.agent_workflow.id} completed")
                status = "completed"
                status_reason = None
                status_code = None
        except Exception as exc:  # pragma: no cover - boundary safety
            status = "failed"
            status_reason = str(exc)
            status_code = exc.status_code if isinstance(exc, AgentGraphExecutorError) else "agent_graph_runtime_exception"
            emit("agent_graph.run.failed", f"Agent graph failed: {exc}", error=str(exc))

        return RunResult(
            status=status,
            data=data,
            summaries={key: summarize_value(value) for key, value in data.items()},
            artifacts=artifacts,
            events=events,
            estimated_tokens_used=0,
            agent_calls=0,
            tool_calls=0,
            blocked_node_id=blocked_node_id,
            resume_checkpoint=result_resume_checkpoint,
            status_reason=status_reason,
            status_code=status_code,
        )

    def _start_work_item(
        self,
        cache: GraphRunCache,
        item: Any,
        planner_order_ref: str,
        emit: Any,
    ) -> Any:
        upstream_refs = upstream_refs_for_item(cache, item)
        envelope = cache.create_agent_task(
            item,
            planner_order_ref=planner_order_ref,
            upstream_refs=upstream_refs,
        )
        emit(
            "agent_task.ready",
            f"Task {item.work_item_id} is ready",
            round=cache.round,
            work_item_id=item.work_item_id,
            assigned_agent_id=item.assignee_agent_id,
            merge_index=item.merge_index,
        )
        emit(
            "agent_task.started",
            f"Task {item.work_item_id} started",
            round=cache.round,
            work_item_id=item.work_item_id,
            envelope=envelope.model_dump(mode="json"),
        )
        return envelope

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
        execution = self.executor.create_execution_result(item=item, envelope=envelope)
        tests = []
        if execution.status == "completed":
            execution_artifact = {
                "artifact_type": "execution_result",
                "artifact_id": execution.execution_result_ref,
                "round": envelope.round,
                "work_item_id": execution.work_item_id,
                "merge_index": execution.merge_index,
                "agent_id": execution.agent_id,
                "status": execution.status,
                "summary": execution.execution_summary,
            }
            tests = [
                self.executor.create_test_result(
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
        payload = execution.artifact_payload or {
            "artifact_type": "execution_result",
            "round": round_number,
            "work_item_id": execution.work_item_id,
            "merge_index": execution.merge_index,
            "agent_id": execution.agent_id,
            "status": execution.status,
            "summary": execution.execution_summary,
        }
        return recorder.record(
            execution.execution_result_ref,
            payload,
            expected_type="execution_result",
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

def _max_concurrency_from_data(data: dict[str, Any]) -> int:
    value = data.get("max_concurrency")
    try:
        return max(1, int(value))
    except (TypeError, ValueError):
        return 4


def _scopes_from_data(data: dict[str, Any]) -> list[str]:
    scopes = data.get("scopes")
    if isinstance(scopes, list):
        return [str(scope) for scope in scopes if str(scope).strip()]
    if isinstance(scopes, str) and scopes.strip():
        return [scopes.strip()]
    return []
