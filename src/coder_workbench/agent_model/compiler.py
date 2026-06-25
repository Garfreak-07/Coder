from __future__ import annotations

from typing import TYPE_CHECKING, Any

from .profile import AgentRuntimeProfile
from .recipe import AgentRecipe, recipe_from_workflow_agent
from .token_budget import TokenBudget

if TYPE_CHECKING:
    from coder_workbench.core import AgentWorkflowSpec

OPENHANDS_PROVIDER_ID = "openhands-sdk"


class RuntimeProfileCompiler:
    """Compile ordinary Agent recipes into internal runtime profiles."""

    def __init__(self, installed_extensions: list[dict[str, Any]] | None = None) -> None:
        self.installed_extensions = installed_extensions or []

    def compile(
        self,
        recipe: AgentRecipe,
        *,
        run_contract: dict[str, Any] | None = None,
        planner_preferences: dict[str, Any] | None = None,
    ) -> AgentRuntimeProfile:
        role = recipe.role
        return AgentRuntimeProfile(
            agent_id=recipe.id,
            agent_name=recipe.name,
            role=role,
            agent_archetype=role,
            engine_id=_engine_id(role),
            harness_id=_harness_id(role),
            harness_runtime_profile_id=_default_harness_runtime_profile_id(role),
            harness_provider_id=OPENHANDS_PROVIDER_ID,
            harness_mode=_harness_mode(role),
            context_profile=_context_profile(role),
            context_policy=_context_policy(role),
            token_budget=_token_budget(role, planner_preferences=planner_preferences),
            allowed_artifacts=_allowed_artifacts(role),
            plugin_policy=_plugin_policy(role, recipe.preferred_extension_ids, run_contract),
            skill_policy=_skill_policy(role, recipe.preferred_extension_ids),
            memory_policy=_memory_policy(role),
            prompt_layers=_prompt_layers(role),
            internal_loops=_internal_loops(role),
            repair_policy=_repair_policy(role),
            tool_policy=_tool_policy(role),
            evaluation_profile=_evaluation_profile(role),
        )

    def compile_workflow(self, workflow: "AgentWorkflowSpec") -> list[AgentRuntimeProfile]:
        profiles: list[AgentRuntimeProfile] = []
        for agent in workflow.agents:
            recipe = recipe_from_workflow_agent(agent, primary_planner_id=workflow.primary_planner_id)
            profile = self.compile(recipe)
            binding = (
                workflow.harness_bindings.workflow_supervisor
                if recipe.role == "planner"
                else workflow.harness_bindings.task_execution
            )
            profiles.append(
                profile.model_copy(
                    update={
                        "agent_name": agent.name,
                        "role_card": agent.role_card,
                        "harness_runtime_profile_id": agent.runtime_profile_id or binding.profile_id,
                        "harness_provider_id": binding.provider_id,
                    }
                )
            )
        return profiles


def compile_agent_recipe(recipe: AgentRecipe) -> AgentRuntimeProfile:
    return RuntimeProfileCompiler().compile(recipe)


def compile_agent_workflow_profiles(workflow: "AgentWorkflowSpec") -> list[AgentRuntimeProfile]:
    return RuntimeProfileCompiler().compile_workflow(workflow)


def _engine_id(role: str) -> str:
    return {
        "planner": "planner-engine",
        "executor": "code-worker-engine",
    }.get(role, "code-worker-engine")


def _harness_id(role: str) -> str | None:
    if role == "executor":
        return "code-worker-harness"
    return None


def _harness_mode(role: str) -> str:
    if role == "planner":
        return "workflow_supervisor"
    return "task_execution"


def _default_harness_runtime_profile_id(role: str) -> str:
    if role == "planner":
        return "openhands-workflow-supervisor-default"
    return "openhands-task-executor-default"


def _context_profile(role: str) -> str:
    if role == "planner":
        return "planner-index-only"
    return "coding-executor"


def _context_policy(role: str) -> dict[str, Any]:
    if role == "planner":
        return {"skill_load_mode": "index_only", "memory": "workflow_summary", "max_skill_tokens": 0}
    return {"skill_load_mode": "on_demand", "memory": "direct_refs_only", "max_skill_tokens": 1600}


def _token_budget(role: str, *, planner_preferences: dict[str, Any] | None) -> TokenBudget:
    planner_strength = str((planner_preferences or {}).get("strength") or "balanced")
    planner_budget = {"fast": 8000, "balanced": 12000, "strong": 18000}.get(planner_strength, 12000)
    budgets = {
        "planner": planner_budget,
        "executor": 9000,
    }
    return TokenBudget(max_input_tokens=budgets.get(role, 8000))


def _allowed_artifacts(role: str) -> list[str]:
    if role == "planner":
        return ["run_contract", "planner_order", "planner_decision", "round_summary"]
    return ["execution_result"]


def _plugin_policy(role: str, preferred_extension_ids: list[str], run_contract: dict[str, Any] | None) -> dict[str, Any]:
    return {
        "mode": "route_per_work_item",
        "preferred_extension_ids": preferred_extension_ids,
        "external_effects_require_preview": True,
        "run_contract_required": bool(run_contract),
        "can_execute_plugins": role == "executor",
    }


def _skill_policy(role: str, preferred_extension_ids: list[str]) -> dict[str, Any]:
    return {
        "mode": "route_relevant_sections",
        "preferred_extension_ids": preferred_extension_ids,
        "load_full_body": False,
        "max_skills": 5 if role != "planner" else 0,
    }


def _memory_policy(role: str) -> dict[str, Any]:
    return {
        "can_read_workflow_memory": role == "planner",
        "can_write_long_term_memory": role == "planner",
    }


def _prompt_layers(role: str) -> dict[str, Any]:
    if role == "planner":
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


def _repair_policy(role: str) -> dict[str, Any]:
    return {
        "schema_repair_attempts": 1,
        "fallback_artifact": "blocked" if role != "planner" else "finish_blocked",
    }


def _internal_loops(role: str) -> dict[str, Any]:
    return {
        "schema_repair_attempts": 1,
        "self_check": role == "executor",
        "planner_repair": role == "planner",
        "verification_repair_attempts": 1 if role == "executor" else 0,
    }


def _tool_policy(role: str) -> dict[str, Any]:
    return {
        "read_files": role == "executor",
        "write_files": role == "executor",
        "run_commands": role == "executor",
        "ask_human": False,
    }


def _evaluation_profile(role: str) -> dict[str, Any]:
    if role == "planner":
        return {"artifact_type": "planner_order", "metrics": ["plan_valid_rate", "wrong_skill_selection_rate"]}
    return {"artifact_type": "execution_result", "metrics": ["schema_valid_rate", "blocked_rate", "verification_evidence_rate"]}
