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


def build_graph():
    graph = StateGraph(CodingState)

    graph.add_node("intake", intake_node)
    graph.add_node("scan_repo", scan_repo_node)
    graph.add_node("module_map", module_map_node)
    graph.add_node("plan", plan_node)
    graph.add_node("approval", approval_node)
    graph.add_node("execute", execute_node)
    graph.add_node("check", check_node)
    graph.add_node("review", review_node)
    graph.add_node("blocked", lambda state: state)

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
