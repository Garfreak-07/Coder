from __future__ import annotations

from typing import TYPE_CHECKING, Literal

from pydantic import BaseModel, ConfigDict, Field

if TYPE_CHECKING:
    from coder_workbench.core.agent_workflow import AgentWorkflowAgent


Authority = Literal["planner", "worker", "tester", "final_tester"]


class AgentAuthorityProfile(BaseModel):
    model_config = ConfigDict(extra="forbid")

    authority: Authority
    can_ask_human: bool = False
    can_write_long_term_memory: bool = False
    can_create_plan_graph: bool = False
    can_trigger_interrupt: bool = False
    can_modify_files: bool = False
    can_run_commands: bool = False
    allowed_artifact_types: list[str] = Field(default_factory=list)


PLANNER_PROFILE = AgentAuthorityProfile(
    authority="planner",
    can_ask_human=True,
    can_write_long_term_memory=True,
    can_create_plan_graph=True,
    allowed_artifact_types=["run_contract", "planner_order", "planner_decision", "round_summary"],
)

WORKER_PROFILE = AgentAuthorityProfile(
    authority="worker",
    can_trigger_interrupt=True,
    can_modify_files=True,
    allowed_artifact_types=["execution_result"],
)

TESTER_PROFILE = AgentAuthorityProfile(
    authority="tester",
    can_run_commands=True,
    allowed_artifact_types=["test_result"],
)

FINAL_TESTER_PROFILE = AgentAuthorityProfile(
    authority="final_tester",
    allowed_artifact_types=["test_result"],
)

AUTHORITY_PROFILES = {
    "planner": PLANNER_PROFILE,
    "worker": WORKER_PROFILE,
    "tester": TESTER_PROFILE,
    "final_tester": FINAL_TESTER_PROFILE,
}


def authority_profile_for_agent(agent: "AgentWorkflowAgent", *, primary_planner_id: str) -> AgentAuthorityProfile:
    if agent.id == primary_planner_id:
        return PLANNER_PROFILE
    if "aggregate_tests" in agent.capabilities:
        return FINAL_TESTER_PROFILE
    if agent.role in {"tester", "reviewer"} or any(
        capability in agent.capabilities
        for capability in {"model_review", "optional_check_command", "return_test_result"}
    ):
        return TESTER_PROFILE
    return WORKER_PROFILE


def authority_catalog() -> list[dict[str, object]]:
    return [
        profile.model_dump(mode="json")
        for profile in AUTHORITY_PROFILES.values()
    ]
