from __future__ import annotations

from typing import Any

from coder_workbench.agent_graph.cache import GraphRunCache
from coder_workbench.agent_graph.context import upstream_refs_for_item
from coder_workbench.agent_graph.scheduler import AgentGraphScheduler
from coder_workbench.agent_graph.schema import ExecutionRecord, PlannerOrder, TestRecord
from coder_workbench.core import AgentWorkflowAgent, AgentWorkflowSpec, assert_valid_agent_workflow
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
    ) -> None:
        self.agent_workflow = agent_workflow
        self.event_sink = event_sink
        self.runtime_settings = runtime_settings

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

        def emit(event_type: str, message: str, **payload: Any) -> None:
            event = RunEvent(type=event_type, message=message, payload=payload)
            events.append(event)
            if self.event_sink:
                self.event_sink(event)

        try:
            if resume_checkpoint or resume_after_node:
                raise ValueError("AgentGraphRunner resume is not implemented in Phase 1")

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
            planner_order = self._planner_order_from_initial_data(data) or self._mock_planner_order(request)
            planner_order_ref = "memory:planner_order:round-1"
            data["planner_order"] = planner_order.model_dump(mode="json", exclude_none=True)
            emit(
                "planner.order.produced",
                "Planner produced a PlanGraph",
                artifact_type="planner_order",
                round=1,
                planner_order=data["planner_order"],
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
                    cache.record_execution(
                        ExecutionRecord(
                            work_item_id=item.work_item_id,
                            order_index=item.order_index,
                            agent_id=item.assignee_agent_id,
                            status="blocked",
                            execution_summary=f"Blocked by failed upstream work item(s): {', '.join(blocked.blocked_by)}.",
                            execution_result_ref=f"memory:execution_result:{item.work_item_id}:blocked",
                        )
                    )
                    emit(
                        "agent_task.blocked",
                        f"Task {item.work_item_id} blocked by upstream failure",
                        round=1,
                        work_item_id=item.work_item_id,
                        blocked_by=blocked.blocked_by,
                    )

                ready_items = scheduler.ready_items()
                if not ready_items:
                    waiting = scheduler.waiting_items()
                    if waiting:
                        emit(
                            "join.waiting",
                            "Waiting for upstream work items",
                            round=1,
                            waiting_work_item_ids=[item.work_item_id for item in waiting],
                        )
                    break

                for item in ready_items:
                    scheduler.mark_running(item.work_item_id)
                    if item.depends_on:
                        emit(
                            "join.completed",
                            f"All upstream work items completed for {item.work_item_id}",
                            round=1,
                            work_item_id=item.work_item_id,
                            depends_on=item.depends_on,
                        )
                    self._run_work_item(cache, item, planner_order_ref, emit)
                    execution = cache.execution_cache[item.work_item_id]
                    if execution.status == "completed":
                        scheduler.mark_completed(item.work_item_id)
                    elif execution.status == "blocked":
                        scheduler.mark_blocked(item.work_item_id)
                    else:
                        scheduler.mark_failed(item.work_item_id)

            data["scheduler_status"] = dict(scheduler.status_by_id)
            data["graph_run_cache"] = cache.as_runtime_payload()

            planner_input_bundle = self._mock_planner_input_bundle(cache)
            data["planner_input_bundle"] = planner_input_bundle
            emit(
                "planner.input_bundle.created",
                "Compact PlannerInputBundle created",
                artifact_type="planner_input_bundle",
                round=1,
                items=len(planner_input_bundle["items"]),
            )

            round_summary = self._mock_round_summary(cache)
            data["round_summary"] = round_summary
            emit(
                "round_summary.created",
                "Round summary created",
                artifact_type="round_summary",
                round=1,
                plan_status=round_summary["plan_status"],
            )

            planner_decision = {
                "artifact_type": "planner_decision",
                "round": 1,
                "task_done": True,
                "next_action": "finish",
                "reason": "Phase 3 AgentGraphRunner mock-mode completed dependency scheduling.",
            }
            data["planner_decision"] = planner_decision
            emit(
                "planner.decision.produced",
                "Planner decision produced",
                artifact_type="planner_decision",
                round=1,
                next_action="finish",
            )
            emit("agent_graph.run.completed", f"Agent graph {self.agent_workflow.id} completed")
            status = "completed"
            status_reason = None
            status_code = None
        except Exception as exc:  # pragma: no cover - boundary safety
            status = "failed"
            status_reason = str(exc)
            status_code = "agent_graph_runtime_exception"
            emit("agent_graph.run.failed", f"Agent graph failed: {exc}", error=str(exc))

        return RunResult(
            status=status,
            data=data,
            summaries={key: summarize_value(value) for key, value in data.items()},
            artifacts={},
            events=events,
            estimated_tokens_used=0,
            agent_calls=0,
            tool_calls=0,
            status_reason=status_reason,
            status_code=status_code,
        )

    def _run_work_item(
        self,
        cache: GraphRunCache,
        item: Any,
        planner_order_ref: str,
        emit: Any,
    ) -> None:
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
            order_index=item.order_index,
        )
        emit(
            "agent_task.started",
            f"Task {item.work_item_id} started",
            round=cache.round,
            work_item_id=item.work_item_id,
            envelope=envelope.model_dump(mode="json"),
        )
        execution_record = cache.record_execution(
            ExecutionRecord(
                work_item_id=item.work_item_id,
                order_index=item.order_index,
                agent_id=item.assignee_agent_id,
                status="completed",
                execution_summary="Phase 3 mock execution completed from an AgentTaskEnvelope.",
                execution_result_ref=f"memory:execution_result:{item.work_item_id}",
            )
        )
        emit(
            "agent_task.completed",
            f"Task {item.work_item_id} completed",
            round=cache.round,
            work_item_id=item.work_item_id,
            execution_result_ref=execution_record.execution_result_ref,
        )
        for tester_agent_id in item.tester_agent_ids:
            test_record = cache.record_test(
                TestRecord(
                    work_item_id=item.work_item_id,
                    order_index=item.order_index,
                    tester_agent_id=tester_agent_id,
                    status="pass",
                    test_summary="Phase 3 mock test evidence recorded.",
                    test_result_ref=f"memory:test_result:{item.work_item_id}:{tester_agent_id}",
                )
            )
            emit(
                "test.local.completed",
                f"Local test for {item.work_item_id} completed",
                round=cache.round,
                work_item_id=item.work_item_id,
                tester_agent_id=test_record.tester_agent_id,
                test_result_ref=test_record.test_result_ref,
            )

    def _planner_order_from_initial_data(self, data: dict[str, Any]) -> PlannerOrder | None:
        value = data.get("planner_order")
        if not isinstance(value, dict):
            return None
        return PlannerOrder.model_validate(value)

    def _mock_planner_order(self, request: str) -> PlannerOrder:
        work_items = []
        testers = [agent for agent in self.agent_workflow.agents if _is_tester(agent)]
        workers = [
            agent
            for agent in self.agent_workflow.agents
            if agent.id != self.agent_workflow.primary_planner_id and not _is_tester(agent)
        ]
        tester_ids = [agent.id for agent in testers]
        for index, agent in enumerate(workers, start=1):
            work_items.append(
                {
                    "work_item_id": f"{_safe_id(agent.id)}-work",
                    "order_index": index,
                    "assignee_agent_id": agent.id,
                    "task_summary": f"Phase 3 mock task for {agent.name or agent.id}.",
                    "depends_on": [],
                    "tester_agent_ids": tester_ids,
                }
            )
        return PlannerOrder.model_validate(
            {
                "artifact_type": "planner_order",
                "round": 1,
                "round_goal": request,
                "plan_graph": {
                    "work_items": work_items,
                    "final_tester_agent_id": tester_ids[-1] if len(tester_ids) > 1 else None,
                },
            }
        )

    def _mock_planner_input_bundle(self, cache: GraphRunCache) -> dict[str, Any]:
        return {
            "artifact_type": "planner_input_bundle",
            "round": cache.round,
            "planner_order_ref": cache.plan_cache.planner_order_ref if cache.plan_cache else "memory:planner_order:round-1",
            "plan_status": "completed",
            "items": [
                {
                    "work_item_id": item.work_item_id,
                    "order_index": item.order_index,
                    "task_summary": item.task_summary,
                    "execution_status": cache.execution_cache[item.work_item_id].status
                    if item.work_item_id in cache.execution_cache
                    else "not_started",
                    "execution_summary": cache.execution_cache[item.work_item_id].execution_summary
                    if item.work_item_id in cache.execution_cache
                    else "",
                    "test_status": _merged_test_status(cache.test_cache.get(item.work_item_id, []), item.tester_agent_ids),
                    "test_summary": _merged_test_summary(cache.test_cache.get(item.work_item_id, []), item.tester_agent_ids),
                    "refs": cache.refs_for_work_item(item.work_item_id),
                }
                for item in cache.work_items()
            ],
        }

    def _mock_round_summary(self, cache: GraphRunCache) -> dict[str, Any]:
        ordered_state = [
            {
                "work_item_id": item.work_item_id,
                "order_index": item.order_index,
                "status": "completed",
                "summary": "Phase 2 mock item completed through ExecutionCache/TestCache.",
                "refs": cache.refs_for_work_item(item.work_item_id),
            }
            for item in cache.work_items()
        ]
        return {
            "artifact_type": "round_summary",
            "round": cache.round,
            "planner_order_ref": cache.plan_cache.planner_order_ref if cache.plan_cache else "memory:planner_order:round-1",
            "plan_status": "completed",
            "completed_count": len(ordered_state),
            "failed_count": 0,
            "blocked_count": 0,
            "ordered_state": ordered_state,
            "remaining_work": [],
            "carry_forward_constraints": [],
        }


def _is_tester(agent: AgentWorkflowAgent) -> bool:
    return agent.role in {"tester", "reviewer"} or any("test" in capability for capability in agent.capabilities)


def _merged_test_status(records: list[TestRecord], tester_agent_ids: list[str]) -> str:
    if not tester_agent_ids:
        return "not_requested"
    if any(record.status == "fail" for record in records):
        return "fail"
    if any(record.status == "blocked" for record in records):
        return "blocked"
    return "pass" if records else "not_requested"


def _merged_test_summary(records: list[TestRecord], tester_agent_ids: list[str]) -> str:
    if not tester_agent_ids:
        return ""
    if not records:
        return "Test was requested but no TestRecord was written."
    return " ".join(record.test_summary for record in records)


def _max_concurrency_from_data(data: dict[str, Any]) -> int:
    value = data.get("max_concurrency")
    try:
        return max(1, int(value))
    except (TypeError, ValueError):
        return 4


def _safe_id(value: str) -> str:
    safe = "".join(char if char.isalnum() or char == "_" else "_" for char in value.strip())
    return safe or "agent"
