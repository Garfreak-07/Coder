from __future__ import annotations

from langgraph.graph import END, START, StateGraph

from .nodes import (
    approval_node,
    check_node,
    execute_node,
    intake_node,
    module_map_node,
    plan_node,
    review_node,
    route_after_approval,
    route_after_review,
    scan_repo_node,
)
from .state import CodingState


def build_graph(event_bus=None):
    graph = StateGraph(CodingState)

    graph.add_node("intake", _with_events("intake", intake_node, event_bus))
    graph.add_node("scan_repo", _with_events("scan_repo", scan_repo_node, event_bus))
    graph.add_node("module_map", _with_events("module_map", module_map_node, event_bus))
    graph.add_node("plan", _with_events("codex_planner", plan_node, event_bus))
    graph.add_node("approval", _with_events("approval", approval_node, event_bus))
    graph.add_node("execute", _with_events("execute", execute_node, event_bus))
    graph.add_node("check", _with_events("check", check_node, event_bus))
    graph.add_node("review", _with_events("codex_planner", review_node, event_bus))
    graph.add_node("blocked", _with_events("blocked", lambda state: state, event_bus))

    graph.add_edge(START, "intake")
    graph.add_edge("intake", "scan_repo")
    graph.add_edge("scan_repo", "module_map")
    graph.add_edge("module_map", "plan")
    graph.add_edge("plan", "approval")

    graph.add_conditional_edges(
        "approval",
        route_after_approval,
        {
            "execute": "execute",
            "blocked": "blocked",
        },
    )

    graph.add_edge("execute", "check")
    graph.add_edge("check", "review")
    graph.add_conditional_edges(
        "review",
        route_after_review,
        {
            "retry": "plan",
            "done": END,
            "blocked": "blocked",
        },
    )
    graph.add_edge("blocked", END)

    return graph.compile()


def _with_events(name, fn, event_bus):
    if event_bus is None:
        return fn

    def wrapped(state):
        event_bus.emit(name, "status", f"{name} started", status="running")
        try:
            result = fn(state)
            event_bus.emit(name, "status", f"{name} completed", status=str(result.get("status", "ok")))
            _emit_a2a_message(name, state, result, event_bus)
            return result
        except Exception as exc:
            event_bus.emit(name, "error", f"{name} failed: {exc}", status="error")
            raise

    return wrapped


def _emit_a2a_message(name, state, result, event_bus) -> None:
    if name == "module_map":
        event_bus.send_message(
            "module_map",
            "codex_planner",
            "context.modules_ready",
            action="notify",
            payload={"module_count": len(result.get("modules", []))},
            metadata={"protocol": "local-a2a-v1", "handoff": "project_context"},
        )
        return

    if name == "codex_planner" and result.get("status") == "planned":
        event_bus.send_message(
            "codex_planner",
            "cc_executor",
            "plan.proposed",
            action="request_execution",
            payload={
                "status": result.get("status"),
                "plan": result.get("plan", ""),
                "proposed_changes": result.get("proposed_changes", []),
            },
            metadata={"protocol": "local-a2a-v1", "handoff": "implementation"},
        )
        event_bus.send_message(
            "codex_planner",
            "approval",
            "plan.proposed",
            action="request_approval",
            payload={
                "status": result.get("status"),
                "proposed_changes": result.get("proposed_changes", []),
                "needs_human": True,
            },
            metadata={"protocol": "local-a2a-v1", "handoff": "human_gate"},
            requires_user=True,
        )
        return

    if name == "execute":
        event_bus.send_message(
            "cc_executor",
            "cc_tester",
            "executor.changed_files",
            action="request_test",
            payload={
                "status": result.get("status"),
                "changed_files": result.get("changed_files", []),
            },
            metadata={"protocol": "local-a2a-v1", "handoff": "test_changes"},
        )
        return

    if name == "check":
        event_bus.send_message(
            "check",
            "codex_planner",
            "check.result",
            action="notify",
            payload={
                "passed": result.get("check_passed"),
                "output": result.get("check_output", ""),
            },
            metadata={"protocol": "local-a2a-v1", "handoff": "validation_result"},
        )
        return

    if name == "codex_planner":
        event_bus.send_message(
            "codex_planner",
            "ui",
            "review.completed",
            action="notify",
            payload={
                "risk_level": result.get("risk_level"),
                "next_step": result.get("next_step"),
                "notes": result.get("review_notes", ""),
            },
            metadata={"protocol": "local-a2a-v1", "handoff": "review_result"},
            requires_user=result.get("next_step") == "blocked",
        )
