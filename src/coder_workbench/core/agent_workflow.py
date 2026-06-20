from __future__ import annotations

from collections import Counter, deque
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field, model_validator

from coder_workbench.core.authority import authority_profile_for_agent
from coder_workbench.core.schema import AgentSpec, ContextPolicy, EdgeSpec, NodeSpec, PermissionPolicy, WorkflowSpec


AgentModelTier = Literal["best", "standard", "economy"]
HandoffType = Literal["run_contract", "planner_order", "execution_result", "test_result", "planner_decision", "round_summary"]
ValidationLevel = Literal["error", "warning"]

VALID_AGENT_ROLES = {
    "planner",
    "executor",
    "worker",
    "tester",
    "reviewer",
    "writer",
    "researcher",
    "summarizer",
    "custom",
}
VALID_MODEL_TIERS = {"best", "standard", "economy"}
ARTIFACT_PRIORITY = [
    "planner_order",
    "execution_result",
    "test_result",
    "run_contract",
    "planner_decision",
    "round_summary",
]


class CapabilityPermissions(BaseModel):
    model_config = ConfigDict(extra="forbid")

    read_files: bool = True
    edit_files: bool = False
    run_commands: bool = False
    use_network: bool = False


class CapabilitySpec(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    label: str
    description: str
    allowed_roles: list[str]
    requires: list[HandoffType] = Field(default_factory=list)
    produces: list[HandoffType] = Field(default_factory=list)
    permissions: CapabilityPermissions = Field(default_factory=CapabilityPermissions)
    can_talk_to_human: bool = False
    runtime_effects: list[str] = Field(default_factory=list)


class AgentWorkflowValidationIssue(BaseModel):
    model_config = ConfigDict(extra="forbid")

    level: ValidationLevel
    code: str
    message: str
    target_type: str
    target_id: str | None = None


class AgentWorkflowValidationResult(BaseModel):
    model_config = ConfigDict(extra="forbid")

    status: Literal["pass", "warning", "error"]
    issues: list[AgentWorkflowValidationIssue] = Field(default_factory=list)
    summary: dict[str, Any] = Field(default_factory=dict)


class AgentWorkflowValidationError(ValueError):
    def __init__(self, result: AgentWorkflowValidationResult) -> None:
        self.result = result
        errors = [issue.message for issue in result.issues if issue.level == "error"]
        super().__init__("; ".join(errors) or "Agent workflow validation failed")


class AgentWorkflowAgent(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    name: str
    role: str
    model_tier: str = "standard"
    can_talk_to_human: bool = False
    capabilities: list[str] = Field(default_factory=list)


class AgentWorkflowEdge(BaseModel):
    model_config = ConfigDict(extra="forbid", populate_by_name=True)

    from_agent: str = Field(alias="from")
    to_agent: str = Field(alias="to")
    handoff: HandoffType | None = None
    loop: bool = False
    label: str | None = None


class AgentWorkflowLoopPolicy(BaseModel):
    model_config = ConfigDict(extra="forbid")

    max_auto_rounds: int | None = 3
    user_can_change: bool = True


class AgentWorkflowLayoutPosition(BaseModel):
    model_config = ConfigDict(extra="forbid")

    x: float
    y: float


class AgentWorkflowUi(BaseModel):
    model_config = ConfigDict(extra="forbid")

    layout: dict[str, AgentWorkflowLayoutPosition] = Field(default_factory=dict)


class AgentWorkflowSpec(BaseModel):
    """User-visible agent workflow.

    This is deliberately smaller than WorkflowSpec. Users see agents, edges,
    capabilities, and loop policy; the compiler expands those choices into
    runtime nodes and internal artifact handoffs.
    """

    model_config = ConfigDict(extra="forbid", populate_by_name=True)

    id: str
    version: str = "0.4"
    name: str
    description: str = ""
    primary_planner_id: str
    agents: list[AgentWorkflowAgent]
    edges: list[AgentWorkflowEdge]
    loop_policy: AgentWorkflowLoopPolicy = Field(default_factory=AgentWorkflowLoopPolicy)
    ui: AgentWorkflowUi = Field(default_factory=AgentWorkflowUi)

    @model_validator(mode="before")
    @classmethod
    def migrate_legacy_agent_workflow(cls, data: Any) -> Any:
        if not isinstance(data, dict):
            return data
        migrated = dict(data)
        if migrated.get("version") in {None, "0.3"}:
            migrated["version"] = "0.4"
        if not migrated.get("primary_planner_id"):
            agents = migrated.get("agents")
            if isinstance(agents, list):
                planner = next(
                    (
                        agent
                        for agent in agents
                        if isinstance(agent, dict) and agent.get("role") == "planner" and agent.get("id")
                    ),
                    None,
                )
                if planner:
                    migrated["primary_planner_id"] = planner["id"]
        return migrated


def capability_registry() -> dict[str, CapabilitySpec]:
    return {capability.id: capability for capability in _CAPABILITIES}


def capability_catalog() -> list[dict[str, Any]]:
    return [capability.model_dump(mode="json") for capability in _CAPABILITIES]


def validate_agent_workflow_payload(data: dict[str, Any]) -> AgentWorkflowValidationResult:
    issues: list[AgentWorkflowValidationIssue] = []
    if not isinstance(data, dict):
        return _validation_result(
            [
                _issue(
                    "schema_invalid",
                    "Agent workflow must be a JSON object.",
                    "workflow",
                )
            ],
            summary={},
        )

    issues.extend(_raw_required_field_issues(data))
    try:
        spec = AgentWorkflowSpec.model_validate(data)
    except Exception as exc:
        issues.append(
            _issue(
                "schema_invalid",
                str(exc),
                "workflow",
            )
        )
        return _validation_result(issues, summary={})
    issues.extend(validate_agent_workflow(spec).issues)
    return _validation_result(issues, summary=_validation_summary(spec))


def validate_agent_workflow(spec: AgentWorkflowSpec) -> AgentWorkflowValidationResult:
    issues: list[AgentWorkflowValidationIssue] = []
    registry = capability_registry()
    agent_ids = [agent.id for agent in spec.agents]
    agent_by_id = {agent.id: agent for agent in spec.agents}
    id_counts = Counter(agent_ids)

    if not spec.id.strip():
        issues.append(_issue("missing_workflow_id", "Workflow must have an ID.", "workflow"))
    if not spec.name.strip():
        issues.append(_issue("missing_workflow_name", "Workflow must have a name.", "workflow"))
    if not spec.agents:
        issues.append(_issue("missing_agents", "Workflow must contain at least one Agent.", "workflow"))
    if not spec.primary_planner_id.strip():
        issues.append(
            _issue(
                "missing_primary_planner",
                "Workflow must contain one primary Planner Agent.",
                "workflow",
            )
        )

    duplicate_ids = sorted(agent_id for agent_id, count in id_counts.items() if agent_id and count > 1)
    for agent_id in duplicate_ids:
        issues.append(
            _issue(
                "duplicate_agent_id",
                f'Agent ID "{agent_id}" is duplicated.',
                "agent",
                agent_id,
            )
        )

    planner_agents = [agent for agent in spec.agents if agent.role == "planner"]
    if len(planner_agents) != 1:
        issues.append(
            _issue(
                "invalid_primary_planner_count",
                "Workflow must contain exactly one Planner Agent.",
                "workflow",
            )
        )
    primary = agent_by_id.get(spec.primary_planner_id)
    if primary is None:
        issues.append(
            _issue(
                "primary_planner_not_found",
                "primary_planner_id must point to an existing Agent.",
                "workflow",
                spec.primary_planner_id or None,
            )
        )
    else:
        if primary.role != "planner":
            issues.append(
                _issue(
                    "primary_planner_role_invalid",
                    "The primary Planner Agent must use the planner role.",
                    "agent",
                    primary.id,
                )
            )
        if not primary.can_talk_to_human:
            issues.append(
                _issue(
                    "primary_planner_cannot_talk_to_human",
                    "Primary Planner must be allowed to talk to the user.",
                    "agent",
                    primary.id,
                )
            )

    max_rounds = spec.loop_policy.max_auto_rounds
    if max_rounds is None:
        issues.append(
            _issue(
                "missing_max_auto_rounds",
                "loop_policy.max_auto_rounds must be set.",
                "loop_policy",
            )
        )
    elif not 0 <= max_rounds <= 20:
        issues.append(
            _issue(
                "max_auto_rounds_out_of_range",
                "loop_policy.max_auto_rounds must be between 0 and 20.",
                "loop_policy",
            )
        )

    produces_by_agent: dict[str, set[str]] = {}
    requires_by_agent: dict[str, set[str]] = {}
    for agent in spec.agents:
        if not agent.id.strip():
            issues.append(_issue("missing_agent_id", "Agent IDs cannot be empty.", "agent"))
        if not agent.name.strip():
            issues.append(_issue("missing_agent_name", "Agent name is required.", "agent", agent.id or None))
        if agent.role not in VALID_AGENT_ROLES:
            issues.append(
                _issue(
                    "invalid_agent_role",
                    f'Agent "{agent.name or agent.id}" uses unsupported role "{agent.role}".',
                    "agent",
                    agent.id or None,
                )
            )
        if agent.model_tier not in VALID_MODEL_TIERS:
            issues.append(
                _issue(
                    "invalid_model_tier",
                    f'Agent "{agent.name or agent.id}" must use model_tier best, standard, or economy.',
                    "agent",
                    agent.id or None,
                )
            )
        if not agent.capabilities:
            issues.append(
                _issue(
                    "missing_agent_capability",
                    f'Agent "{agent.name or agent.id}" must have at least one capability.',
                    "agent",
                    agent.id or None,
                )
            )
        if agent.id != spec.primary_planner_id and agent.can_talk_to_human:
            issues.append(
                _issue(
                    "non_primary_agent_can_talk_to_human",
                    f'Agent "{agent.name or agent.id}" cannot talk to the user. Only the primary Planner can talk to the user.',
                    "agent",
                    agent.id or None,
                )
            )

        produces: set[str] = set()
        requires: set[str] = set()
        for capability_id in agent.capabilities:
            capability = registry.get(capability_id)
            if capability is None:
                issues.append(
                    _issue(
                        "unknown_capability",
                        f'Agent "{agent.name or agent.id}" uses unknown capability "{capability_id}".',
                        "capability",
                        capability_id,
                    )
                )
                continue
            produces.update(capability.produces)
            requires.update(capability.requires)
            if agent.role not in capability.allowed_roles:
                issues.append(
                    _issue(
                        "capability_role_not_allowed",
                        f'Capability "{capability_id}" is not allowed for role "{agent.role}".',
                        "capability",
                        capability_id,
                    )
                )
            if agent.id != spec.primary_planner_id and capability.can_talk_to_human:
                issues.append(
                    _issue(
                        "human_capability_not_allowed",
                        f'Capability "{capability_id}" can talk to the user and is only allowed on the primary Planner.',
                        "capability",
                        capability_id,
                    )
                )
            if agent.id != spec.primary_planner_id and "planner_decision" in capability.produces:
                issues.append(
                    _issue(
                        "planner_decision_authority_violation",
                        f'Agent "{agent.name or agent.id}" cannot produce planner_decision.',
                        "agent",
                        agent.id or None,
                    )
                )
        profile = authority_profile_for_agent(agent, primary_planner_id=spec.primary_planner_id)
        allowed_artifacts = set(profile.allowed_artifact_types)
        for artifact in sorted(produces - allowed_artifacts):
            issues.append(
                _issue(
                    "authority_artifact_not_allowed",
                    f'Agent "{agent.name or agent.id}" with {profile.authority} authority cannot produce {artifact}.',
                    "agent",
                    agent.id or None,
                )
            )
        produces_by_agent[agent.id] = produces
        requires_by_agent[agent.id] = requires

    seen_edges: set[tuple[str, str, bool, str | None]] = set()
    for edge in spec.edges:
        if not edge.from_agent:
            issues.append(_issue("missing_edge_source", "Edge source cannot be empty.", "edge"))
        if not edge.to_agent:
            issues.append(_issue("missing_edge_target", "Edge target cannot be empty.", "edge"))
        if edge.from_agent and edge.from_agent not in agent_by_id:
            issues.append(
                _issue(
                    "edge_source_not_found",
                    f'Edge source "{edge.from_agent}" does not exist.',
                    "edge",
                    edge.from_agent,
                )
            )
        if edge.to_agent and edge.to_agent not in agent_by_id:
            issues.append(
                _issue(
                    "edge_target_not_found",
                    f'Edge target "{edge.to_agent}" does not exist.',
                    "edge",
                    edge.to_agent,
                )
            )
        edge_key = (edge.from_agent, edge.to_agent, edge.loop, edge.label)
        if edge_key in seen_edges:
            issues.append(
                _issue(
                    "duplicate_edge",
                    f'Duplicate edge "{edge.from_agent} -> {edge.to_agent}" is not allowed.',
                    "edge",
                    f"{edge.from_agent}->{edge.to_agent}",
                )
            )
        seen_edges.add(edge_key)
        if edge.loop and max_rounds is None:
            issues.append(
                _issue(
                    "loop_without_max_rounds",
                    "This workflow contains a loop, but max automatic rounds is not set.",
                    "loop_policy",
                )
            )
        if edge.loop and edge.to_agent != spec.primary_planner_id:
            issues.append(
                _issue(
                    "loop_must_return_to_primary_planner",
                    "Feedback loops must route back to the primary Planner.",
                    "edge",
                    f"{edge.from_agent}->{edge.to_agent}",
                )
            )

    if primary is not None and spec.agents:
        reachable = _reachable_agent_ids(spec, primary.id)
        meaningful_agents = {agent.id for agent in spec.agents if agent.id != primary.id}
        if not meaningful_agents:
            issues.append(
                _issue(
                    "missing_meaningful_agent",
                    "Primary Planner must reach at least one worker, tester, or reviewer Agent.",
                    "workflow",
                )
            )
        elif not reachable.intersection(meaningful_agents):
            issues.append(
                _issue(
                    "primary_planner_path_missing",
                    "At least one path from the primary Planner must reach another meaningful Agent.",
                    "workflow",
                )
            )

    test_agents = [agent for agent in spec.agents if "test_result" in produces_by_agent.get(agent.id, set())]
    if len(test_agents) > 1 and not any("aggregate_tests" in agent.capabilities for agent in test_agents):
        issues.append(
            _issue(
                "missing_final_tester",
                "Workflows with multiple testing Agents must include a user-added final tester with aggregate_tests.",
                "workflow",
            )
        )

    worker_cycle = _non_planner_cycle(spec)
    if worker_cycle:
        issues.append(
            _issue(
                "agent_cycle_without_planner",
                "Worker/tester subgraphs cannot form cycles that bypass the primary Planner.",
                "workflow",
                "->".join(worker_cycle),
            )
        )

    upstream_by_agent = _upstream_agents(spec)
    for agent in spec.agents:
        if agent.id not in agent_by_id:
            continue
        available: set[str] = set()
        for upstream_id in upstream_by_agent.get(agent.id, set()):
            available.update(produces_by_agent.get(upstream_id, set()))
        if agent.id == spec.primary_planner_id:
            available.update(produces_by_agent.get(agent.id, set()))
        missing = sorted(requires_by_agent.get(agent.id, set()) - available)
        for artifact in missing:
            issues.append(
                _issue(
                    "unsatisfied_capability_input",
                    f'Agent "{agent.name or agent.id}" needs {artifact}, but no upstream Agent produces {artifact}.',
                    "agent",
                    agent.id,
                )
            )

    for edge in spec.edges:
        from_outputs = produces_by_agent.get(edge.from_agent, set())
        to_requires = requires_by_agent.get(edge.to_agent, set())
        inferred = _infer_edge_handoff(from_outputs, to_requires)
        if edge.handoff and edge.handoff in from_outputs:
            inferred = edge.handoff
        if edge.from_agent in agent_by_id and edge.to_agent in agent_by_id and inferred is None:
            issues.append(
                _issue(
                    "edge_without_meaningful_output",
                    f'Edge "{edge.from_agent} -> {edge.to_agent}" cannot be saved because no produced artifact satisfies the target Agent.',
                    "edge",
                    f"{edge.from_agent}->{edge.to_agent}",
                )
            )

    return _validation_result(issues, summary=_validation_summary(spec))


def assert_valid_agent_workflow(spec: AgentWorkflowSpec) -> None:
    result = validate_agent_workflow(spec)
    if any(issue.level == "error" for issue in result.issues):
        raise AgentWorkflowValidationError(result)


def default_planner_led_agent_workflow() -> AgentWorkflowSpec:
    return AgentWorkflowSpec.model_validate(
        {
            "id": "default-planner-led",
            "version": "0.4",
            "name": "Planner-led Agent Workflow",
            "description": "Planner decides. Executor changes only by order. Tester returns evidence. Runtime hides graph details.",
            "primary_planner_id": "planner",
            "agents": [
                {
                    "id": "planner",
                    "name": "Planner Agent",
                    "role": "planner",
                    "model_tier": "best",
                    "can_talk_to_human": True,
                    "capabilities": [
                        "negotiate_contract",
                        "make_plan",
                        "judge_completion",
                        "judge_risk",
                        "make_next_decision",
                        "round_summarize",
                    ],
                },
                {
                    "id": "executor",
                    "name": "Executor Agent",
                    "role": "executor",
                    "model_tier": "standard",
                    "can_talk_to_human": False,
                    "capabilities": [
                        "follow_planner_order",
                        "modify_files",
                        "return_execution_result",
                    ],
                },
                {
                    "id": "tester",
                    "name": "Tester Agent",
                    "role": "tester",
                    "model_tier": "standard",
                    "can_talk_to_human": False,
                    "capabilities": [
                        "model_review",
                        "optional_check_command",
                        "return_test_result",
                    ],
                },
            ],
            "edges": [
                {"from": "planner", "to": "executor"},
                {"from": "executor", "to": "tester"},
                {"from": "tester", "to": "planner", "loop": True},
            ],
            "loop_policy": {"max_auto_rounds": 3, "user_can_change": True},
            "ui": {
                "layout": {
                    "planner": {"x": 60, "y": 120},
                    "executor": {"x": 360, "y": 120},
                    "tester": {"x": 660, "y": 120},
                }
            },
        }
    )


def _compile_agent_workflow_legacy_impl(spec: AgentWorkflowSpec) -> WorkflowSpec:
    """Compile AgentWorkflowSpec into the legacy WorkflowSpec preview."""

    assert_valid_agent_workflow(spec)
    primary = next(agent for agent in spec.agents if agent.id == spec.primary_planner_id)
    runtime_agents = _ordered_runtime_agents(spec)
    max_rounds = spec.loop_policy.max_auto_rounds
    assert max_rounds is not None

    agents = [
        _runtime_agent(
            source=primary,
            runtime_id="planner_contract",
            role="Planner Agent",
            goal="Negotiate the run contract with the human-facing request.",
            artifact_type="run_contract",
            output_key="run_contract",
        ),
        _runtime_agent(
            source=primary,
            runtime_id="planner_order",
            role="Planner Agent",
            goal="Produce the next executable order for downstream Agents.",
            artifact_type="planner_order",
            output_key="planner_order",
            input_keys=["run_contract", "round_summary", "execution_result", "test_result"],
            summary_keys=["round_summary"],
        ),
    ]
    nodes = [
        NodeSpec(id="start", type="start"),
        NodeSpec(id="run_contract", type="agent", agent_id="planner_contract", output_key="run_contract"),
        NodeSpec(id="planner_order", type="agent", agent_id="planner_order", output_key="planner_order"),
    ]
    edges = [
        EdgeSpec(from_node="start", to_node="run_contract"),
        EdgeSpec(from_node="run_contract", to_node="planner_order"),
    ]

    previous_node = "planner_order"
    for agent in runtime_agents:
        artifact_type = _runtime_artifact_type(agent)
        output_key = artifact_type or f"{_safe_id(agent.id)}_result"
        node_id = f"agent_{_safe_id(agent.id)}"
        agents.append(
            _runtime_agent(
                source=agent,
                runtime_id=node_id,
                role=f"{agent.role.title()} Agent",
                goal=_runtime_goal(agent, artifact_type),
                artifact_type=artifact_type,
                output_key=output_key,
                input_keys=_runtime_input_keys(agent),
                summary_keys=_runtime_input_keys(agent),
                can_edit=_can_edit(agent),
                can_run_commands=_can_run_commands(agent),
                use_network=_can_use_network(agent),
            )
        )
        nodes.append(NodeSpec(id=node_id, type="agent", agent_id=node_id, output_key=output_key))
        edges.append(EdgeSpec(from_node=previous_node, to_node=node_id))
        previous_node = node_id

    agents.extend(
        [
            _runtime_agent(
                source=primary,
                runtime_id="planner_decision",
                role="Planner Agent",
                goal="Decide whether to finish, continue, ask the human, or stop.",
                artifact_type="planner_decision",
                output_key="planner_decision",
                input_keys=["run_contract", "execution_result", "test_result", "round_summary"],
                summary_keys=["execution_result", "test_result", "round_summary"],
            ),
            _runtime_agent(
                source=primary,
                runtime_id="round_summarizer",
                role="Planner Agent",
                goal="Compress this round into a compact carry-forward summary.",
                artifact_type="round_summary",
                output_key="round_summary",
                input_keys=["planner_order", "execution_result", "test_result", "planner_decision"],
                summary_keys=["planner_order", "execution_result", "test_result", "planner_decision"],
            ),
        ]
    )
    nodes.extend(
        [
            NodeSpec(id="planner_decision", type="agent", agent_id="planner_decision", output_key="planner_decision"),
            NodeSpec(id="round_summary", type="agent", agent_id="round_summarizer", output_key="round_summary"),
            NodeSpec(
                id="planner_loop",
                type="loop",
                loop_mode="retry_until",
                condition="planner_decision.next_action == 'finish' or planner_decision.next_action == 'stop' or planner_decision.next_action == 'ask_human'",
                max_iterations=max_rounds or 1,
                output_key="planner_loop",
            ),
            NodeSpec(id="finish", type="end"),
        ]
    )
    edges.extend(
        [
            EdgeSpec(from_node=previous_node, to_node="planner_decision"),
            EdgeSpec(from_node="planner_decision", to_node="round_summary"),
            EdgeSpec(from_node="round_summary", to_node="planner_loop"),
            EdgeSpec(
                from_node="planner_loop",
                to_node="planner_order",
                when="planner_loop.should_continue == True",
                max_traversals=max(1, max_rounds),
            ),
            EdgeSpec(from_node="planner_loop", to_node="finish", when="planner_loop.should_continue == False"),
        ]
    )

    return WorkflowSpec(
        id=f"{spec.id}-runtime",
        version=spec.version,
        name=spec.name,
        description=spec.description,
        max_steps=max(12, 4 + (len(runtime_agents) + 4) * max(1, max_rounds)),
        max_agent_calls=max(6, (len(runtime_agents) + 4) * max(1, max_rounds)),
        max_tool_calls=0,
        token_budget=80000,
        agents=agents,
        nodes=nodes,
        edges=edges,
        stop_conditions=[
            "planner_decision.next_action == finish",
            "planner_decision.next_action == ask_human",
            "planner_decision.next_action == stop",
            "max_auto_rounds reached",
            "token budget exceeded",
        ],
    )


def _runtime_agent(
    *,
    source: AgentWorkflowAgent,
    runtime_id: str,
    role: str,
    goal: str,
    artifact_type: str | None,
    output_key: str,
    input_keys: list[str] | None = None,
    summary_keys: list[str] | None = None,
    can_edit: bool = False,
    can_run_commands: bool = False,
    use_network: bool = False,
) -> AgentSpec:
    model = None if source.model_tier == "standard" else source.model_tier
    return AgentSpec(
        id=runtime_id,
        name=source.name,
        role=role,
        goal=goal,
        instructions=(
            "Return strict JSON for the requested artifact. "
            "Do not include full transcripts. Use only the structured inputs supplied by the runtime."
        ),
        model=model,
        tools=[],
        output_key=output_key,
        artifact_type=artifact_type,  # type: ignore[arg-type]
        permissions=PermissionPolicy(
            read_files=True,
            edit_files=can_edit,
            run_commands=can_run_commands,
            use_network=use_network,
            requires_approval=can_edit or can_run_commands or use_network,
        ),
        context=ContextPolicy(
            input_keys=input_keys or [],
            summary_keys=summary_keys or [],
            max_items_per_key=12,
            max_chars_per_value=3500,
            include_all_state=False,
            include_event_history=False,
            include_full_outputs=False,
        ),
    )


def _raw_required_field_issues(data: dict[str, Any]) -> list[AgentWorkflowValidationIssue]:
    issues: list[AgentWorkflowValidationIssue] = []
    if not str(data.get("id") or "").strip():
        issues.append(_issue("missing_workflow_id", "Workflow must have an ID.", "workflow"))
    if not str(data.get("name") or "").strip():
        issues.append(_issue("missing_workflow_name", "Workflow must have a name.", "workflow"))
    if not str(data.get("primary_planner_id") or "").strip():
        issues.append(
            _issue(
                "missing_primary_planner",
                "Workflow must contain one primary Planner Agent.",
                "workflow",
            )
        )
    if "agents" not in data:
        issues.append(_issue("missing_agents", "Workflow must contain at least one Agent.", "workflow"))
    if "edges" not in data:
        issues.append(_issue("missing_edges", "Workflow must define Agent edges.", "edge"))
    loop_policy = data.get("loop_policy")
    if not isinstance(loop_policy, dict):
        issues.append(_issue("missing_loop_policy", "Workflow must define loop_policy.", "loop_policy"))
    elif "max_auto_rounds" not in loop_policy:
        issues.append(
            _issue(
                "missing_max_auto_rounds",
                "loop_policy.max_auto_rounds must be set.",
                "loop_policy",
            )
        )
    return issues


def _validation_result(
    issues: list[AgentWorkflowValidationIssue],
    *,
    summary: dict[str, Any],
) -> AgentWorkflowValidationResult:
    if any(issue.level == "error" for issue in issues):
        status: Literal["pass", "warning", "error"] = "error"
    elif any(issue.level == "warning" for issue in issues):
        status = "warning"
    else:
        status = "pass"
    deduped: list[AgentWorkflowValidationIssue] = []
    seen: set[tuple[str, str | None, str]] = set()
    for issue in issues:
        key = (issue.code, issue.target_id, issue.message)
        if key in seen:
            continue
        seen.add(key)
        deduped.append(issue)
    return AgentWorkflowValidationResult(status=status, issues=deduped, summary=summary)


def _validation_summary(spec: AgentWorkflowSpec) -> dict[str, Any]:
    return {
        "agents": len(spec.agents),
        "edges": len(spec.edges),
        "primary_planner_id": spec.primary_planner_id,
        "max_auto_rounds": spec.loop_policy.max_auto_rounds,
    }


def _issue(
    code: str,
    message: str,
    target_type: str,
    target_id: str | None = None,
    *,
    level: ValidationLevel = "error",
) -> AgentWorkflowValidationIssue:
    return AgentWorkflowValidationIssue(
        level=level,
        code=code,
        message=message,
        target_type=target_type,
        target_id=target_id,
    )


def _reachable_agent_ids(spec: AgentWorkflowSpec, start_id: str) -> set[str]:
    graph: dict[str, list[str]] = {}
    for edge in spec.edges:
        graph.setdefault(edge.from_agent, []).append(edge.to_agent)
    reachable: set[str] = set()
    queue: deque[str] = deque(graph.get(start_id, []))
    while queue:
        agent_id = queue.popleft()
        if agent_id in reachable:
            continue
        reachable.add(agent_id)
        queue.extend(graph.get(agent_id, []))
    return reachable


def _upstream_agents(spec: AgentWorkflowSpec) -> dict[str, set[str]]:
    direct: dict[str, set[str]] = {}
    for edge in spec.edges:
        direct.setdefault(edge.to_agent, set()).add(edge.from_agent)
    upstream: dict[str, set[str]] = {}
    for agent in spec.agents:
        collected: set[str] = set()
        queue: deque[str] = deque(direct.get(agent.id, set()))
        while queue:
            agent_id = queue.popleft()
            if agent_id in collected:
                continue
            collected.add(agent_id)
            queue.extend(direct.get(agent_id, set()))
        upstream[agent.id] = collected
    return upstream


def _non_planner_cycle(spec: AgentWorkflowSpec) -> list[str]:
    graph: dict[str, list[str]] = {}
    for edge in spec.edges:
        if edge.loop or spec.primary_planner_id in {edge.from_agent, edge.to_agent}:
            continue
        graph.setdefault(edge.from_agent, []).append(edge.to_agent)

    visiting: set[str] = set()
    visited: set[str] = set()
    stack: list[str] = []

    def visit(agent_id: str) -> list[str]:
        if agent_id in visiting:
            try:
                start = stack.index(agent_id)
            except ValueError:
                start = 0
            return stack[start:] + [agent_id]
        if agent_id in visited:
            return []
        visiting.add(agent_id)
        stack.append(agent_id)
        for next_id in graph.get(agent_id, []):
            cycle = visit(next_id)
            if cycle:
                return cycle
        stack.pop()
        visiting.remove(agent_id)
        visited.add(agent_id)
        return []

    for agent in spec.agents:
        cycle = visit(agent.id)
        if cycle:
            return cycle
    return []


def _infer_edge_handoff(from_outputs: set[str], to_requires: set[str]) -> str | None:
    candidates = from_outputs.intersection(to_requires)
    for artifact in ARTIFACT_PRIORITY:
        if artifact in candidates:
            return artifact
    return None


def _ordered_runtime_agents(spec: AgentWorkflowSpec) -> list[AgentWorkflowAgent]:
    agent_by_id = {agent.id: agent for agent in spec.agents}
    graph: dict[str, list[str]] = {}
    for edge in spec.edges:
        if edge.loop:
            continue
        graph.setdefault(edge.from_agent, []).append(edge.to_agent)
    ordered: list[AgentWorkflowAgent] = []
    seen = {spec.primary_planner_id}
    queue: deque[str] = deque(graph.get(spec.primary_planner_id, []))
    while queue:
        agent_id = queue.popleft()
        if agent_id in seen:
            continue
        seen.add(agent_id)
        agent = agent_by_id.get(agent_id)
        if agent is not None:
            ordered.append(agent)
        queue.extend(graph.get(agent_id, []))
    for agent in spec.agents:
        if agent.id not in seen and agent.id != spec.primary_planner_id:
            ordered.append(agent)
    return ordered


def _runtime_artifact_type(agent: AgentWorkflowAgent) -> str | None:
    produces = _agent_produces(agent)
    for artifact in ("test_result", "execution_result", "planner_order", "round_summary", "planner_decision", "run_contract"):
        if artifact in produces:
            return artifact
    return None


def _runtime_goal(agent: AgentWorkflowAgent, artifact_type: str | None) -> str:
    if artifact_type == "execution_result":
        return "Follow the PlannerOrder and return only execution facts."
    if artifact_type == "test_result":
        return "Review execution evidence and return only a TestResult."
    return f"Follow the PlannerOrder and return structured facts for the {agent.role} role."


def _runtime_input_keys(agent: AgentWorkflowAgent) -> list[str]:
    required = _agent_requires(agent)
    if required:
        return sorted(required)
    if _runtime_artifact_type(agent) == "test_result":
        return ["execution_result", "planner_order"]
    return ["planner_order"]


def _agent_requires(agent: AgentWorkflowAgent) -> set[str]:
    registry = capability_registry()
    required: set[str] = set()
    for capability_id in agent.capabilities:
        capability = registry.get(capability_id)
        if capability:
            required.update(capability.requires)
    return required


def _agent_produces(agent: AgentWorkflowAgent) -> set[str]:
    registry = capability_registry()
    produced: set[str] = set()
    for capability_id in agent.capabilities:
        capability = registry.get(capability_id)
        if capability:
            produced.update(capability.produces)
    return produced


def _agent_permissions(agent: AgentWorkflowAgent) -> CapabilityPermissions:
    permissions = CapabilityPermissions(read_files=True)
    registry = capability_registry()
    for capability_id in agent.capabilities:
        capability = registry.get(capability_id)
        if not capability:
            continue
        permissions.read_files = permissions.read_files or capability.permissions.read_files
        permissions.edit_files = permissions.edit_files or capability.permissions.edit_files
        permissions.run_commands = permissions.run_commands or capability.permissions.run_commands
        permissions.use_network = permissions.use_network or capability.permissions.use_network
    return permissions


def _can_edit(agent: AgentWorkflowAgent) -> bool:
    return _agent_permissions(agent).edit_files


def _can_run_commands(agent: AgentWorkflowAgent) -> bool:
    return _agent_permissions(agent).run_commands


def _can_use_network(agent: AgentWorkflowAgent) -> bool:
    return _agent_permissions(agent).use_network


def _safe_id(value: str) -> str:
    safe = "".join(char if char.isalnum() or char == "_" else "_" for char in value.strip())
    return safe or "agent"


_CAPABILITIES = [
    CapabilitySpec(
        id="negotiate_contract",
        label="Negotiate contract",
        description="Planner creates the run agreement with the user-facing request.",
        allowed_roles=["planner"],
        produces=["run_contract"],
        can_talk_to_human=True,
    ),
    CapabilitySpec(
        id="make_plan",
        label="Make plan",
        description="Planner creates executable instructions for downstream Agents.",
        allowed_roles=["planner"],
        requires=["run_contract"],
        produces=["planner_order"],
        can_talk_to_human=True,
    ),
    CapabilitySpec(
        id="judge_completion",
        label="Judge completion",
        description="Planner decides whether the task is complete from evidence.",
        allowed_roles=["planner"],
        requires=["test_result"],
        produces=["planner_decision"],
        can_talk_to_human=True,
    ),
    CapabilitySpec(
        id="judge_risk",
        label="Judge risk",
        description="Planner makes subjective risk decisions from evidence.",
        allowed_roles=["planner"],
        requires=["test_result"],
        produces=["planner_decision"],
        can_talk_to_human=True,
    ),
    CapabilitySpec(
        id="make_next_decision",
        label="Make next decision",
        description="Planner chooses continue, finish, ask_human, or stop.",
        allowed_roles=["planner"],
        requires=["test_result"],
        produces=["planner_decision"],
        can_talk_to_human=True,
    ),
    CapabilitySpec(
        id="round_summarize",
        label="Summarize round",
        description="Planner compresses the round for carry-forward context.",
        allowed_roles=["planner"],
        requires=["planner_order", "execution_result", "test_result", "planner_decision"],
        produces=["round_summary"],
    ),
    CapabilitySpec(
        id="follow_planner_order",
        label="Follow Planner order",
        description="Agent follows PlannerOrder and does not redefine the global task.",
        allowed_roles=["executor", "worker", "writer", "researcher", "summarizer", "custom"],
        requires=["planner_order"],
    ),
    CapabilitySpec(
        id="modify_files",
        label="Modify files",
        description="Prepare and apply file changes under Planner authorization.",
        allowed_roles=["executor", "worker"],
        requires=["run_contract", "planner_order"],
        produces=["execution_result"],
        permissions=CapabilityPermissions(read_files=True, edit_files=True),
        runtime_effects=["patch_preview", "snapshot", "apply_patch", "rollback"],
    ),
    CapabilitySpec(
        id="generate_text",
        label="Generate text",
        description="Produce non-code text output from Planner instructions.",
        allowed_roles=["executor", "worker", "writer", "researcher", "summarizer", "custom"],
        requires=["planner_order"],
        produces=["execution_result"],
    ),
    CapabilitySpec(
        id="return_execution_result",
        label="Return execution result",
        description="Agent reports facts as ExecutionResult, not decisions.",
        allowed_roles=["executor", "worker", "writer", "researcher", "summarizer", "custom"],
        produces=["execution_result"],
    ),
    CapabilitySpec(
        id="model_review",
        label="Model review",
        description="Reviewer inspects ExecutionResult and returns evidence.",
        allowed_roles=["tester", "reviewer"],
        requires=["execution_result"],
        produces=["test_result"],
    ),
    CapabilitySpec(
        id="optional_check_command",
        label="Optional check command",
        description="Reviewer may attach command evidence when the runtime enables it.",
        allowed_roles=["tester", "reviewer"],
        requires=["execution_result"],
        produces=["test_result"],
        permissions=CapabilityPermissions(read_files=True, run_commands=True),
        runtime_effects=["check_command"],
    ),
    CapabilitySpec(
        id="aggregate_tests",
        label="Aggregate tests",
        description="Combine multiple ordered test results into one final TestResult for Planner.",
        allowed_roles=["tester", "reviewer"],
        requires=["test_result"],
        produces=["test_result"],
    ),
    CapabilitySpec(
        id="return_test_result",
        label="Return test result",
        description="Agent returns evidence as TestResult, not next actions.",
        allowed_roles=["tester", "reviewer"],
        produces=["test_result"],
    ),
]
