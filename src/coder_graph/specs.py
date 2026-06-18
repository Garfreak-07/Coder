from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Literal, TypedDict


class AgentSpec(TypedDict):
    id: str
    role: str
    goal: str
    input_keys: list[str]
    output_schema: dict[str, str]
    model: str | None
    tools: list[str]


class WorkflowStepSpec(TypedDict):
    id: str
    kind: Literal["agent", "deterministic", "human_gate"]
    uses: str
    input_keys: list[str]
    output_key: str


class WorkflowSpec(TypedDict):
    id: str
    name: str
    description: str
    max_loops: int
    agents: list[AgentSpec]
    steps: list[WorkflowStepSpec]
    stop_conditions: list[str]


def load_workflow_spec(path: str | Path) -> WorkflowSpec:
    spec_path = Path(path).expanduser().resolve()
    data = json.loads(spec_path.read_text(encoding="utf-8"))
    return validate_workflow_spec(data, source=str(spec_path))


def validate_workflow_spec(data: dict[str, Any], source: str = "<memory>") -> WorkflowSpec:
    required = ["id", "name", "description", "max_loops", "agents", "steps", "stop_conditions"]
    missing = [key for key in required if key not in data]
    if missing:
        raise ValueError(f"Workflow spec {source} is missing keys: {missing}")

    if not isinstance(data["agents"], list) or not isinstance(data["steps"], list):
        raise ValueError(f"Workflow spec {source} must contain list fields: agents, steps")

    agent_ids = set()
    agents: list[AgentSpec] = []
    for item in data["agents"]:
        agent = _validate_agent_spec(item, source)
        if agent["id"] in agent_ids:
            raise ValueError(f"Workflow spec {source} has duplicate agent id: {agent['id']}")
        agent_ids.add(agent["id"])
        agents.append(agent)

    steps: list[WorkflowStepSpec] = []
    for item in data["steps"]:
        step = _validate_step_spec(item, source)
        if step["kind"] == "agent" and step["uses"] not in agent_ids:
            raise ValueError(f"Workflow spec {source} step {step['id']} references unknown agent: {step['uses']}")
        steps.append(step)

    max_loops = int(data["max_loops"])
    if max_loops < 1 or max_loops > 10:
        raise ValueError(f"Workflow spec {source} max_loops must be between 1 and 10")

    return {
        "id": str(data["id"]),
        "name": str(data["name"]),
        "description": str(data["description"]),
        "max_loops": max_loops,
        "agents": agents,
        "steps": steps,
        "stop_conditions": [str(item) for item in data["stop_conditions"]],
    }


def summarize_workflow_spec(spec: WorkflowSpec) -> str:
    lines = [
        f"{spec['name']} ({spec['id']})",
        spec["description"],
        f"max_loops: {spec['max_loops']}",
        "agents:",
        *[f"- {agent['id']}: {agent['role']}" for agent in spec["agents"]],
        "steps:",
        *[f"- {step['id']} [{step['kind']}] uses {step['uses']}" for step in spec["steps"]],
        "stop_conditions:",
        *[f"- {condition}" for condition in spec["stop_conditions"]],
    ]
    return "\n".join(lines)


def _validate_agent_spec(data: dict[str, Any], source: str) -> AgentSpec:
    required = ["id", "role", "goal", "input_keys", "output_schema", "tools"]
    missing = [key for key in required if key not in data]
    if missing:
        raise ValueError(f"Agent spec in {source} is missing keys: {missing}")

    return {
        "id": str(data["id"]),
        "role": str(data["role"]),
        "goal": str(data["goal"]),
        "input_keys": [str(item) for item in data["input_keys"]],
        "output_schema": {str(key): str(value) for key, value in data["output_schema"].items()},
        "model": str(data["model"]) if data.get("model") else None,
        "tools": [str(item) for item in data["tools"]],
    }


def _validate_step_spec(data: dict[str, Any], source: str) -> WorkflowStepSpec:
    required = ["id", "kind", "uses", "input_keys", "output_key"]
    missing = [key for key in required if key not in data]
    if missing:
        raise ValueError(f"Step spec in {source} is missing keys: {missing}")

    kind = str(data["kind"])
    if kind not in {"agent", "deterministic", "human_gate"}:
        raise ValueError(f"Step spec in {source} has invalid kind: {kind}")

    return {
        "id": str(data["id"]),
        "kind": kind,  # type: ignore[typeddict-item]
        "uses": str(data["uses"]),
        "input_keys": [str(item) for item in data["input_keys"]],
        "output_key": str(data["output_key"]),
    }

