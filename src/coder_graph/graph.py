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
    graph.add_node("plan", _with_events("planner", plan_node, event_bus))
    graph.add_node("approval", _with_events("approval", approval_node, event_bus))
    graph.add_node("execute", _with_events("execute", execute_node, event_bus))
    graph.add_node("check", _with_events("check", check_node, event_bus))
    graph.add_node("review", _with_events("reviewer", review_node, event_bus))
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
            return result
        except Exception as exc:
            event_bus.emit(name, "error", f"{name} failed: {exc}", status="error")
            raise

    return wrapped
