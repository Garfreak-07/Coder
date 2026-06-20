from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, ConfigDict


SkillAuthority = Literal["planner", "worker", "tester", "final_tester"]


class AgentSkillModule(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    authority: SkillAuthority
    description: str


SKILL_MODULES = [
    AgentSkillModule(
        id="task_modeling",
        authority="planner",
        description="Translate the user request into a scoped task model.",
    ),
    AgentSkillModule(
        id="plan_graph_decomposition",
        authority="planner",
        description="Split remaining work into dependency-aware AgentGraph work items.",
    ),
    AgentSkillModule(
        id="capability_routing",
        authority="planner",
        description="Assign work to Agents based on capabilities and authority.",
    ),
    AgentSkillModule(
        id="dependency_planning",
        authority="planner",
        description="Use depends_on for true execution dependencies only.",
    ),
    AgentSkillModule(
        id="risk_judgment",
        authority="planner",
        description="Decide whether risk can be handled automatically or needs the user.",
    ),
    AgentSkillModule(
        id="replanning",
        authority="planner",
        description="Plan corrective work from PlannerInputBundle and interrupts.",
    ),
    AgentSkillModule(
        id="human_escalation",
        authority="planner",
        description="Ask the user when the next step exceeds the agreed direction.",
    ),
    AgentSkillModule(
        id="memory_read_write",
        authority="planner",
        description="Read run context and write workflow memory after Planner decisions.",
    ),
    AgentSkillModule(
        id="tool_policy_planning",
        authority="planner",
        description="Plan tool use through downstream Agent capabilities and runtime policy.",
    ),
    AgentSkillModule(
        id="follow_task_envelope",
        authority="worker",
        description="Stay inside the assigned AgentTaskEnvelope.",
    ),
    AgentSkillModule(
        id="local_execution",
        authority="worker",
        description="Perform the assigned local implementation work.",
    ),
    AgentSkillModule(
        id="proposed_changes",
        authority="worker",
        description="Return proposed file changes as execution facts.",
    ),
    AgentSkillModule(
        id="blocker_reporting",
        authority="worker",
        description="Report blocked execution with the Planner intervention protocol.",
    ),
    AgentSkillModule(
        id="execution_result_output",
        authority="worker",
        description="Return execution_result artifacts only.",
    ),
    AgentSkillModule(
        id="evidence_review",
        authority="tester",
        description="Review execution evidence without deciding global next steps.",
    ),
    AgentSkillModule(
        id="check_command_proposal",
        authority="tester",
        description="Attach optional command evidence when runtime policy allows it.",
    ),
    AgentSkillModule(
        id="test_result_output",
        authority="tester",
        description="Return test_result artifacts only.",
    ),
    AgentSkillModule(
        id="confidence_calibration",
        authority="tester",
        description="Calibrate confidence from observed evidence.",
    ),
    AgentSkillModule(
        id="aggregate_tests",
        authority="final_tester",
        description="Aggregate multiple local test results for Planner.",
    ),
    AgentSkillModule(
        id="round_evidence_compression",
        authority="final_tester",
        description="Compress round-level evidence without making Planner decisions.",
    ),
]


def skill_module_catalog() -> list[dict[str, str]]:
    return [module.model_dump(mode="json") for module in SKILL_MODULES]


def skill_modules_for_authority(authority: SkillAuthority) -> list[AgentSkillModule]:
    return [module for module in SKILL_MODULES if module.authority == authority]
