from __future__ import annotations

from coder_workbench.core.agent_workflow import AgentWorkflowSpec, _compile_agent_workflow_legacy_impl
from coder_workbench.core.schema import WorkflowSpec


def compile_agent_workflow_legacy_preview(spec: AgentWorkflowSpec) -> WorkflowSpec:
    """Compile AgentWorkflowSpec for legacy runtime preview only.

    This remains for advanced runtime preview and legacy compatibility only.
    The normal AgentGraphRuntime path must call AgentGraphRunner directly.
    """

    return _compile_agent_workflow_legacy_impl(spec)


def compile_agent_workflow(spec: AgentWorkflowSpec) -> WorkflowSpec:
    """Compatibility alias for compile_agent_workflow_legacy_preview."""

    return compile_agent_workflow_legacy_preview(spec)
