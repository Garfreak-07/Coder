from __future__ import annotations

from typing import Any

from coder_workbench.core import AgentSpec, ContextPolicy
from coder_workbench.runtime.state import RunState, summarize_value


def build_agent_context(agent: AgentSpec, state: RunState) -> dict[str, Any]:
    """Build a compact, structured prompt context for an agent.

    Token efficiency rule: pass selected fields and summaries by default. Full
    outputs and event history are opt-in per agent.
    """

    policy = agent.context or ContextPolicy()
    keys = list(state.data.keys()) if policy.include_all_state else policy.input_keys
    context: dict[str, Any] = {
        "request": state.request,
        "repo_root": state.repo_root,
        "state_summaries": {},
        "state": {},
    }

    for key in keys:
        if key not in state.data:
            continue
        value = state.data[key]
        context["state_summaries"][key] = state.summaries.get(key) or summarize_value(value)
        context["state"][key] = _compact_value(value, policy)

    for key in policy.summary_keys:
        if key in state.summaries:
            context["state_summaries"][key] = state.summaries[key]

    if policy.include_event_history:
        context["events"] = [
            {"type": event.type, "node_id": event.node_id, "message": event.message}
            for event in state.events[-20:]
        ]

    return context


def build_context_packet(
    agent: AgentSpec,
    state: RunState,
    *,
    node_id: str,
    context: dict[str, Any],
    estimated_tokens: int,
) -> dict[str, Any]:
    """Build the inspectable context packet shown in run events.

    The executor still receives the compact context. This packet is the product
    surface for trust/debugging: it explains what was selected, why it is small,
    which tools are allowed, and what loop iteration is current.
    """

    selected_state = context.get("state", {})
    summaries = context.get("state_summaries", {})
    loop = _current_loop_state(state)
    packet: dict[str, Any] = {
        "task": state.request,
        "agent": {
            "id": agent.id,
            "name": agent.name,
            "role": agent.role,
            "goal": agent.goal,
            "output_key": agent.output_key,
        },
        "node_id": node_id,
        "selected_state_keys": sorted(selected_state.keys()),
        "state_summaries": summaries,
        "selected_state": selected_state,
        "project_context": {
            "repo_root": state.repo_root,
            "scopes": state.data.get("scopes", []),
        },
        "allowed_tools": agent.tools,
        "permissions": agent.permissions.model_dump(),
        "context_policy": agent.context.model_dump(),
        "loop": loop,
        "token_estimate": {
            "packet": estimated_tokens,
            "run_used_after_packet": state.estimated_tokens_used + estimated_tokens,
            "budget": state.token_budget,
        },
        "output_contract": {
            "key": agent.output_key,
            "schema": "structured JSON object",
        },
    }
    return packet


def estimate_tokens(value: Any) -> int:
    # Conservative approximation. The important property is consistency so the
    # runtime can compare nodes and enforce budgets.
    return max(1, len(str(value)) // 4)


def _compact_value(value: Any, policy: ContextPolicy) -> Any:
    if policy.include_full_outputs:
        return value
    if isinstance(value, str):
        return value[: policy.max_chars_per_value]
    if isinstance(value, list):
        return [_compact_value(item, policy) for item in value[: policy.max_items_per_key]]
    if isinstance(value, dict):
        compact: dict[str, Any] = {}
        for index, (key, item) in enumerate(value.items()):
            if index >= policy.max_items_per_key:
                break
            compact[key] = _compact_value(item, policy)
        return compact
    return value


def _current_loop_state(state: RunState) -> dict[str, Any] | None:
    active: list[dict[str, Any]] = []
    for node_id, loop_state in state.loop_states.items():
        if loop_state.get("continue") and not loop_state.get("break_reason"):
            active.append({"node_id": node_id, **loop_state})
    if not active:
        return None
    return sorted(active, key=lambda item: str(item.get("updated_at", "")))[-1]
