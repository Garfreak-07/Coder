use coder_config::ProjectConfig;
use coder_store::{RunStore, StoreError};
use thiserror::Error;

mod backend_registry;
mod browser_verifier;
mod browser_verifier_dynamic;
mod context_budget;
mod context_compaction;
mod mock_runner;
mod model_tool_loop;
mod native_backend;
mod provider_streaming;
mod subagent_context;
mod subagent_runtime;
mod tool_execution;
mod workflow_backend_execution;
mod workflow_compaction_events;
mod workflow_context_projection;
mod workflow_control;
mod workflow_events;
mod workflow_graph;
mod workflow_harness_request;
mod workflow_reports;
mod workflow_run_types;
mod workflow_runner_core;
mod workflow_verification;
pub use backend_registry::{BackendRegistry, PlannerModelBackend};
pub use browser_verifier::BrowserVerifierBackend;
pub use browser_verifier_dynamic::{
    browser_verifier_runtime_status, BrowserVerifierPlaywrightCandidate,
    BrowserVerifierRuntimeStatus,
};
pub use context_budget::{context_budget_for_runtime, ContextBudget};
pub use mock_runner::{MockRunOptions, MockRunOutcome, MockRunOutput, MockWorkflowRunner};
pub use model_tool_loop::{
    execute_model_tool_turn, model_tool_concurrency, synthesize_missing_model_tool_results,
    ModelToolExecutionError, ModelToolExecutionRequest, ModelToolExecutionResult,
    ModelToolExecutor, ModelToolHostContext, ModelToolLoopOptions, ModelToolResultBlock,
    ModelToolTurnOutput, ModelToolUseBlock, MODEL_TOOL_RESULT_CONTRACT,
};
pub(crate) use native_backend::{native_selected_tools, truncate_public};
pub use native_backend::{NativeMockBackend, NativeMockOutcome, NativeRustBackend};
pub use provider_streaming::{
    OpenAiCompatibleStreamAdapter, ProviderStreamEvent, ProviderStreamEventKind,
    ProviderStreamFinal, ProviderStreamIssue,
};
pub use subagent_context::SubagentInvocationKind;
pub use subagent_runtime::{SubagentRunInput, SubagentRunOutput, SubagentRuntime};
pub use tool_execution::ToolConcurrency;
pub use workflow_run_types::{
    replay_run_status, WorkflowRunControl, WorkflowRunOptions, WorkflowRunOutput,
};

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("workflow not found: {0}")]
    WorkflowNotFound(String),
    #[error("backend not found: {0}")]
    BackendNotFound(String),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
}

pub struct WorkflowRunner {
    config: ProjectConfig,
    store: RunStore,
    backends: BackendRegistry,
}

impl WorkflowRunner {
    pub fn new(config: ProjectConfig, store: RunStore) -> Self {
        let backends = BackendRegistry::from_project_config(&config, store.clone());
        Self {
            config,
            store,
            backends,
        }
    }

    pub fn with_registry(
        config: ProjectConfig,
        store: RunStore,
        backends: BackendRegistry,
    ) -> Self {
        Self {
            config,
            store,
            backends,
        }
    }
}

#[cfg(test)]
mod tests;
