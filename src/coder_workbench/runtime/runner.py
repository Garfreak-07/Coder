from __future__ import annotations

from typing import Any

from coder_workbench.core import WorkflowSpec
from coder_workbench.executors import AgentExecutor, DefaultAgentExecutor
from coder_workbench.runtime.conditions import evaluate_condition
from coder_workbench.runtime.context import build_agent_context, build_context_packet, estimate_tokens
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
    ) -> None:
        self.workflow = workflow
        self.nodes = workflow.node_by_id()
        self.agents = workflow.agent_by_id()
        self.agent_executor = agent_executor or DefaultAgentExecutor()
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
        context = build_agent_context(agent, state)
        estimated = estimate_tokens(context)
        projected_tokens = state.estimated_tokens_used + estimated
        packet = build_context_packet(agent, state, node_id=node_id, context=context, estimated_tokens=estimated)
        state.emit(
            "agent.context_packet",
            f"Context packet for agent {agent.id}",
            node_id=node_id,
            packet=packet,
        )
        if state.token_budget and projected_tokens > state.token_budget:
            state.emit(
                "budget.warning",
                "Estimated token budget exceeded; agent call blocked",
                node_id=node_id,
                estimated_tokens_used=state.estimated_tokens_used,
                projected_tokens_used=projected_tokens,
                estimated_tokens=estimated,
                token_budget=state.token_budget,
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
        state.set_value(node.output_key or agent.output_key or node.id, result)
        return result

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
        if node.type == "mcp_tool":
            args = dict(args)
            args.setdefault("__mcp_tool", node.tool)
        state.emit("tool.called", f"Tool {node.tool} called", node_id=node_id, args=args)
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
) -> RunResult:
    return WorkflowRunner(workflow, event_sink=event_sink).run(
        request=request,
        repo_root=repo_root,
        initial_data=initial_data,
        resume_checkpoint=resume_checkpoint,
        prior_events=prior_events,
        resume_after_node=resume_after_node,
    )
