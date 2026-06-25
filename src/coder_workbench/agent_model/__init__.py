from .token_budget import TokenBudget

__all__ = [
    "AgentRecipe",
    "AgentRecipeRole",
    "AgentRuntimeProfile",
    "RuntimeProfileCompiler",
    "RuntimeProfileCache",
    "RuntimeProfileCacheResult",
    "TokenBudget",
    "compile_agent_recipe",
    "compile_agent_workflow_profiles",
    "recipe_from_workflow_agent",
    "runtime_profile_hash",
]


def __getattr__(name: str):
    if name in {"RuntimeProfileCompiler", "compile_agent_recipe", "compile_agent_workflow_profiles"}:
        from .compiler import RuntimeProfileCompiler, compile_agent_recipe, compile_agent_workflow_profiles

        values = {
            "RuntimeProfileCompiler": RuntimeProfileCompiler,
            "compile_agent_recipe": compile_agent_recipe,
            "compile_agent_workflow_profiles": compile_agent_workflow_profiles,
        }
        return values[name]
    if name in {"RuntimeProfileCache", "RuntimeProfileCacheResult", "runtime_profile_hash"}:
        from .compiler_cache import RuntimeProfileCache, RuntimeProfileCacheResult, runtime_profile_hash

        values = {
            "RuntimeProfileCache": RuntimeProfileCache,
            "RuntimeProfileCacheResult": RuntimeProfileCacheResult,
            "runtime_profile_hash": runtime_profile_hash,
        }
        return values[name]
    if name == "AgentRuntimeProfile":
        from .profile import AgentRuntimeProfile

        return AgentRuntimeProfile
    if name in {"AgentRecipe", "AgentRecipeRole", "recipe_from_workflow_agent"}:
        from .recipe import AgentRecipe, AgentRecipeRole, recipe_from_workflow_agent

        values = {
            "AgentRecipe": AgentRecipe,
            "AgentRecipeRole": AgentRecipeRole,
            "recipe_from_workflow_agent": recipe_from_workflow_agent,
        }
        return values[name]
    raise AttributeError(name)
