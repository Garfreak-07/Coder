from .agent_workflow import (
    AgentWorkflowAgent,
    AgentWorkflowEdge,
    AgentWorkflowLoopPolicy,
    AgentWorkflowSpec,
    AgentWorkflowUi,
    AgentWorkflowValidationError,
    AgentWorkflowValidationIssue,
    AgentWorkflowValidationResult,
    CapabilityPermissions,
    CapabilitySpec,
    assert_valid_agent_workflow,
    capability_catalog,
    capability_registry,
    default_planner_led_agent_workflow,
    validate_agent_workflow,
    validate_agent_workflow_payload,
)
from .authority import (
    AUTHORITY_PROFILES,
    AgentAuthorityProfile,
    authority_catalog,
    authority_profile_for_agent,
)
from .archetypes import (
    RoleCardSpec,
    compile_agent_runtime_profile,
    compile_runtime_profiles,
    default_capabilities_for_role,
    role_card_catalog,
    role_card_for_id,
    role_card_registry,
)

__all__ = [
    "AgentWorkflowAgent",
    "AgentWorkflowEdge",
    "AgentWorkflowLoopPolicy",
    "AgentWorkflowSpec",
    "AgentWorkflowUi",
    "AgentWorkflowValidationError",
    "AgentWorkflowValidationIssue",
    "AgentWorkflowValidationResult",
    "AgentRuntimeProfile",
    "CapabilityPermissions",
    "CapabilitySpec",
    "RoleCardSpec",
    "AUTHORITY_PROFILES",
    "AgentAuthorityProfile",
    "assert_valid_agent_workflow",
    "authority_catalog",
    "authority_profile_for_agent",
    "capability_catalog",
    "capability_registry",
    "compile_agent_runtime_profile",
    "compile_runtime_profiles",
    "default_capabilities_for_role",
    "default_planner_led_agent_workflow",
    "role_card_catalog",
    "role_card_for_id",
    "role_card_registry",
    "validate_agent_workflow",
    "validate_agent_workflow_payload",
]


def __getattr__(name: str):
    if name == "AgentRuntimeProfile":
        from coder_workbench.agent_model.profile import AgentRuntimeProfile

        return AgentRuntimeProfile
    raise AttributeError(name)
