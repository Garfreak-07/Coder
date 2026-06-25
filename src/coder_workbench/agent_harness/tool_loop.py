from __future__ import annotations

import json
import hashlib
from concurrent.futures import ThreadPoolExecutor
from typing import Any

from pydantic import ValidationError

from coder_workbench.actions import ActionGateway, ActionResult, RunContext
from coder_workbench.actions.result_budget import ResultBudget, apply_result_budget
from coder_workbench.agent_graph.artifacts import graph_artifact_id
from coder_workbench.agent_graph.repair import parse_json_object
from coder_workbench.agent_graph.schema import AgentTaskEnvelope, WorkItem
from coder_workbench.agent_harness.action_protocol import HarnessActionBatch, HarnessActionRequest, HarnessObservation
from coder_workbench.agent_harness.artifact_repair_pipeline import ArtifactRepairPipeline, RepairContext
from coder_workbench.agent_harness.context_preprocessor import CodeWorkerContextPreprocessor
from coder_workbench.agent_harness.execution_verification import ensure_blocked_contract, ensure_execution_verification
from coder_workbench.agent_harness.recovery_policy import RecoveryPolicy
from coder_workbench.agent_harness.self_check import ExecutorSelfChecker
from coder_workbench.agent_harness.session import CodeWorkerLoopState, HarnessSession
from coder_workbench.agent_harness.stop_gate import StopGate
from coder_workbench.agent_harness.tool_batcher import ToolBatcher
from coder_workbench.agent_harness.tool_gate import ToolGate
from coder_workbench.core.artifacts import validate_artifact


class CodeWorkerToolLoop:
    def __init__(
        self,
        *,
        model: Any | None,
        action_gateway: ActionGateway,
        run_context: RunContext,
        tool_gate: ToolGate,
        repair_pipeline: ArtifactRepairPipeline | None = None,
        self_checker: ExecutorSelfChecker | None = None,
        max_turns: int = 16,
        emit: Any | None = None,
    ) -> None:
        self.model = model
        self.action_gateway = action_gateway
        self.run_context = run_context
        self.tool_gate = tool_gate
        self.repair_pipeline = repair_pipeline or ArtifactRepairPipeline()
        self.self_checker = self_checker or ExecutorSelfChecker(self.repair_pipeline)
        self.recovery_policy = RecoveryPolicy()
        self.stop_gate = StopGate()
        self.context_preprocessor = CodeWorkerContextPreprocessor()
        self.result_budget = ResultBudget(max_inline_chars=4000, preview_chars=1200)
        self.tool_batcher = ToolBatcher()
        self.max_turns = max(1, max_turns)
        self.emit = emit

    def run(
        self,
        *,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        prompt: str,
    ) -> dict[str, Any]:
        state = CodeWorkerLoopState(
            session=HarnessSession(
                run_id=self.run_context.run_id,
                round=envelope.round,
                work_item_id=item.work_item_id,
                agent_id=item.assignee_agent_id,
                merge_index=item.merge_index,
                task_summary=item.task_summary,
                capability_set=dict(envelope.capability_set or {}),
                coding_context_packet=dict(envelope.coding_context_packet or {}),
            ),
            max_turns=self.max_turns,
        )
        _emit(
            self.emit,
            "code_worker.loop.started",
            "CodeWorker tool loop started",
            run_id=state.session.run_id,
            round=state.session.round,
            work_item_id=state.session.work_item_id,
            agent_id=state.session.agent_id,
        )
        if self.model is None:
            return self._finalize_execution_result(
                {
                    "artifact_type": "execution_result",
                    "status": "completed",
                    "summary": "CodeWorkerToolLoop mock completed a dry-run execution.",
                    "no_op_rationale": "No model was configured, so no tool actions were requested.",
                },
                state=state,
                item=item,
                envelope=envelope,
            )

        while state.turn_count < state.max_turns:
            state.turn_count += 1
            _emit(
                self.emit,
                "code_worker.loop.model_call.started",
                "CodeWorker model step started",
                run_id=state.session.run_id,
                round=state.session.round,
                work_item_id=state.session.work_item_id,
                agent_id=state.session.agent_id,
                turn_count=state.turn_count,
            )
            response = self.model.invoke(self._prompt(prompt, item, envelope, state))
            content = str(getattr(response, "content", response))
            state.last_model_output = content
            payload = parse_json_object(content)
            if payload is None:
                if self._recoverable_model_output_error(state, "Model output was not a JSON object.", "invalid_json"):
                    continue
                return self._blocked_result(
                    state,
                    item,
                    envelope,
                    "Model output was not valid JSON after one correction.",
                    "schema_validation_failed",
                )

            _emit(
                self.emit,
                "code_worker.loop.model_call.completed",
                "CodeWorker model step completed",
                run_id=state.session.run_id,
                round=state.session.round,
                work_item_id=state.session.work_item_id,
                agent_id=state.session.agent_id,
                turn_count=state.turn_count,
                artifact_type=payload.get("artifact_type"),
            )
            artifact_type = str(payload.get("artifact_type") or "")
            if artifact_type == "execution_result":
                final = self._finalize_execution_result(payload, state=state, item=item, envelope=envelope)
                if final is None:
                    continue
                return final
            if artifact_type in {"planner_decision", "final_report"}:
                return self._blocked_result(
                    state,
                    item,
                    envelope,
                    f"Executor attempted to return planner-only artifact_type: {artifact_type}",
                    "permission_boundary",
                )
            if artifact_type == "harness_action_batch":
                try:
                    action_batch = HarnessActionBatch.model_validate(payload)
                except ValidationError as exc:
                    if self._recoverable_model_output_error(
                        state,
                        "Model returned malformed harness_action_batch.",
                        "invalid_action_schema",
                        payload_preview={"errors": exc.errors()[:8]},
                    ):
                        continue
                    return self._blocked_result(
                        state,
                        item,
                        envelope,
                        "Model returned malformed harness_action_batch after one correction.",
                        "schema_validation_failed",
                    )
                result = self._process_action_requests(action_batch.actions, state=state, item=item, envelope=envelope)
                if result is not None:
                    return result
                continue
            if artifact_type != "harness_action":
                if self._recoverable_model_output_error(
                    state,
                    f"Model returned unsupported artifact_type: {artifact_type or 'missing'}",
                    "invalid_artifact_type",
                ):
                    continue
                return self._blocked_result(
                    state,
                    item,
                    envelope,
                    "Model did not return harness_action or execution_result after one correction.",
                    "schema_validation_failed",
                )

            try:
                request = HarnessActionRequest.model_validate(payload)
            except ValidationError as exc:
                if self._recoverable_model_output_error(
                    state,
                    "Model returned malformed harness_action.",
                    "invalid_action_schema",
                    payload_preview={"errors": exc.errors()[:8]},
                ):
                    continue
                return self._blocked_result(
                    state,
                    item,
                    envelope,
                    "Model returned malformed harness_action after one correction.",
                    "schema_validation_failed",
                )

            result = self._process_action_requests([request], state=state, item=item, envelope=envelope)
            if result is not None:
                return result
            continue

        return self._blocked_result(
            state,
            item,
            envelope,
            f"CodeWorker tool loop reached max_turns={state.max_turns}.",
            "timeout",
        )

    def _process_action_requests(
        self,
        requests: list[HarnessActionRequest],
        *,
        state: CodeWorkerLoopState,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
    ) -> dict[str, Any] | None:
        try:
            batches = self.tool_batcher.partition(requests)
        except KeyError as exc:
            request = next(
                (
                    item
                    for item in requests
                    if item.action_type not in self.tool_batcher.metadata_registry.names()
                ),
                requests[0],
            )
            decision = self.tool_gate.decide(request)
            if not decision.allowed and decision.observation is not None:
                self._record_blocked_action(state, request, decision.observation, decision.reason, decision.error_code)
                return self._blocked_result(
                    state,
                    item,
                    envelope,
                    decision.reason or "CodeWorker action was blocked.",
                    _blocker_type_for_error(decision.error_code),
                )
            observation = HarnessObservation(
                action_id=request.action_id,
                action_type=request.action_type,
                status="blocked",
                summary=str(exc),
                evidence_refs=[f"harness_observation:{request.action_id}"],
                error_code="unknown_action_type",
            )
            self._record_observation(state, request, observation)
            return self._blocked_result(
                state,
                item,
                envelope,
                str(exc),
                "tool_unavailable",
            )

        processed_action_ids: set[str] = set()
        for batch in batches:
            if batch.execution_mode == "concurrent":
                blocked = self._process_concurrent_action_batch(batch.actions, state=state, item=item, envelope=envelope)
                processed_action_ids.update(action.action_id for action in batch.actions)
                if blocked is not None:
                    return blocked
                continue

            request = batch.actions[0]
            decision = self.tool_gate.decide(request)
            if not decision.allowed:
                assert decision.observation is not None
                self._record_blocked_action(state, request, decision.observation, decision.reason, decision.error_code)
                return self._blocked_result(
                    state,
                    item,
                    envelope,
                    decision.reason or "CodeWorker action was blocked.",
                    _blocker_type_for_error(decision.error_code),
                )

            if request.action_type == "return_execution_result":
                final_payload = request.payload.get("artifact")
                if not isinstance(final_payload, dict):
                    final_payload = dict(request.payload)
                final_payload["artifact_type"] = "execution_result"
                final = self._finalize_execution_result(final_payload, state=state, item=item, envelope=envelope)
                if final is None:
                    return None
                return final

            assert decision.action_spec is not None
            self._emit_action_allowed(state, request)
            result = self.action_gateway.run(decision.action_spec, run_context=self.run_context)
            observation = self._record_action_result(state, request, result)
            processed_action_ids.add(request.action_id)
            if observation.status != "ok":
                remaining = [action for action in requests if action.action_id not in processed_action_ids]
                self._record_skipped_actions(state, remaining, failed_action_id=request.action_id)
                state.transition = {
                    "reason": "exclusive_action_failed",
                    "from_action_id": request.action_id,
                    "error_code": observation.error_code,
                }
                return None

        state.transition = {
            "reason": "next_turn",
            "from_action_ids": [request.action_id for request in requests],
        }
        return None

    def _process_concurrent_action_batch(
        self,
        requests: list[HarnessActionRequest],
        *,
        state: CodeWorkerLoopState,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
    ) -> dict[str, Any] | None:
        decisions = [(request, self.tool_gate.decide(request)) for request in requests]
        for request, decision in decisions:
            if not decision.allowed:
                assert decision.observation is not None
                self._record_blocked_action(state, request, decision.observation, decision.reason, decision.error_code)
                return self._blocked_result(
                    state,
                    item,
                    envelope,
                    decision.reason or "CodeWorker action was blocked.",
                    _blocker_type_for_error(decision.error_code),
                )

        for request, _decision in decisions:
            self._emit_action_allowed(state, request)

        max_workers = min(4, max(1, len(decisions)))
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            futures = [
                (
                    request,
                    executor.submit(self.action_gateway.run, decision.action_spec, run_context=self.run_context),
                )
                for request, decision in decisions
                if decision.action_spec is not None
            ]
            for request, future in futures:
                self._record_action_result(state, request, future.result())
        return None

    def _record_blocked_action(
        self,
        state: CodeWorkerLoopState,
        request: HarnessActionRequest,
        observation: HarnessObservation,
        reason: str,
        error_code: str | None,
    ) -> None:
        self._record_observation(state, request, observation)
        _emit(
            self.emit,
            "code_worker.loop.action.blocked",
            reason,
            run_id=state.session.run_id,
            round=state.session.round,
            work_item_id=state.session.work_item_id,
            agent_id=state.session.agent_id,
            turn_count=state.turn_count,
            action_id=request.action_id,
            action_type=request.action_type,
            error_code=error_code,
        )

    def _emit_action_allowed(self, state: CodeWorkerLoopState, request: HarnessActionRequest) -> None:
        _emit(
            self.emit,
            "code_worker.loop.action.allowed",
            "CodeWorker action allowed",
            run_id=state.session.run_id,
            round=state.session.round,
            work_item_id=state.session.work_item_id,
            agent_id=state.session.agent_id,
            turn_count=state.turn_count,
            action_id=request.action_id,
            action_type=request.action_type,
        )

    def _record_action_result(
        self,
        state: CodeWorkerLoopState,
        request: HarnessActionRequest,
        result: ActionResult,
    ) -> HarnessObservation:
        observation = self._observation_from_result(request, result)
        self._record_observation(state, request, observation)
        _emit(
            self.emit,
            "code_worker.loop.action.executed",
            observation.summary,
            run_id=state.session.run_id,
            round=state.session.round,
            work_item_id=state.session.work_item_id,
            agent_id=state.session.agent_id,
            turn_count=state.turn_count,
            action_id=request.action_id,
            action_type=request.action_type,
            status=observation.status,
            error_code=observation.error_code,
            evidence_refs=observation.evidence_refs,
        )
        return observation

    def _record_skipped_actions(
        self,
        state: CodeWorkerLoopState,
        requests: list[HarnessActionRequest],
        *,
        failed_action_id: str,
    ) -> None:
        for request in requests:
            observation = HarnessObservation(
                action_id=request.action_id,
                action_type=request.action_type,
                status="blocked",
                summary=f"Skipped because prior exclusive action failed: {failed_action_id}.",
                evidence_refs=[f"harness_observation:{request.action_id}"],
                error_code="skipped_after_failed_exclusive_action",
            )
            self._record_observation(state, request, observation)

    def _prompt(
        self,
        base_prompt: str,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        state: CodeWorkerLoopState,
    ) -> str:
        from coder_workbench.agent_graph.prompts import build_worker_tool_loop_prompt

        prepared_context = self.context_preprocessor.prepare(item=item, envelope=envelope, state=state)
        return build_worker_tool_loop_prompt(
            base_prompt=base_prompt,
            item=item,
            envelope=envelope,
            prepared_context=prepared_context.model_dump(mode="json"),
            capability_set=state.session.capability_set,
        )

    def _recoverable_model_output_error(
        self,
        state: CodeWorkerLoopState,
        summary: str,
        error_code: str,
        payload_preview: dict[str, Any] | None = None,
    ) -> bool:
        decision = self.recovery_policy.decide(error_code, attempts=state.session.recovery_attempts)
        state.max_output_recovery_count += 1
        observation = HarnessObservation(
            action_id=f"model-output-{state.turn_count}",
            action_type="model_step",
            status="blocked",
            summary=summary,
            evidence_refs=[f"harness_observation:model-output-{state.turn_count}"],
            payload_preview={"next_instruction": decision.next_instruction, **(payload_preview or {})},
            error_code=error_code,
        )
        state.session.observations.append(observation)
        state.session.blocked_reasons.append(summary)
        state.session.recovery_attempts.append(
            {
                "error_code": error_code,
                "reason": summary,
                "turn_count": state.turn_count,
                "recoverable": decision.recoverable,
            }
        )
        state.transition = {"reason": error_code, "recoverable": decision.recoverable}
        _emit(
            self.emit,
            "code_worker.loop.recovery.scheduled",
            summary,
            run_id=state.session.run_id,
            round=state.session.round,
            work_item_id=state.session.work_item_id,
            agent_id=state.session.agent_id,
            turn_count=state.turn_count,
            error_code=error_code,
        )
        return decision.recoverable

    def _observation_from_result(self, request: HarnessActionRequest, result: ActionResult) -> HarnessObservation:
        status = result.status
        error_code = result.error_code
        result_payload, budget_refs = apply_result_budget(
            dict(result.payload),
            data=self.run_context.mutable_data,
            run_id=self.run_context.run_id,
            action_id=request.action_id,
            action_type=request.action_type,
            budget=self.result_budget,
        )
        if budget_refs:
            result_payload.setdefault("result_budget", {})["externalized_refs"] = budget_refs
        result_payload, aggregate_ref = self._externalize_payload_if_large(request, result_payload)
        if aggregate_ref:
            budget_refs.append(aggregate_ref)
        payload = _json_preview(result_payload)
        if request.action_type == "run_command_sandbox":
            command_result = result_payload.get("result") if isinstance(result_payload.get("result"), dict) else {}
            if command_result and command_result.get("passed") is False and status == "ok":
                status = "failed"
                error_code = "command_failed"
        output_ref = result.output_ref or _first_externalized_ref(result_payload)
        summary = _bounded_result_summary(
            result.summary or f"{request.action_type} completed with status {status}.",
            action_type=request.action_type,
            status=status,
            output_ref=output_ref,
        )
        evidence_refs = [f"harness_observation:{request.action_id}"]
        if output_ref:
            evidence_refs.append(output_ref)
        return HarnessObservation(
            action_id=request.action_id,
            action_type=request.action_type,
            status=status,
            summary=summary,
            output_ref=output_ref,
            evidence_refs=_unique(evidence_refs),
            payload_preview=payload,
            error_code=error_code,
        )

    def _externalize_payload_if_large(
        self,
        request: HarnessActionRequest,
        payload: dict[str, Any],
    ) -> tuple[dict[str, Any], str | None]:
        text = json.dumps(payload, ensure_ascii=False, default=str)
        if len(text) <= self.result_budget.max_inline_chars:
            return payload, None
        digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
        blob_id = f"sha256:{digest}"
        pending = self.run_context.mutable_data.setdefault("pending_blob_writes", {})
        if isinstance(pending, dict):
            pending[blob_id] = {
                "blob_id": blob_id,
                "ref_type": "tool-result",
                "field_path": "payload",
                "preview": text[: self.result_budget.preview_chars],
                "content": text,
                "original_chars": len(text),
                "media_type": "application/json; charset=utf-8",
            }
        return (
            {
                "output_ref": blob_id,
                "preview": text[: self.result_budget.preview_chars],
                "truncated": True,
                "original_chars": len(text),
                "result_budget": {
                    "externalized_refs": [
                        blob_id,
                    ],
                    "action_id": request.action_id,
                    "action_type": request.action_type,
                },
            },
            blob_id,
        )

    def _record_observation(
        self,
        state: CodeWorkerLoopState,
        request: HarnessActionRequest,
        observation: HarnessObservation,
    ) -> None:
        session = state.session
        session.observations.append(observation)
        session.evidence_refs = _unique([*session.evidence_refs, *observation.evidence_refs])
        if observation.status != "ok":
            session.blocked_reasons.append(observation.summary)
        payload = observation.payload_preview
        if request.action_type == "read_file" and observation.status == "ok":
            path = str(request.payload.get("path") or "").strip()
            session.opened_files = _unique([*session.opened_files, path] if path else session.opened_files)
        elif request.action_type == "search_files" and observation.status == "ok":
            query = str(request.payload.get("query") or "").strip()
            session.searched_patterns = _unique([*session.searched_patterns, query] if query else session.searched_patterns)
        elif request.action_type == "propose_patch" and observation.status == "ok":
            preview = payload.get("preview") if isinstance(payload.get("preview"), dict) else {}
            patch_id = str(preview.get("patch_id") or "").strip()
            if patch_id:
                session.patch_refs = _unique([*session.patch_refs, patch_id])
        elif request.action_type == "apply_patch_sandbox" and observation.status == "ok":
            result = payload.get("result") if isinstance(payload.get("result"), dict) else {}
            snapshot_id = str(result.get("snapshot_id") or "").strip()
            if snapshot_id:
                session.patch_refs = _unique([*session.patch_refs, snapshot_id])
            for item in result.get("applied") or []:
                if not isinstance(item, dict):
                    continue
                path = str(item.get("path") or "").strip()
                action = str(item.get("action") or "").strip()
                if not path:
                    continue
                if action == "create":
                    session.created_files = _unique([*session.created_files, path])
                elif action == "delete":
                    session.deleted_files = _unique([*session.deleted_files, path])
                else:
                    session.changed_files = _unique([*session.changed_files, path])
        elif request.action_type == "run_command_sandbox":
            check = _command_check_from_observation(request, observation, payload)
            session.command_checks.append(check)

    def _finalize_execution_result(
        self,
        payload: dict[str, Any],
        *,
        state: CodeWorkerLoopState,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
    ) -> dict[str, Any] | None:
        state.stop_gate_active = True
        _emit(
            self.emit,
            "code_worker.loop.stop_gate.started",
            "CodeWorker stop gate started",
            run_id=state.session.run_id,
            round=state.session.round,
            work_item_id=state.session.work_item_id,
            agent_id=state.session.agent_id,
        )
        gate_decision = self.stop_gate.evaluate(payload, state=state, item=item, envelope=envelope)
        if not gate_decision.accepted:
            if gate_decision.recoverable and gate_decision.observation is not None:
                recovery = self.recovery_policy.decide(
                    gate_decision.error_code or "stop_gate_failed",
                    attempts=state.session.recovery_attempts,
                )
                state.session.stop_gate_failures.append(
                    {
                        "turn_count": state.turn_count,
                        "reason": gate_decision.reason,
                        "error_code": gate_decision.error_code,
                        "recoverable": recovery.recoverable,
                    }
                )
                state.session.recovery_attempts.append(
                    {
                        "turn_count": state.turn_count,
                        "reason": gate_decision.reason,
                        "error_code": gate_decision.error_code or "stop_gate_failed",
                        "next_instruction": recovery.next_instruction,
                        "recoverable": recovery.recoverable,
                    }
                )
                if recovery.recoverable:
                    observation = gate_decision.observation.model_copy(
                        update={
                            "payload_preview": {
                                **gate_decision.observation.payload_preview,
                                "next_instruction": recovery.next_instruction,
                            }
                        }
                    )
                    state.session.observations.append(observation)
                    state.session.blocked_reasons.append(gate_decision.reason)
                    state.transition = {
                        "reason": "stop_gate_retry",
                        "error_code": gate_decision.error_code,
                        "turn_count": state.turn_count,
                    }
                    _emit(
                        self.emit,
                        "code_worker.loop.stop_gate.failed",
                        gate_decision.reason,
                        run_id=state.session.run_id,
                        round=state.session.round,
                        work_item_id=state.session.work_item_id,
                        agent_id=state.session.agent_id,
                        error_code=gate_decision.error_code,
                    )
                    return None
            return self._blocked_result(
                state,
                item,
                envelope,
                gate_decision.reason or "Stop gate rejected candidate execution_result.",
                _blocker_type_for_error(gate_decision.error_code),
            )

        enriched = self._enrich_final_payload(payload, state, item, envelope)
        outcome = self.repair_pipeline.repair(
            expected_type="execution_result",
            invalid_output=json.dumps(enriched, ensure_ascii=False, default=str),
            parsed_payload=enriched,
            model=self.model,
            context=RepairContext(
                agent_id=item.assignee_agent_id,
                work_item_id=item.work_item_id,
                merge_index=item.merge_index,
                round_number=envelope.round,
                emit=self.emit,
                schema_notes="Return a valid execution_result JSON object with verification.",
            ),
        )
        artifact = outcome.artifact
        if artifact is None:
            return self._blocked_result(
                state,
                item,
                envelope,
                "Stop gate could not produce a valid execution_result.",
                "schema_validation_failed",
            )
        checked = self.self_checker.check(
            artifact,
            item=item,
            envelope=envelope,
            model=self.model,
            emit=self.emit,
        )
        artifact = checked.artifact
        _emit(
            self.emit,
            "code_worker.loop.stop_gate.passed" if checked.status == "ok" else "code_worker.loop.stop_gate.failed",
            "CodeWorker stop gate completed",
            run_id=state.session.run_id,
            round=state.session.round,
            work_item_id=state.session.work_item_id,
            agent_id=state.session.agent_id,
            status=artifact.get("status"),
            error_code=artifact.get("blocker_type"),
        )
        _emit(
            self.emit,
            "code_worker.loop.completed" if artifact.get("status") == "completed" else "code_worker.loop.blocked",
            str(artifact.get("summary") or "CodeWorker tool loop completed."),
            run_id=state.session.run_id,
            round=state.session.round,
            work_item_id=state.session.work_item_id,
            agent_id=state.session.agent_id,
            status=artifact.get("status"),
        )
        return artifact

    def _enrich_final_payload(
        self,
        payload: dict[str, Any],
        state: CodeWorkerLoopState,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
    ) -> dict[str, Any]:
        session = state.session
        enriched = dict(payload)
        enriched.update(
            {
                "artifact_type": "execution_result",
                "round": envelope.round,
                "work_item_id": item.work_item_id,
                "merge_index": item.merge_index,
                "agent_id": item.assignee_agent_id,
                "changed_files": session.changed_files,
                "created_files": session.created_files,
                "deleted_files": session.deleted_files,
                "patch_refs": session.patch_refs,
                "evidence_refs": session.evidence_refs,
                "outputs": session.evidence_refs,
                "requested_actions": [
                    {
                        "action_id": observation.action_id,
                        "action_type": observation.action_type,
                        "status": observation.status,
                        "summary": observation.summary,
                        "output_ref": observation.output_ref,
                    }
                    for observation in session.observations
                    if observation.action_type != "model_step"
                ],
                "attempted_actions": [
                    observation.action_type
                    for observation in session.observations
                    if observation.action_type != "model_step"
                ],
            }
        )
        command_checks = list(session.command_checks)
        if command_checks:
            last_status = str(command_checks[-1].get("status") or "skipped")
            verification_status = "pass" if last_status == "pass" else "blocked" if last_status == "blocked" else "fail"
        elif session.evidence_refs:
            verification_status = "pass"
        else:
            verification_status = "skipped"
            enriched.setdefault("no_op_rationale", "No runtime tool action was needed for this work item.")
        if verification_status in {"fail", "blocked"}:
            enriched["status"] = "blocked"
            enriched.setdefault("unexpected_issues", [verification_status])
            enriched.setdefault("remaining_work", [str(enriched.get("summary") or "Resolve failed CodeWorker check.")])
            enriched.setdefault("needs_planner_decision", True)
            enriched.setdefault("blocker_type", "command_failed" if verification_status == "fail" else "tool_unavailable")
            enriched.setdefault("continue_without_human_possible", True)
        verification = {
            "status": verification_status,
            "checks_run": command_checks,
            "evidence_refs": session.evidence_refs,
            "confidence": "medium" if verification_status == "pass" else "low",
            "remaining_work": [] if verification_status in {"pass", "skipped"} else list(enriched.get("remaining_work") or []),
            "no_check_rationale": enriched.get("no_op_rationale") if verification_status == "skipped" else None,
            "repair_attempted": False,
            "repair_summary": None,
        }
        enriched["verification"] = verification
        return ensure_execution_verification(enriched)

    def _blocked_result(
        self,
        state: CodeWorkerLoopState,
        item: WorkItem,
        envelope: AgentTaskEnvelope,
        summary: str,
        blocker_type: str,
    ) -> dict[str, Any]:
        session = state.session
        payload = ensure_blocked_contract(
            {
                "artifact_type": "execution_result",
                "round": envelope.round,
                "work_item_id": item.work_item_id,
                "merge_index": item.merge_index,
                "agent_id": item.assignee_agent_id,
                "status": "blocked",
                "summary": summary,
                "changed_files": session.changed_files,
                "created_files": session.created_files,
                "deleted_files": session.deleted_files,
                "patch_refs": session.patch_refs,
                "outputs": session.evidence_refs,
                "evidence_refs": session.evidence_refs,
                "requested_actions": [
                    observation.model_dump(mode="json", exclude_none=True)
                    for observation in session.observations
                ],
                "attempted_actions": [
                    observation.action_type
                    for observation in session.observations
                    if observation.action_type != "model_step"
                ],
                "unexpected_issues": [blocker_type],
                "remaining_work": [summary],
                "needs_planner_decision": True,
                "blocker_type": blocker_type,
                "continue_without_human_possible": blocker_type not in {"permission_boundary", "risk_path_blocked"},
                "verification": {
                    "status": "blocked",
                    "checks_run": session.command_checks,
                    "evidence_refs": session.evidence_refs,
                    "confidence": "low",
                    "remaining_work": [summary],
                    "no_check_rationale": None,
                    "repair_attempted": False,
                    "repair_summary": None,
                },
            }
        )
        _emit(
            self.emit,
            "code_worker.loop.blocked",
            summary,
            run_id=state.session.run_id,
            round=state.session.round,
            work_item_id=state.session.work_item_id,
            agent_id=state.session.agent_id,
            error_code=blocker_type,
        )
        return validate_artifact(payload, expected_type="execution_result")


def _command_check_from_observation(
    request: HarnessActionRequest,
    observation: HarnessObservation,
    payload: dict[str, Any],
) -> dict[str, Any]:
    command_result = payload.get("result") if isinstance(payload.get("result"), dict) else {}
    if observation.status == "ok":
        status = "pass"
    elif observation.status == "blocked":
        status = "blocked"
    else:
        status = "fail"
    return {
        "check_id": request.action_id,
        "kind": "command",
        "command": str(command_result.get("command") or request.payload.get("command") or ""),
        "status": status,
        "summary": observation.summary,
        "output_ref": observation.output_ref,
        "evidence_refs": observation.evidence_refs,
    }


def _identity_issue(payload: dict[str, Any], item: WorkItem, envelope: AgentTaskEnvelope) -> str:
    if payload.get("work_item_id") not in {None, "", item.work_item_id}:
        return "execution_result work_item_id does not match assigned WorkItem"
    if payload.get("merge_index") not in {None, item.merge_index}:
        return "execution_result merge_index does not match assigned WorkItem"
    if payload.get("agent_id") not in {None, "", item.assignee_agent_id, envelope.assigned_agent_id}:
        return "execution_result agent_id does not match assigned agent"
    return ""


def _first_externalized_ref(payload: dict[str, Any]) -> str | None:
    refs = _collect_externalized_refs(payload)
    return refs[0] if refs else None


def _collect_externalized_refs(value: Any) -> list[str]:
    refs: list[str] = []
    if isinstance(value, dict):
        blob_id = value.get("blob_id")
        if isinstance(blob_id, str) and blob_id.startswith("sha256:"):
            refs.append(blob_id)
        for item in value.values():
            refs.extend(_collect_externalized_refs(item))
    elif isinstance(value, list):
        for item in value:
            refs.extend(_collect_externalized_refs(item))
    return _unique(refs)


def _json_preview(value: Any, *, max_chars: int = 4000) -> dict[str, Any]:
    if isinstance(value, dict):
        payload = value
    else:
        payload = {"value": value}
    try:
        text = json.dumps(payload, ensure_ascii=False, default=str)
    except TypeError:
        text = json.dumps({"value": str(payload)}, ensure_ascii=False)
    if len(text) <= max_chars:
        try:
            parsed = json.loads(text)
            return parsed if isinstance(parsed, dict) else {"value": parsed}
        except json.JSONDecodeError:
            return {"preview": text}
    return {
        "preview": text[:max_chars],
        "truncated": True,
        "original_chars": len(text),
    }


def _bounded_result_summary(
    summary: str,
    *,
    action_type: str,
    status: str,
    output_ref: str | None,
    max_chars: int = 800,
) -> str:
    text = str(summary or "")
    if len(text) <= max_chars:
        return text
    ref_text = f" Output ref: {output_ref}." if output_ref else ""
    return (
        f"{action_type} completed with status {status}; "
        f"summary was {len(text)} chars and was truncated.{ref_text}"
    )


def _blocker_type_for_error(error_code: str | None) -> str:
    return {
        "scope_violation": "scope_violation",
        "risk_path_blocked": "risk_path_blocked",
        "permission_boundary": "permission_boundary",
        "capability_denied": "permission_boundary",
        "command_failed": "command_failed",
        "unknown_action_type": "tool_unavailable",
        "invalid_action_payload": "tool_unavailable",
        "invalid_json": "schema_validation_failed",
        "invalid_action_schema": "schema_validation_failed",
        "invalid_artifact_type": "schema_validation_failed",
        "schema_validation_failed": "schema_validation_failed",
        "stop_gate_failed": "schema_validation_failed",
    }.get(str(error_code or ""), "unknown_error")


def _unique(values: list[str]) -> list[str]:
    seen: set[str] = set()
    output: list[str] = []
    for value in values:
        text = str(value or "").strip()
        if not text or text in seen:
            continue
        seen.add(text)
        output.append(text)
    return output


def _emit(emit: Any | None, event_type: str, message: str, **payload: Any) -> None:
    if emit is not None:
        emit(event_type, message, **{key: value for key, value in payload.items() if value is not None})
