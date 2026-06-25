from __future__ import annotations

from typing import TYPE_CHECKING, Literal

from pydantic import BaseModel, ConfigDict

from coder_workbench.core.authority import AgentAuthorityProfile, authority_profile_for_agent

if TYPE_CHECKING:
    from coder_workbench.agent_model.profile import AgentRuntimeProfile
    from coder_workbench.agent_model.token_budget import TokenBudget
    from coder_workbench.core.agent_workflow import AgentWorkflowAgent, AgentWorkflowSpec


RoleCardId = Literal["executor"]
AgentArchetype = Literal["planner", "executor"]


class RoleCardSpec(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: RoleCardId
    label: str
    archetype: AgentArchetype
    role: str
    engine_id: str
    default_capabilities: list[str]
    description: str


ROLE_CARDS = [
    RoleCardSpec(
        id="executor",
        label="Executor",
        archetype="executor",
        role="executor",
        engine_id="code-worker-engine",
        default_capabilities=[
            "follow_planner_order",
            "modify_files",
            "optional_check_command",
            "return_execution_result",
        ],
        description="Perform bounded execution work assigned by Planner and return verification evidence.",
    ),
]


def role_card_registry() -> dict[str, RoleCardSpec]:
    return {card.id: card for card in ROLE_CARDS}


def role_card_catalog() -> list[dict[str, object]]:
    return [card.model_dump(mode="json") for card in ROLE_CARDS]


def role_card_for_id(role_card_id: str) -> RoleCardSpec:
    try:
        return role_card_registry()[role_card_id]
    except KeyError as exc:
        raise ValueError(f"unknown role_card {role_card_id!r}") from exc


def agent_payload_from_role_card(data: dict[str, object]) -> dict[str, object]:
    migrated = dict(data)
    role_card = data.get("role_card")
    card = role_card_for_id(role_card.strip()) if isinstance(role_card, str) and role_card.strip() else None
    if card is not None and not str(migrated.get("role") or "").strip():
        migrated["role"] = card.role
    capabilities = migrated.get("capabilities")
    if not isinstance(capabilities, list) or not capabilities:
        migrated["capabilities"] = list(
            card.default_capabilities if card is not None else default_capabilities_for_role(str(migrated.get("role") or ""))
        )
    return migrated


def default_capabilities_for_role(role: str) -> list[str]:
    role = role.strip()
    if role == "planner":
        return [
            "negotiate_contract",
            "make_plan",
            "judge_completion",
            "judge_risk",
            "make_next_decision",
            "round_summarize",
        ]
    if role == "executor":
        return [
            "follow_planner_order",
            "modify_files",
            "optional_check_command",
            "return_execution_result",
        ]
    return []


def compile_agent_runtime_profile(
    agent: "AgentWorkflowAgent",
    *,
    primary_planner_id: str,
) -> AgentRuntimeProfile:
    from coder_workbench.agent_model import RuntimeProfileCompiler
    from coder_workbench.agent_model.recipe import recipe_from_workflow_agent

    authority = authority_profile_for_agent(agent, primary_planner_id=primary_planner_id)
    archetype = _archetype_for_agent(agent, authority.authority)
    profile = RuntimeProfileCompiler().compile(
        recipe_from_workflow_agent(agent, primary_planner_id=primary_planner_id)
    )
    return profile.model_copy(
        update={
            "agent_name": agent.name,
            "role_card": agent.role_card,
            "agent_archetype": archetype,
            "engine_id": _engine_id_for_agent(agent, archetype),
            "harness_id": _harness_id_for_archetype(archetype),
            "authority": authority,
            "allowed_artifacts": list(authority.allowed_artifact_types),
            "context_policy": _context_policy(archetype),
            "memory_policy": _memory_policy(authority.authority),
            "prompt_layers": _prompt_layers(archetype),
            "token_budget": _token_budget(archetype),
            "internal_loops": _internal_loops(authority.authority),
            "tool_policy": _tool_policy(agent, authority),
            "evaluation_profile": _evaluation_profile(archetype),
        }
    )


def compile_runtime_profiles(workflow: "AgentWorkflowSpec") -> list[AgentRuntimeProfile]:
    return [
        compile_agent_runtime_profile(agent, primary_planner_id=workflow.primary_planner_id)
        for agent in workflow.agents
    ]


def _archetype_for_agent(agent: "AgentWorkflowAgent", authority: str) -> str:
    if agent.role_card:
        return role_card_for_id(agent.role_card).archetype
    if authority == "planner":
        return "planner"
    return "executor"


def _engine_id_for_agent(agent: "AgentWorkflowAgent", archetype: str) -> str:
    if agent.role_card:
        return role_card_for_id(agent.role_card).engine_id
    return {
        "planner": "planner-engine",
        "executor": "code-worker-engine",
    }[archetype]


def _harness_id_for_archetype(archetype: str) -> str | None:
    if archetype == "executor":
        return "code-worker-harness"
    return None


def _context_policy(archetype: str) -> dict[str, object]:
    if archetype == "planner":
        return {"skill_load_mode": "index_only", "memory": "workflow_summary", "max_skill_tokens": 0}
    return {"skill_load_mode": "on_demand", "memory": "direct_refs_only", "max_skill_tokens": 1600}


def _memory_policy(authority: str) -> dict[str, object]:
    return {
        "can_read_workflow_memory": authority == "planner",
        "can_write_long_term_memory": authority == "planner",
    }


def _prompt_layers(archetype: str) -> dict[str, object]:
    if archetype == "planner":
        return {
            "schema_version": "prompt-layers/v1",
            "layer_order": [
                "output_contract",
                "planner_rules",
                "harness_contract",
                "task_context",
                "state_view",
                "capability_set",
                "memory_context",
            ],
            "max_layer_chars": 8000,
        }
    return {
        "schema_version": "prompt-layers/v1",
        "layer_order": [
            "output_contract",
            "executor_rules",
            "harness_contract",
            "agent_context",
            "work_item",
            "task_envelope",
            "coding_context",
            "capability_set",
            "skill_context",
        ],
        "max_layer_chars": 8000,
    }


def _token_budget(archetype: str) -> TokenBudget:
    from coder_workbench.agent_model.token_budget import TokenBudget

    budgets = {
        "planner": 12000,
        "executor": 9000,
    }
    return TokenBudget(max_input_tokens=budgets.get(archetype, 8000), managed_by_runtime=True)


def _internal_loops(authority: str) -> dict[str, object]:
    return {
        "schema_repair_attempts": 1,
        "self_check": authority == "executor",
        "planner_repair": authority == "planner",
        "verification_repair_attempts": 1 if authority == "executor" else 0,
    }


def _tool_policy(agent: "AgentWorkflowAgent", authority: AgentAuthorityProfile) -> dict[str, object]:
    return {
        "read_files": True,
        "edit_files": authority.can_modify_files and "modify_files" in agent.capabilities,
        "run_commands": authority.can_run_commands and "optional_check_command" in agent.capabilities,
        "external_effects_require_preview": True,
        "connector_operations": "deny_by_default",
    }


def _evaluation_profile(archetype: str) -> dict[str, object]:
    if archetype == "planner":
        return {"artifact_type": "planner_order", "metrics": ["plan_valid_rate", "wrong_skill_selection_rate"]}
    return {"artifact_type": "execution_result", "metrics": ["schema_valid_rate", "blocked_rate", "verification_evidence_rate"]}
