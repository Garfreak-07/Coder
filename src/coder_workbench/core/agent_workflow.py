from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, model_validator

from coder_workbench.core.schema import AgentSpec, ContextPolicy, EdgeSpec, NodeSpec, PermissionPolicy, WorkflowSpec


AgentModelTier = Literal["best", "standard", "economy"]
AgentCapability = Literal[
    "negotiate_contract",
    "make_plan",
    "judge_completion",
    "judge_risk",
    "make_next_decision",
    "modify_files",
    "follow_planner_order",
    "return_execution_result",
    "model_review",
    "optional_check_command",
    "return_test_result",
]
HandoffType = Literal["run_contract", "planner_order", "execution_result", "test_result", "planner_decision", "round_summary"]


class AgentWorkflowAgent(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    name: str
    role: Literal["planner", "executor", "tester"]
    model_tier: AgentModelTier = "standard"
    can_talk_to_human: bool = False
    capabilities: list[AgentCapability] = Field(default_factory=list)


class AgentWorkflowEdge(BaseModel):
    model_config = ConfigDict(extra="forbid")

    from_agent: str = Field(alias="from")
    to_agent: str = Field(alias="to")
    handoff: HandoffType
    loop: bool = False


class AgentWorkflowLoopPolicy(BaseModel):
    model_config = ConfigDict(extra="forbid")

    max_auto_rounds: int = Field(default=3, ge=0, le=20)
    user_can_change: bool = True


class AgentWorkflowSpec(BaseModel):
    """User-visible agent workflow.

    This is deliberately smaller than WorkflowSpec. Users see agents and
    handoff edges; the compiler expands that into runtime nodes.
    """

    model_config = ConfigDict(extra="forbid", populate_by_name=True)

    id: str
    version: str = "0.3"
    name: str
    description: str = ""
    agents: list[AgentWorkflowAgent]
    edges: list[AgentWorkflowEdge]
    loop_policy: AgentWorkflowLoopPolicy = Field(default_factory=AgentWorkflowLoopPolicy)

    @model_validator(mode="after")
    def validate_agent_workflow(self) -> "AgentWorkflowSpec":
        agent_ids = {agent.id for agent in self.agents}
        roles = {agent.role for agent in self.agents}
        required = {"planner", "executor", "tester"}
        missing_roles = required - roles
        if missing_roles:
            raise ValueError(f"agent workflow is missing roles: {', '.join(sorted(missing_roles))}")
        planners = [agent for agent in self.agents if agent.role == "planner"]
        if len(planners) != 1:
            raise ValueError("agent workflow requires exactly one planner")
        if not planners[0].can_talk_to_human:
            raise ValueError("planner must be the only human communication entry")
        for agent in self.agents:
            if agent.role != "planner" and agent.can_talk_to_human:
                raise ValueError(f"{agent.role} agent {agent.id} cannot talk to human")
        for edge in self.edges:
            if edge.from_agent not in agent_ids:
                raise ValueError(f"edge source not found: {edge.from_agent}")
            if edge.to_agent not in agent_ids:
                raise ValueError(f"edge target not found: {edge.to_agent}")
        return self


def default_planner_led_agent_workflow() -> AgentWorkflowSpec:
    return AgentWorkflowSpec.model_validate(
        {
            "id": "default-planner-led",
            "version": "0.3",
            "name": "Planner-led Agent Workflow",
            "description": "Planner decides. Executor changes only by order. Tester returns evidence. Runtime hides graph details.",
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
                    ],
                },
                {
                    "id": "executor",
                    "name": "Executor Agent",
                    "role": "executor",
                    "model_tier": "standard",
                    "can_talk_to_human": False,
                    "capabilities": [
                        "modify_files",
                        "follow_planner_order",
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
                {"from": "planner", "to": "executor", "handoff": "planner_order"},
                {"from": "executor", "to": "tester", "handoff": "execution_result"},
                {"from": "tester", "to": "planner", "handoff": "test_result", "loop": True},
            ],
            "loop_policy": {"max_auto_rounds": 3, "user_can_change": True},
        }
    )


def compile_agent_workflow(spec: AgentWorkflowSpec) -> WorkflowSpec:
    """Compile an Agent-only workflow into the current runtime WorkflowSpec."""

    planner = next(agent for agent in spec.agents if agent.role == "planner")
    executor = next(agent for agent in spec.agents if agent.role == "executor")
    tester = next(agent for agent in spec.agents if agent.role == "tester")
    max_rounds = spec.loop_policy.max_auto_rounds

    return WorkflowSpec(
        id=f"{spec.id}-runtime",
        version=spec.version,
        name=spec.name,
        description=spec.description,
        max_steps=max(12, 6 * max(1, max_rounds)),
        max_agent_calls=max(6, 6 * max(1, max_rounds)),
        max_tool_calls=0,
        token_budget=80000,
        agents=[
            _runtime_agent(
                source=planner,
                runtime_id="planner_contract",
                role="Planner Agent",
                goal="Negotiate the run contract with the human-facing request.",
                artifact_type="run_contract",
                output_key="run_contract",
            ),
            _runtime_agent(
                source=planner,
                runtime_id="planner_order",
                role="Planner Agent",
                goal="Produce the next executable order for the Executor.",
                artifact_type="planner_order",
                output_key="planner_order",
                input_keys=["run_contract", "round_summary", "execution_result", "test_result"],
                summary_keys=["round_summary"],
            ),
            _runtime_agent(
                source=executor,
                runtime_id="executor",
                role="Executor Agent",
                goal="Follow the PlannerOrder and return only execution facts.",
                artifact_type="execution_result",
                output_key="execution_result",
                input_keys=["run_contract", "planner_order"],
                summary_keys=["run_contract", "planner_order"],
                can_edit=True,
            ),
            _runtime_agent(
                source=tester,
                runtime_id="tester",
                role="Tester Agent",
                goal="Review execution evidence and return only a TestResult.",
                artifact_type="test_result",
                output_key="test_result",
                input_keys=["planner_order", "execution_result"],
                summary_keys=["planner_order", "execution_result"],
            ),
            _runtime_agent(
                source=planner,
                runtime_id="planner_decision",
                role="Planner Agent",
                goal="Decide whether to finish, continue, ask the human, or stop.",
                artifact_type="planner_decision",
                output_key="planner_decision",
                input_keys=["run_contract", "execution_result", "test_result", "round_summary"],
                summary_keys=["execution_result", "test_result", "round_summary"],
            ),
            _runtime_agent(
                source=planner,
                runtime_id="round_summarizer",
                role="Planner Agent",
                goal="Compress this round into a compact carry-forward summary.",
                artifact_type="round_summary",
                output_key="round_summary",
                input_keys=["planner_order", "execution_result", "test_result", "planner_decision"],
                summary_keys=["planner_order", "execution_result", "test_result", "planner_decision"],
            ),
        ],
        nodes=[
            NodeSpec(id="start", type="start"),
            NodeSpec(id="run_contract", type="agent", agent_id="planner_contract", output_key="run_contract"),
            NodeSpec(id="planner_order", type="agent", agent_id="planner_order", output_key="planner_order"),
            NodeSpec(id="execute", type="agent", agent_id="executor", output_key="execution_result"),
            NodeSpec(id="test", type="agent", agent_id="tester", output_key="test_result"),
            NodeSpec(id="planner_decision", type="agent", agent_id="planner_decision", output_key="planner_decision"),
            NodeSpec(id="round_summary", type="agent", agent_id="round_summarizer", output_key="round_summary"),
            NodeSpec(
                id="planner_loop",
                type="loop",
                loop_mode="retry_until",
                condition="planner_decision.next_action == 'finish' or planner_decision.next_action == 'stop' or planner_decision.next_action == 'ask_human'",
                max_iterations=max_rounds,
                output_key="planner_loop",
            ),
            NodeSpec(id="finish", type="end"),
        ],
        edges=[
            EdgeSpec(from_node="start", to_node="run_contract"),
            EdgeSpec(from_node="run_contract", to_node="planner_order"),
            EdgeSpec(from_node="planner_order", to_node="execute"),
            EdgeSpec(from_node="execute", to_node="test"),
            EdgeSpec(from_node="test", to_node="planner_decision"),
            EdgeSpec(from_node="planner_decision", to_node="round_summary"),
            EdgeSpec(from_node="round_summary", to_node="planner_loop"),
            EdgeSpec(
                from_node="planner_loop",
                to_node="planner_order",
                when="planner_loop.should_continue == True",
                max_traversals=max_rounds,
            ),
            EdgeSpec(from_node="planner_loop", to_node="finish", when="planner_loop.should_continue == False"),
        ],
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
    artifact_type: str,
    output_key: str,
    input_keys: list[str] | None = None,
    summary_keys: list[str] | None = None,
    can_edit: bool = False,
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
            run_commands=False,
            use_network=False,
            requires_approval=can_edit,
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
