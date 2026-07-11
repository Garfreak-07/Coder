use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use async_trait::async_trait;
use coder_core::RunId;
use coder_harness::HarnessRunEventRef;
use serde_json::{json, Value};
use tokio::task::JoinSet;

use crate::tool_execution::{
    max_tool_use_concurrency_from_env, StreamingToolExecutorState,
    StreamingToolSyntheticErrorReason, StreamingToolUpdate, StreamingToolUpdateKind,
    ToolConcurrency,
};

pub const MODEL_TOOL_RESULT_CONTRACT: &str = "coder.model_tool_result.v1";

#[derive(Debug, Clone, PartialEq)]
pub struct ModelToolUseBlock {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub concurrency: ToolConcurrency,
}

impl ModelToolUseBlock {
    pub fn new(id: impl Into<String>, name: impl Into<String>, input: Value) -> Self {
        let name = name.into();
        let concurrency = model_tool_concurrency(&name);
        Self {
            id: id.into(),
            name,
            input,
            concurrency,
        }
    }

    pub fn with_concurrency(
        id: impl Into<String>,
        name: impl Into<String>,
        input: Value,
        concurrency: ToolConcurrency,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            input,
            concurrency,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelToolExecutionRequest {
    pub tool_use_id: String,
    pub tool_name: String,
    pub input: Value,
    pub host_context: ModelToolHostContext,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelToolHostContext {
    pub run_id: Option<String>,
    pub harness_id: Option<String>,
    pub agent_id: Option<String>,
    pub current_model: Option<String>,
    pub current_effort: Option<Value>,
    pub drain_later_notifications: bool,
    pub skill_context_modifiers: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct ModelToolExecutionResult {
    pub tool_use_id: String,
    pub tool_name: String,
    pub status: String,
    pub is_error: bool,
    pub content: String,
    pub content_truncated: bool,
    pub payload: Value,
    pub refs: Vec<HarnessRunEventRef>,
    pub cancels_siblings: bool,
    pub phases: Vec<Value>,
}

impl ModelToolExecutionResult {
    pub fn completed(
        tool_use_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        let tool_use_id = tool_use_id.into();
        let tool_name = tool_name.into();
        let content = content.into();
        Self {
            tool_use_id,
            tool_name,
            status: "completed".to_owned(),
            is_error: false,
            content: content.clone(),
            content_truncated: false,
            payload: json!({
                "status": "completed",
                "content": content
            }),
            refs: Vec::new(),
            cancels_siblings: false,
            phases: Vec::new(),
        }
    }

    pub fn failed(
        tool_use_id: impl Into<String>,
        tool_name: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let tool_use_id = tool_use_id.into();
        let tool_name = tool_name.into();
        let message = message.into();
        Self {
            tool_use_id,
            tool_name,
            status: "failed".to_owned(),
            is_error: true,
            content: tool_use_error_content(&message),
            content_truncated: false,
            payload: json!({
                "status": "failed",
                "error": message
            }),
            refs: Vec::new(),
            cancels_siblings: false,
            phases: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelToolExecutionError {
    pub message: String,
    pub cancels_siblings: bool,
}

impl ModelToolExecutionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cancels_siblings: false,
        }
    }

    pub fn cancels_siblings(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cancels_siblings: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelToolResultBlock {
    pub contract: &'static str,
    pub source: &'static str,
    pub result_type: &'static str,
    pub tool_use_id: String,
    pub tool_name: String,
    pub status: String,
    pub is_error: bool,
    pub content: String,
    pub content_truncated: bool,
    pub payload: Value,
    pub refs: Vec<HarnessRunEventRef>,
    pub phases: Vec<Value>,
    pub claude_sources: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelToolLoopOptions {
    pub max_tool_use_concurrency: usize,
    pub host_context: ModelToolHostContext,
}

impl Default for ModelToolLoopOptions {
    fn default() -> Self {
        Self {
            max_tool_use_concurrency: max_tool_use_concurrency_from_env(),
            host_context: ModelToolHostContext::default(),
        }
    }
}

impl ModelToolLoopOptions {
    pub fn with_max_tool_use_concurrency(max_tool_use_concurrency: usize) -> Self {
        Self {
            max_tool_use_concurrency: max_tool_use_concurrency.max(1),
            host_context: ModelToolHostContext::default(),
        }
    }

    pub fn with_host_context(mut self, host_context: ModelToolHostContext) -> Self {
        self.host_context = host_context;
        self
    }
}

#[derive(Debug, Clone)]
pub struct ModelToolTurnOutput {
    pub contract: &'static str,
    pub source: &'static str,
    pub results: Vec<ModelToolResultBlock>,
    pub attachments: Vec<Value>,
    pub claude_sources: Vec<&'static str>,
}

#[async_trait]
pub trait ModelToolExecutor: Send + Sync {
    async fn execute_model_tool(
        &self,
        request: ModelToolExecutionRequest,
    ) -> Result<ModelToolExecutionResult, ModelToolExecutionError>;

    async fn finalize_model_tool_turn_results(
        &self,
        results: Vec<ModelToolResultBlock>,
    ) -> Vec<ModelToolResultBlock> {
        results
    }

    async fn drain_model_tool_turn_attachments(
        &self,
        _host_context: &ModelToolHostContext,
        _results: &[ModelToolResultBlock],
    ) -> Vec<Value> {
        Vec::new()
    }

    async fn drain_idle_model_tool_attachments(&self, _run_id: &RunId) -> Vec<Value> {
        Vec::new()
    }
}

pub async fn execute_model_tool_turn(
    tool_uses: Vec<ModelToolUseBlock>,
    executor: Arc<dyn ModelToolExecutor>,
    options: ModelToolLoopOptions,
) -> ModelToolTurnOutput {
    let mut state =
        StreamingToolExecutorState::with_max_concurrency(options.max_tool_use_concurrency);
    let mut host_context = options.host_context.clone();
    if tool_uses
        .iter()
        .any(|block| is_model_tool_sleep_name(&block.name))
    {
        host_context.drain_later_notifications = true;
    }
    let mut tools_by_id = BTreeMap::new();
    let mut ready_ids = Vec::new();

    for block in tool_uses {
        tools_by_id.insert(block.id.clone(), block.clone());
        ready_ids.extend(state.add_tool(block.id.clone(), block.name.clone(), block.concurrency));
    }

    let mut pending_results = BTreeMap::new();
    let mut emitted_ids = BTreeSet::new();
    let mut results = Vec::new();

    while !ready_ids.is_empty() {
        let mut join_set = JoinSet::new();
        for tool_id in &ready_ids {
            let Some(block) = tools_by_id.get(tool_id).cloned() else {
                continue;
            };
            let executor = Arc::clone(&executor);
            let host_context = host_context.clone();
            let tool_id = tool_id.clone();
            join_set.spawn(async move {
                let request = ModelToolExecutionRequest {
                    tool_use_id: block.id.clone(),
                    tool_name: block.name.clone(),
                    input: block.input.clone(),
                    host_context,
                };
                let outcome = executor.execute_model_tool(request).await;
                (tool_id, block, outcome)
            });
        }

        let mut next_ready_ids = Vec::new();
        while let Some(joined) = join_set.join_next().await {
            let Ok((tool_id, block, outcome)) = joined else {
                continue;
            };
            if emitted_ids.contains(&tool_id) {
                continue;
            }

            let execution_result = match outcome {
                Ok(result) => normalize_execution_result(&block, result),
                Err(error) => execution_error_result(&block, error),
            };
            let cancels_siblings = execution_result.cancels_siblings;
            let result_block = result_block_from_execution(execution_result);
            let is_error = result_block.is_error;
            let content = result_block.content.clone();
            pending_results.insert(tool_id.clone(), result_block);

            next_ready_ids.extend(state.complete_tool(
                &tool_id,
                content,
                is_error,
                cancels_siblings,
            ));
            push_model_tool_updates(
                state.yield_available(),
                &mut pending_results,
                &mut results,
                &mut emitted_ids,
            );

            if is_error && cancels_siblings {
                join_set.abort_all();
            }
        }
        ready_ids = next_ready_ids;
    }

    state.cancel_unfinished(StreamingToolSyntheticErrorReason::MissingToolResult);
    push_model_tool_updates(
        state.yield_available(),
        &mut pending_results,
        &mut results,
        &mut emitted_ids,
    );
    let results = executor.finalize_model_tool_turn_results(results).await;
    let attachments = executor
        .drain_model_tool_turn_attachments(&host_context, &results)
        .await;

    ModelToolTurnOutput {
        contract: "coder.model_tool_turn.v1",
        source: "coder-workflow",
        results,
        attachments,
        claude_sources: claude_tool_loop_sources(),
    }
}

fn is_model_tool_sleep_name(tool_name: &str) -> bool {
    matches!(tool_name, "sleep" | "Sleep" | "sleep_tool" | "SleepTool")
}

pub fn synthesize_missing_model_tool_results(
    tool_uses: Vec<ModelToolUseBlock>,
) -> Vec<ModelToolResultBlock> {
    let mut state = StreamingToolExecutorState::with_max_concurrency(1);
    for block in tool_uses {
        state.add_tool(block.id, block.name, block.concurrency);
    }
    state.cancel_unfinished(StreamingToolSyntheticErrorReason::MissingToolResult);

    let mut results = Vec::new();
    for update in state.yield_available() {
        if update.kind != StreamingToolUpdateKind::Progress {
            results.push(synthetic_result_block_from_update(update));
        }
    }
    results
}

pub fn model_tool_concurrency(tool_name: &str) -> ToolConcurrency {
    match tool_name {
        "repo_find_files"
        | "find_files"
        | "repo_files"
        | "search_files"
        | "repo_search_text"
        | "repo_search"
        | "search_text"
        | "repo_read_file"
        | "read_file"
        | "repo_read_file_range"
        | "read_file_range"
        | "git_status"
        | "git_diff"
        | "inspect_git_diff"
        | "read_command_output"
        | "read_subagent_status"
        | "TaskOutput"
        | "task_output"
        | "AgentOutputTool"
        | "BashOutputTool" => ToolConcurrency::ConcurrentSafe,
        "agent_subagent"
        | "agent"
        | "subagent"
        | "Skill"
        | "skill"
        | "SkillTool"
        | "skill_tool"
        | "command_run"
        | "run_command"
        | "run_command_sandbox"
        | "command_background"
        | "cancel_command_background"
        | "cancel_subagent_background"
        | "TaskStop"
        | "task_stop"
        | "KillShell"
        | "kill_shell"
        | "patch_preview"
        | "preview_patch"
        | "propose_patch"
        | "patch_apply"
        | "apply_patch"
        | "apply_patch_sandbox" => ToolConcurrency::Exclusive,
        _ => ToolConcurrency::ConcurrentSafe,
    }
}

fn normalize_execution_result(
    block: &ModelToolUseBlock,
    mut result: ModelToolExecutionResult,
) -> ModelToolExecutionResult {
    result.tool_use_id = block.id.clone();
    result.tool_name = block.name.clone();
    result
}

fn execution_error_result(
    block: &ModelToolUseBlock,
    error: ModelToolExecutionError,
) -> ModelToolExecutionResult {
    ModelToolExecutionResult {
        tool_use_id: block.id.clone(),
        tool_name: block.name.clone(),
        status: "failed".to_owned(),
        is_error: true,
        content: tool_use_error_content(&format!(
            "Error calling tool ({}): {}",
            block.name, error.message
        )),
        content_truncated: false,
        payload: json!({
            "status": "failed",
            "error": error.message
        }),
        refs: Vec::new(),
        cancels_siblings: error.cancels_siblings,
        phases: Vec::new(),
    }
}

fn result_block_from_execution(result: ModelToolExecutionResult) -> ModelToolResultBlock {
    ModelToolResultBlock {
        contract: MODEL_TOOL_RESULT_CONTRACT,
        source: "coder-workflow",
        result_type: "tool_result",
        tool_use_id: result.tool_use_id,
        tool_name: result.tool_name,
        status: result.status,
        is_error: result.is_error,
        content: result.content,
        content_truncated: result.content_truncated,
        payload: result.payload,
        refs: result.refs,
        phases: result.phases,
        claude_sources: claude_tool_loop_sources(),
    }
}

fn push_model_tool_updates(
    updates: Vec<StreamingToolUpdate>,
    pending_results: &mut BTreeMap<String, ModelToolResultBlock>,
    results: &mut Vec<ModelToolResultBlock>,
    emitted_ids: &mut BTreeSet<String>,
) {
    for update in updates {
        if update.kind == StreamingToolUpdateKind::Progress {
            continue;
        }
        let tool_id = update.tool_id.clone();
        let result = pending_results
            .remove(&tool_id)
            .unwrap_or_else(|| synthetic_result_block_from_update(update));
        emitted_ids.insert(tool_id);
        results.push(result);
    }
}

fn synthetic_result_block_from_update(update: StreamingToolUpdate) -> ModelToolResultBlock {
    let is_error = update.kind == StreamingToolUpdateKind::SyntheticError;
    let status = if is_error { "failed" } else { "completed" };
    let content = if is_error {
        tool_use_error_content(&update.content)
    } else {
        update.content.clone()
    };
    ModelToolResultBlock {
        contract: MODEL_TOOL_RESULT_CONTRACT,
        source: "coder-workflow",
        result_type: "tool_result",
        tool_use_id: update.tool_id,
        tool_name: update.tool_name,
        status: status.to_owned(),
        is_error,
        content,
        content_truncated: false,
        payload: json!({
            "status": status,
            "synthetic": true,
            "message": update.content,
            "update_kind": update.kind.as_str()
        }),
        refs: Vec::new(),
        phases: Vec::new(),
        claude_sources: claude_tool_loop_sources(),
    }
}

fn tool_use_error_content(message: &str) -> String {
    if message.contains("<tool_use_error>") {
        message.to_owned()
    } else {
        format!("<tool_use_error>{message}</tool_use_error>")
    }
}

fn claude_tool_loop_sources() -> Vec<&'static str> {
    vec![
        "src/query.ts",
        "src/services/tools/StreamingToolExecutor.ts",
        "src/services/tools/toolExecution.ts",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
        time::Duration,
    };

    #[derive(Clone)]
    struct StubResponse {
        delay_ms: u64,
        outcome: Result<ModelToolExecutionResult, ModelToolExecutionError>,
    }

    struct StubExecutor {
        responses: BTreeMap<String, StubResponse>,
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ModelToolExecutor for StubExecutor {
        async fn execute_model_tool(
            &self,
            request: ModelToolExecutionRequest,
        ) -> Result<ModelToolExecutionResult, ModelToolExecutionError> {
            self.events
                .lock()
                .unwrap()
                .push(format!("start:{}", request.tool_use_id));
            let response = self.responses.get(&request.tool_use_id).unwrap().clone();
            if response.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(response.delay_ms)).await;
            }
            self.events
                .lock()
                .unwrap()
                .push(format!("finish:{}", request.tool_use_id));
            response.outcome
        }
    }

    struct FinalizingExecutor {
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ModelToolExecutor for FinalizingExecutor {
        async fn execute_model_tool(
            &self,
            request: ModelToolExecutionRequest,
        ) -> Result<ModelToolExecutionResult, ModelToolExecutionError> {
            self.events
                .lock()
                .unwrap()
                .push(format!("execute:{}", request.tool_use_id));
            Ok(ModelToolExecutionResult::completed(
                request.tool_use_id,
                request.tool_name,
                "raw result",
            ))
        }

        async fn finalize_model_tool_turn_results(
            &self,
            mut results: Vec<ModelToolResultBlock>,
        ) -> Vec<ModelToolResultBlock> {
            self.events.lock().unwrap().push("finalize".to_owned());
            for result in &mut results {
                result.content.push_str(" finalized");
            }
            results
        }
    }

    struct AttachmentExecutor {
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ModelToolExecutor for AttachmentExecutor {
        async fn execute_model_tool(
            &self,
            request: ModelToolExecutionRequest,
        ) -> Result<ModelToolExecutionResult, ModelToolExecutionError> {
            self.events
                .lock()
                .unwrap()
                .push(format!("execute:{}", request.tool_use_id));
            Ok(ModelToolExecutionResult::completed(
                request.tool_use_id,
                request.tool_name,
                "raw result",
            ))
        }

        async fn drain_model_tool_turn_attachments(
            &self,
            host_context: &ModelToolHostContext,
            results: &[ModelToolResultBlock],
        ) -> Vec<Value> {
            self.events.lock().unwrap().push(format!(
                "drain:{}:{}",
                host_context.run_id.as_deref().unwrap_or("none"),
                results.len()
            ));
            vec![json!({
                "type": "queued_command",
                "prompt": "background notice"
            })]
        }
    }

    fn executor(
        responses: impl IntoIterator<Item = (&'static str, StubResponse)>,
        events: Arc<Mutex<Vec<String>>>,
    ) -> Arc<dyn ModelToolExecutor> {
        Arc::new(StubExecutor {
            responses: responses
                .into_iter()
                .map(|(id, response)| (id.to_owned(), response))
                .collect(),
            events,
        })
    }

    fn success(id: &'static str, tool: &'static str, content: &'static str) -> StubResponse {
        StubResponse {
            delay_ms: 0,
            outcome: Ok(ModelToolExecutionResult::completed(id, tool, content)),
        }
    }

    #[tokio::test]
    async fn model_tool_turn_calls_executor_result_finalizer() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let executor = Arc::new(FinalizingExecutor {
            events: Arc::clone(&events),
        });

        let output = execute_model_tool_turn(
            vec![ModelToolUseBlock::new(
                "tool-finalize",
                "repo_read_file",
                json!({}),
            )],
            executor,
            ModelToolLoopOptions::with_max_tool_use_concurrency(10),
        )
        .await;

        assert_eq!(
            events.lock().unwrap().clone(),
            vec!["execute:tool-finalize", "finalize"]
        );
        assert_eq!(output.results[0].content, "raw result finalized");
    }

    #[tokio::test]
    async fn model_tool_turn_drains_executor_attachments_after_results() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let executor = Arc::new(AttachmentExecutor {
            events: Arc::clone(&events),
        });

        let output = execute_model_tool_turn(
            vec![ModelToolUseBlock::new(
                "tool-attachment",
                "repo_read_file",
                json!({}),
            )],
            executor,
            ModelToolLoopOptions::with_max_tool_use_concurrency(10).with_host_context(
                ModelToolHostContext {
                    run_id: Some("run-attachments".to_owned()),
                    harness_id: Some("native-code-edit".to_owned()),
                    ..ModelToolHostContext::default()
                },
            ),
        )
        .await;

        assert_eq!(
            events.lock().unwrap().clone(),
            vec!["execute:tool-attachment", "drain:run-attachments:1"]
        );
        assert_eq!(output.attachments.len(), 1);
        assert_eq!(output.attachments[0]["type"], "queued_command");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn model_tool_turn_runs_safe_tools_concurrently_but_returns_ordered_results() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut slow = success("tool-1", "repo_read_file", "first");
        slow.delay_ms = 40;
        let mut fast = success("tool-2", "git_diff", "second");
        fast.delay_ms = 1;
        let executor = executor([("tool-1", slow), ("tool-2", fast)], events.clone());

        let output = execute_model_tool_turn(
            vec![
                ModelToolUseBlock::new("tool-1", "repo_read_file", json!({})),
                ModelToolUseBlock::new("tool-2", "git_diff", json!({})),
            ],
            executor,
            ModelToolLoopOptions::with_max_tool_use_concurrency(2),
        )
        .await;

        assert_eq!(
            output
                .results
                .iter()
                .map(|result| result.tool_use_id.as_str())
                .collect::<Vec<_>>(),
            vec!["tool-1", "tool-2"]
        );
        let events = events.lock().unwrap().clone();
        assert!(
            events
                .iter()
                .position(|event| event == "start:tool-2")
                .unwrap()
                < events
                    .iter()
                    .position(|event| event == "finish:tool-1")
                    .unwrap()
        );
    }

    #[tokio::test]
    async fn model_tool_turn_honors_exclusive_barriers() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let executor = executor(
            [
                ("read", success("read", "repo_read_file", "read")),
                ("bash", success("bash", "command_run", "bash")),
                ("diff", success("diff", "git_diff", "diff")),
            ],
            events.clone(),
        );

        let output = execute_model_tool_turn(
            vec![
                ModelToolUseBlock::new("read", "repo_read_file", json!({})),
                ModelToolUseBlock::new("bash", "command_run", json!({})),
                ModelToolUseBlock::new("diff", "git_diff", json!({})),
            ],
            executor,
            ModelToolLoopOptions::with_max_tool_use_concurrency(10),
        )
        .await;

        assert_eq!(output.results.len(), 3);
        assert_eq!(
            events.lock().unwrap().clone(),
            vec![
                "start:read",
                "finish:read",
                "start:bash",
                "finish:bash",
                "start:diff",
                "finish:diff"
            ]
        );
    }

    #[tokio::test]
    async fn model_tool_turn_wraps_executor_errors_as_tool_results() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let executor = executor(
            [(
                "bad",
                StubResponse {
                    delay_ms: 0,
                    outcome: Err(ModelToolExecutionError::new("boom")),
                },
            )],
            events,
        );

        let output = execute_model_tool_turn(
            vec![ModelToolUseBlock::new("bad", "repo_search_text", json!({}))],
            executor,
            ModelToolLoopOptions::with_max_tool_use_concurrency(10),
        )
        .await;

        assert_eq!(output.results.len(), 1);
        assert!(output.results[0].is_error);
        assert!(output.results[0]
            .content
            .contains("<tool_use_error>Error calling tool (repo_search_text): boom"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn model_tool_turn_synthesizes_sibling_errors() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut slow = success("read", "repo_read_file", "read");
        slow.delay_ms = 50;
        let mut failing = StubResponse {
            delay_ms: 1,
            outcome: Err(ModelToolExecutionError::cancels_siblings("shell failed")),
        };
        failing.delay_ms = 1;
        let executor = executor([("read", slow), ("bash", failing)], events);

        let output = execute_model_tool_turn(
            vec![
                ModelToolUseBlock::with_concurrency(
                    "read",
                    "repo_read_file",
                    json!({}),
                    ToolConcurrency::ConcurrentSafe,
                ),
                ModelToolUseBlock::with_concurrency(
                    "bash",
                    "command_run",
                    json!({}),
                    ToolConcurrency::ConcurrentSafe,
                ),
            ],
            executor,
            ModelToolLoopOptions::with_max_tool_use_concurrency(2),
        )
        .await;

        assert_eq!(output.results.len(), 2);
        assert!(output.results.iter().all(|result| result.is_error));
        assert_eq!(output.results[0].tool_use_id, "read");
        assert!(output.results[0].content.contains("Cancelled"));
        assert_eq!(output.results[1].tool_use_id, "bash");
    }

    #[test]
    fn missing_model_tool_results_are_synthetic_error_blocks() {
        let results = synthesize_missing_model_tool_results(vec![
            ModelToolUseBlock::new("tool-1", "repo_search_text", json!({})),
            ModelToolUseBlock::new("tool-2", "repo_read_file", json!({})),
        ]);

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.is_error));
        assert!(results.iter().all(|result| result
            .content
            .contains("before this tool produced a result")));
    }

    #[test]
    fn model_tool_concurrency_classifies_side_effect_tools_as_exclusive() {
        assert_eq!(
            model_tool_concurrency("repo_read_file"),
            ToolConcurrency::ConcurrentSafe
        );
        assert_eq!(
            model_tool_concurrency("TaskOutput"),
            ToolConcurrency::ConcurrentSafe
        );
        assert_eq!(
            model_tool_concurrency("command_run"),
            ToolConcurrency::Exclusive
        );
        assert_eq!(
            model_tool_concurrency("TaskStop"),
            ToolConcurrency::Exclusive
        );
        assert_eq!(
            model_tool_concurrency("KillShell"),
            ToolConcurrency::Exclusive
        );
        assert_eq!(
            model_tool_concurrency("patch_apply"),
            ToolConcurrency::Exclusive
        );
        assert_eq!(
            model_tool_concurrency("agent_subagent"),
            ToolConcurrency::Exclusive
        );
        assert_eq!(model_tool_concurrency("Skill"), ToolConcurrency::Exclusive);
    }
}
