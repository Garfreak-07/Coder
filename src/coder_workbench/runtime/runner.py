from __future__ import annotations

import json
from typing import Any
from uuid import uuid4

from coder_workbench.core.artifacts import ArtifactValidationError, artifact_summary, supported_artifact_types, validate_artifact
from coder_workbench.core import WorkflowSpec
from coder_workbench.executors import AgentExecutor, DefaultAgentExecutor
from coder_workbench.runtime.conditions import evaluate_condition
from coder_workbench.runtime.context import build_agent_context, build_context_packet, fit_context_to_token_budget
from coder_workbench.runtime.state import RunEvent, RunResult, RunState, summarize_value
from coder_workbench.tools import ToolRegistry, default_tool_registry


class WorkflowRunner:
    """Small JSON-driven workflow interpreter.

    This is the product kernel candidate: canvas edges become real routing,
    agents are adapter-backed, and token budget pressure is tracked centrally.
    """

    def __init__(
        self,
        workflow: WorkflowSpec,
        agent_executor: AgentExecutor | None = None,
        tools: ToolRegistry | None = None,
        event_sink: Any | None = None,
        runtime_settings: Any | None = None,
    ) -> None:
        self.workflow = workflow
        self.nodes = workflow.node_by_id()
        self.agents = workflow.agent_by_id()
        self.agent_executor = agent_executor or DefaultAgentExecutor(runtime_settings=runtime_settings)
        self.tools = tools or default_tool_registry()
        self.event_sink = event_sink

    def run(
        self,
        request: str,
        repo_root: str,
        initial_data: dict[str, Any] | None = None,
        resume_checkpoint: dict[str, Any] | None = None,
        prior_events: list[RunEvent] | None = None,
        resume_after_node: str | None = None,
    ) -> RunResult:
        state = RunState(
            workflow_id=self.workflow.id,
            request=request,
            repo_root=repo_root,
            data=dict(initial_data or {}),
            token_budget=self.workflow.token_budget,
        )
        if resume_checkpoint:
            state.data.update(resume_checkpoint.get("data", {}))
            state.summaries.update(resume_checkpoint.get("summaries", {}))
            state.visited_nodes.update(resume_checkpoint.get("visited_nodes", {}))
            state.traversed_edges.update(resume_checkpoint.get("traversed_edges", {}))
            state.loop_states.update(resume_checkpoint.get("loop_states", {}))
            state.estimated_tokens_used = int(resume_checkpoint.get("estimated_tokens_used", 0))
            state.agent_calls = int(resume_checkpoint.get("agent_calls", 0))
            state.tool_calls = int(resume_checkpoint.get("tool_calls", 0))
            state.current_node = resume_checkpoint.get("current_node")
        if prior_events:
            state.events.extend(prior_events)
        if self.event_sink:
            state.set_event_sink(self.event_sink)
        state.emit("run.started", f"Workflow {self.workflow.id} {'resumed' if resume_after_node else 'started'}")

        if resume_after_node and self.nodes[resume_after_node].type == "human_gate":
            queue = self._next_nodes(state, resume_after_node)
        elif resume_after_node:
            queue = [resume_after_node]
        else:
            queue = [node.id for node in self.workflow.nodes if node.type == "start"]
        try:
            while queue and state.status == "running":
                if sum(state.visited_nodes.values()) >= self.workflow.max_steps:
                    self._block(state, "max_steps reached")
                    break

                node_id = queue.pop(0)
                node = self.nodes[node_id]
                state.current_node = node_id
                state.visited_nodes[node_id] = state.visited_nodes.get(node_id, 0) + 1
                state.emit("node.started", f"Node {node_id} started", node_id=node_id, node_type=node.type)

                if node.type == "start":
                    result = {"status": "started"}
                elif node.type == "end":
                    result = {"status": "completed"}
                    state.status = "completed"
                elif node.type == "agent":
                    result = self._run_agent_node(state, node_id)
                elif node.type in {"tool", "mcp_tool"}:
                    result = self._run_tool_node(state, node_id)
                elif node.type == "condition":
                    result = {"passed": evaluate_condition(node.condition, state.data)}
                    state.set_value(node.output_key or node.id, result)
                elif node.type == "loop":
                    result = self._run_loop_node(state, node_id)
                elif node.type == "human_gate":
                    result = self._run_human_gate(state, node_id)
                else:  # pragma: no cover - pydantic prevents this
                    raise ValueError(f"Unsupported node type: {node.type}")

                state.emit(
                    "node.completed",
                    f"Node {node_id} completed",
                    node_id=node_id,
                    result_summary=summarize_value(result),
                    result_status=result.get("status") if isinstance(result, dict) else None,
                    result_keys=sorted(result.keys()) if isinstance(result, dict) else None,
                    result_size_chars=len(str(result)),
                )

                if state.status != "running":
                    break
                queue.extend(self._next_nodes(state, node_id))

            if state.status == "running":
                state.status = "completed"
            state.emit(f"run.{state.status}", f"Workflow {state.status}")
        except Exception as exc:  # pragma: no cover - boundary safety
            state.status = "failed"
            state.emit("run.failed", f"Workflow failed: {exc}", error=str(exc))

        return RunResult(
            status=state.status,
            data=state.data,
            summaries=state.summaries,
            artifacts=state.artifacts,
            events=state.events,
            estimated_tokens_used=state.estimated_tokens_used,
            agent_calls=state.agent_calls,
            tool_calls=state.tool_calls,
            blocked_node_id=state.current_node if state.status == "blocked" else None,
            resume_checkpoint=self._checkpoint(state) if state.status == "blocked" else None,
        )

    def _run_agent_node(self, state: RunState, node_id: str) -> dict[str, Any]:
        node = self.nodes[node_id]
        assert node.agent_id
        if state.agent_calls >= self.workflow.max_agent_calls:
            self._block(state, "max_agent_calls reached")
            return {"status": "blocked"}
        agent = self.agents[node.agent_id]
        policy_violations = self._agent_tool_policy_violations(agent)
        if policy_violations:
            state.status = "failed"
            return {
                "status": "failed",
                "error": "agent tool policy violation",
                "policy_violations": policy_violations,
            }
        context = build_agent_context(agent, state)
        budget_remaining = state.token_budget - state.estimated_tokens_used if state.token_budget else None
        context, estimated, original_estimated, reductions = fit_context_to_token_budget(context, budget_remaining)
        projected_tokens = state.estimated_tokens_used + estimated
        packet = build_context_packet(
            agent,
            state,
            node_id=node_id,
            context=context,
            estimated_tokens=estimated,
            original_estimated_tokens=original_estimated,
            context_reductions=reductions,
        )
        state.emit(
            "agent.context_packet",
            f"Context packet for agent {agent.id}",
            node_id=node_id,
            packet=packet,
        )
        if reductions:
            state.emit(
                "budget.warning",
                "Context compacted before agent call",
                node_id=node_id,
                estimated_tokens_used=state.estimated_tokens_used,
                original_estimated_tokens=original_estimated,
                estimated_tokens=estimated,
                projected_tokens_used=projected_tokens,
                token_budget=state.token_budget,
                context_reductions=reductions,
            )
        if state.token_budget and projected_tokens > state.token_budget:
            state.emit(
                "budget.warning",
                "Estimated token budget exceeded after compaction; agent call blocked",
                node_id=node_id,
                estimated_tokens_used=state.estimated_tokens_used,
                projected_tokens_used=projected_tokens,
                estimated_tokens=estimated,
                original_estimated_tokens=original_estimated,
                token_budget=state.token_budget,
                context_reductions=reductions,
            )
            self._block(state, "token budget exceeded")
            return {
                "status": "blocked",
                "reason": "token budget exceeded",
                "estimated_tokens": estimated,
                "projected_tokens_used": projected_tokens,
                "token_budget": state.token_budget,
            }
        state.estimated_tokens_used = projected_tokens
        state.agent_calls += 1
        state.emit("agent.called", f"Agent {agent.id} called", node_id=node_id, estimated_tokens=estimated)
        result = self.agent_executor.run(agent, context)
        artifact_result = self._record_agent_artifact(state, node_id, result, expected_type=agent.artifact_type)
        if artifact_result is not None:
            result = artifact_result
            if state.status == "blocked":
                return {
                    "status": "blocked",
                    "reason": "artifact validation failed",
                }
        state.set_value(node.output_key or agent.output_key or node.id, result)
        return result

    def _record_agent_artifact(
        self,
        state: RunState,
        node_id: str,
        result: dict[str, Any],
        *,
        expected_type: str | None,
    ) -> dict[str, Any] | None:
        if not isinstance(result, dict):
            return None
        candidate_type = expected_type or str(result.get("artifact_type") or "")
        if not candidate_type:
            return None
        if candidate_type not in supported_artifact_types():
            state.emit(
                "artifact.validation_failed",
                f"Unsupported artifact type: {candidate_type}",
                node_id=node_id,
                artifact_type=candidate_type,
                errors=[{"loc": ["artifact_type"], "msg": f"unsupported artifact type: {candidate_type}"}],
            )
            self._block(state, "artifact validation failed")
            return result

        artifact_id = f"artifact_{uuid4().hex}"
        try:
            artifact = validate_artifact(result, expected_type=expected_type, artifact_id=artifact_id)
        except ArtifactValidationError as exc:
            state.emit(
                "artifact.validation_failed",
                f"{exc.artifact_type} failed schema validation",
                node_id=node_id,
                artifact_type=exc.artifact_type,
                errors=exc.errors,
            )
            self._block(state, "artifact validation failed")
            return result

        summary = artifact_summary(artifact)
        state.artifacts[artifact_id] = artifact
        state.emit(
            "artifact.produced",
            f"Artifact {artifact_id} produced",
            node_id=node_id,
            artifact_id=artifact_id,
            artifact_type=artifact["artifact_type"],
            summary=summary,
            size_chars=len(json.dumps(artifact, ensure_ascii=False)),
        )
        return artifact

    def _run_loop_node(self, state: RunState, node_id: str) -> dict[str, Any]:
        node = self.nodes[node_id]
        mode = node.loop_mode or "retry_until"
        max_iterations = node.max_iterations or 3
        loop_state = dict(state.loop_states.get(node_id, {}))
        iteration = int(loop_state.get("iteration", 0)) + 1
        current_item: Any | None = None
        should_continue = True
        break_reason: str | None = None

        if iteration == 1:
            state.emit("loop.started", f"Loop {node_id} started", node_id=node_id, mode=mode, max_iterations=max_iterations)

        if iteration > max_iterations:
            should_continue = False
            break_reason = "max_iterations"
            iteration = max_iterations
        elif mode == "for_each":
            items = state.data.get(node.items_key or "", [])
            if not isinstance(items, list):
                items = []
            if iteration > len(items):
                should_continue = False
                break_reason = "items_exhausted"
                iteration = max(0, iteration - 1)
            else:
                current_item = items[iteration - 1]
                state.set_value(node.item_key or f"{node_id}_item", current_item)
        elif node.condition:
            condition_passed = evaluate_condition(node.condition, state.data)
            if mode == "while" and not condition_passed:
                should_continue = False
                break_reason = "condition_false"
                iteration = max(0, iteration - 1)
            if mode == "retry_until" and condition_passed:
                should_continue = False
                break_reason = "condition_satisfied"
                iteration = max(0, iteration - 1)

        state.set_value(node.iteration_key or f"{node_id}_iteration", iteration)
        result = {
            "mode": mode,
            "iteration": iteration,
            "continue": should_continue,
            "should_continue": should_continue,
            "break_reason": break_reason,
            "max_iterations": max_iterations,
            "current_item": current_item,
        }
        state.loop_states[node_id] = result
        state.set_value(node.output_key or node.id, result)

        if should_continue:
            state.emit("loop.iteration.started", f"Loop {node_id} iteration {iteration} started", node_id=node_id, **result)
        else:
            state.emit("loop.completed", f"Loop {node_id} completed: {break_reason}", node_id=node_id, **result)
        return result

    def _run_tool_node(self, state: RunState, node_id: str) -> dict[str, Any]:
        node = self.nodes[node_id]
        assert node.tool
        if state.tool_calls >= self.workflow.max_tool_calls:
            self._block(state, "max_tool_calls reached")
            return {"status": "blocked"}
        state.tool_calls += 1
        args = self._resolve_inputs(node.input, state)
        tool_name = "mcp_call" if node.type == "mcp_tool" else node.tool
        capability = self.tools.capability(tool_name)
        if capability is None:
            message = f"Tool {node.tool!r} is not registered."
            state.status = "failed"
            return {"status": "failed", "error": message}
        if node.type == "mcp_tool":
            args = dict(args)
            args.setdefault("__mcp_tool", node.tool)
        state.emit("tool.called", f"Tool {node.tool} called", node_id=node_id, args=args, capability=capability.to_dict())
        result = self.tools.run(
            tool_name,
            args,
            {
                "repo_root": state.repo_root,
                "request": state.request,
                "data": state.data,
                "scopes": state.data.get("scopes", []),
                "node_id": node_id,
                "run_id": state.data.get("run_id"),
            },
        )
        state.emit(
            "tool.result",
            f"Tool {node.tool} returned",
            node_id=node_id,
            tool=node.tool,
            result=result,
            result_summary=summarize_value(result),
            result_status=result.get("status") if isinstance(result, dict) else None,
            result_keys=sorted(result.keys()) if isinstance(result, dict) else None,
            result_size_chars=len(str(result)),
        )
        state.set_value(node.output_key or node.id, result)
        if result.get("blocked"):
            approval_payload = dict(result)
            approval_payload.pop("message", None)
            state.emit(
                "approval.required",
                str(result.get("message") or result.get("output") or "Tool requires approval"),
                node_id=node_id,
                **approval_payload,
            )
            self._block(state, str(result.get("message") or "tool approval required"))
        return result

    def _agent_tool_policy_violations(self, agent: Any) -> list[dict[str, Any]]:
        violations: list[dict[str, Any]] = []
        for tool_name in agent.tools:
            capability = self.tools.capability(tool_name)
            if capability is None:
                violations.append(
                    {
                        "code": "agent_unknown_tool",
                        "agent_id": agent.id,
                        "tool": tool_name,
                        "message": f"Agent {agent.id} declares unknown tool {tool_name!r}.",
                    }
                )
                continue
            missing = [
                permission
                for permission in capability.permissions
                if not bool(getattr(agent.permissions, permission, False))
            ]
            if missing:
                violations.append(
                    {
                        "code": "agent_tool_permission_denied",
                        "agent_id": agent.id,
                        "tool": tool_name,
                        "missing_permissions": missing,
                        "message": f"Agent {agent.id} lacks permissions for {tool_name!r}: {', '.join(missing)}.",
                    }
                )
            if capability.requires_approval and not agent.permissions.requires_approval:
                violations.append(
                    {
                        "code": "agent_tool_requires_approval",
                        "agent_id": agent.id,
                        "tool": tool_name,
                        "message": f"Agent {agent.id} declares approval-gated tool {tool_name!r} without requiring approval.",
                    }
                )
        return violations

    def _run_human_gate(self, state: RunState, node_id: str) -> dict[str, Any]:
        node = self.nodes[node_id]
        approved = bool(
            state.data.get(f"{node_id}_approved", False)
            or state.data.get("preapprove_all", False)
            or node.input.get("auto_approve", False)
        )
        result = {
            "approved": approved,
            "reason": node.approval_reason or "Human approval gate",
            "approval_type": "human_gate",
        }
        state.set_value(node.output_key or node.id, result)
        if not approved:
            state.emit("approval.required", "Workflow paused for approval", node_id=node_id, **result)
            self._block(state, "approval required")
        return result

    def _next_nodes(self, state: RunState, node_id: str) -> list[str]:
        candidates = sorted(
            [edge for edge in self.workflow.edges if edge.from_node == node_id],
            key=lambda edge: edge.priority,
            reverse=True,
        )
        selected: list[str] = []
        for edge in candidates:
            edge_key = f"{edge.from_node}->{edge.to_node}:{edge.when or ''}"
            traversals = state.traversed_edges.get(edge_key, 0)
            if edge.max_traversals is not None and traversals >= edge.max_traversals:
                continue
            if not evaluate_condition(edge.when, state.data):
                continue
            state.traversed_edges[edge_key] = traversals + 1
            if edge.to_node in self.nodes and self.nodes[edge.to_node].type == "loop":
                loop_state = state.loop_states.get(edge.to_node)
                if loop_state and loop_state.get("continue"):
                    state.emit(
                        "loop.iteration.completed",
                        f"Loop {edge.to_node} iteration {loop_state.get('iteration')} completed",
                        node_id=edge.to_node,
                        **loop_state,
                    )
            selected.append(edge.to_node)
            state.emit("edge.selected", f"{edge.from_node} -> {edge.to_node}", from_node=edge.from_node, to_node=edge.to_node, when=edge.when)
        return selected

    def _resolve_inputs(self, value: Any, state: RunState) -> Any:
        if isinstance(value, str) and value.startswith("$"):
            return state.data.get(value[1:])
        if isinstance(value, dict):
            return {key: self._resolve_inputs(item, state) for key, item in value.items()}
        if isinstance(value, list):
            return [self._resolve_inputs(item, state) for item in value]
        return value

    def _block(self, state: RunState, reason: str) -> None:
        state.status = "blocked"
        state.emit("run.blocked", reason, node_id=state.current_node)

    def _checkpoint(self, state: RunState) -> dict[str, Any]:
        return {
            "data": state.data,
            "summaries": state.summaries,
            "visited_nodes": state.visited_nodes,
            "traversed_edges": state.traversed_edges,
            "loop_states": state.loop_states,
            "estimated_tokens_used": state.estimated_tokens_used,
            "agent_calls": state.agent_calls,
            "tool_calls": state.tool_calls,
            "current_node": state.current_node,
        }


def run_workflow(
    workflow: WorkflowSpec,
    request: str,
    repo_root: str,
    initial_data: dict[str, Any] | None = None,
    event_sink: Any | None = None,
    resume_checkpoint: dict[str, Any] | None = None,
    prior_events: list[RunEvent] | None = None,
    resume_after_node: str | None = None,
    runner_factory: Any | None = None,
) -> RunResult:
    runner = runner_factory(workflow) if runner_factory else WorkflowRunner(workflow, event_sink=event_sink)
    if event_sink is not None:
        runner.event_sink = event_sink
    return runner.run(
        request=request,
        repo_root=repo_root,
        initial_data=initial_data,
        resume_checkpoint=resume_checkpoint,
        prior_events=prior_events,
        resume_after_node=resume_after_node,
    )
