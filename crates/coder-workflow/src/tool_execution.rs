use std::env;

pub const DEFAULT_MAX_TOOL_USE_CONCURRENCY: usize = 10;
pub const CODER_MAX_TOOL_USE_CONCURRENCY_ENV: &str = "CODER_MAX_TOOL_USE_CONCURRENCY";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolConcurrency {
    ConcurrentSafe,
    Exclusive,
}

impl ToolConcurrency {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConcurrentSafe => "concurrent_safe",
            Self::Exclusive => "exclusive",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionStep {
    pub tool: String,
    pub concurrency: ToolConcurrency,
}

impl ToolExecutionStep {
    pub fn new(tool: impl Into<String>, concurrency: ToolConcurrency) -> Self {
        Self {
            tool: tool.into(),
            concurrency,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionBatch {
    pub concurrency: ToolConcurrency,
    pub tools: Vec<String>,
}

pub fn max_tool_use_concurrency_from_env() -> usize {
    let configured = env::var(CODER_MAX_TOOL_USE_CONCURRENCY_ENV).ok();
    parse_max_tool_use_concurrency(configured.as_deref())
}

fn parse_max_tool_use_concurrency(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_TOOL_USE_CONCURRENCY)
}

pub fn partition_tool_steps(
    steps: impl IntoIterator<Item = ToolExecutionStep>,
) -> Vec<ToolExecutionBatch> {
    let mut batches: Vec<ToolExecutionBatch> = Vec::new();
    for step in steps {
        if step.concurrency == ToolConcurrency::ConcurrentSafe {
            if let Some(last) = batches.last_mut() {
                if last.concurrency == ToolConcurrency::ConcurrentSafe {
                    last.tools.push(step.tool);
                    continue;
                }
            }
        }
        batches.push(ToolExecutionBatch {
            concurrency: step.concurrency,
            tools: vec![step.tool],
        });
    }
    batches
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingToolStatus {
    Queued,
    Executing,
    Completed,
    Yielded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamingToolUpdateKind {
    Result,
    SyntheticError,
}

impl StreamingToolUpdateKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Result => "result",
            Self::SyntheticError => "synthetic_error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamingToolUpdate {
    pub tool_id: String,
    pub tool_name: String,
    pub kind: StreamingToolUpdateKind,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedStreamingTool {
    id: String,
    name: String,
    concurrency: ToolConcurrency,
    status: StreamingToolStatus,
    results: Vec<(StreamingToolUpdateKind, String)>,
}

impl TrackedStreamingTool {
    fn new(id: impl Into<String>, name: impl Into<String>, concurrency: ToolConcurrency) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            concurrency,
            status: StreamingToolStatus::Queued,
            results: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamingToolExecutorState {
    tools: Vec<TrackedStreamingTool>,
    max_concurrent_tools: usize,
}

impl Default for StreamingToolExecutorState {
    fn default() -> Self {
        Self {
            tools: Vec::new(),
            max_concurrent_tools: usize::MAX,
        }
    }
}

impl StreamingToolExecutorState {
    pub fn with_max_concurrency(max_concurrent_tools: usize) -> Self {
        Self {
            max_concurrent_tools: max_concurrent_tools.max(1),
            ..Self::default()
        }
    }

    pub fn add_tool(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        concurrency: ToolConcurrency,
    ) -> Vec<String> {
        self.tools
            .push(TrackedStreamingTool::new(id, name, concurrency));
        self.start_ready_tools()
    }

    pub fn complete_tool(
        &mut self,
        tool_id: &str,
        content: impl Into<String>,
        is_error: bool,
        cancels_siblings: bool,
    ) -> Vec<String> {
        let Some(index) = self.tool_index(tool_id) else {
            return Vec::new();
        };
        let kind = if is_error {
            StreamingToolUpdateKind::SyntheticError
        } else {
            StreamingToolUpdateKind::Result
        };
        self.tools[index].results.push((kind, content.into()));
        self.tools[index].status = StreamingToolStatus::Completed;

        if is_error && cancels_siblings {
            let description = format!("{}({})", self.tools[index].name, self.tools[index].id);
            for (candidate_index, tool) in self.tools.iter_mut().enumerate() {
                if candidate_index == index || tool.status == StreamingToolStatus::Yielded {
                    continue;
                }
                if matches!(
                    tool.status,
                    StreamingToolStatus::Queued | StreamingToolStatus::Executing
                ) {
                    tool.status = StreamingToolStatus::Completed;
                    tool.results.clear();
                    tool.results.push((
                        StreamingToolUpdateKind::SyntheticError,
                        format!("Cancelled: parallel tool call {description} errored"),
                    ));
                }
            }
            return Vec::new();
        }

        self.start_ready_tools()
    }

    pub fn cancel_unfinished(&mut self) {
        for tool in &mut self.tools {
            if matches!(
                tool.status,
                StreamingToolStatus::Queued | StreamingToolStatus::Executing
            ) {
                tool.status = StreamingToolStatus::Completed;
                tool.results.clear();
                tool.results.push((
                    StreamingToolUpdateKind::SyntheticError,
                    "Model response ended before this tool produced a result".to_owned(),
                ));
            }
        }
    }

    pub fn yield_available(&mut self) -> Vec<StreamingToolUpdate> {
        let mut updates = Vec::new();
        for tool in &mut self.tools {
            if tool.status == StreamingToolStatus::Yielded {
                continue;
            }
            if tool.status == StreamingToolStatus::Completed {
                tool.status = StreamingToolStatus::Yielded;
                for (kind, content) in tool.results.drain(..) {
                    updates.push(StreamingToolUpdate {
                        tool_id: tool.id.clone(),
                        tool_name: tool.name.clone(),
                        kind,
                        content,
                    });
                }
            } else {
                break;
            }
        }
        updates
    }

    fn start_ready_tools(&mut self) -> Vec<String> {
        let mut started = Vec::new();
        let len = self.tools.len();
        for index in 0..len {
            if self.tools[index].status != StreamingToolStatus::Queued {
                continue;
            }
            if self.can_execute_tool(index) {
                self.tools[index].status = StreamingToolStatus::Executing;
                started.push(self.tools[index].id.clone());
            } else if self.tools[index].concurrency == ToolConcurrency::Exclusive {
                break;
            }
        }
        started
    }

    fn can_execute_tool(&self, index: usize) -> bool {
        let executing = self
            .tools
            .iter()
            .filter(|tool| tool.status == StreamingToolStatus::Executing)
            .collect::<Vec<_>>();
        executing.is_empty()
            || (executing.len() < self.max_concurrent_tools
                && self.tools[index].concurrency == ToolConcurrency::ConcurrentSafe
                && executing
                    .iter()
                    .all(|tool| tool.concurrency == ToolConcurrency::ConcurrentSafe))
    }

    fn tool_index(&self, tool_id: &str) -> Option<usize> {
        self.tools.iter().position(|tool| tool.id == tool_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_groups_consecutive_safe_tools_and_keeps_exclusive_single() {
        let batches = partition_tool_steps([
            ToolExecutionStep::new("read_file", ToolConcurrency::ConcurrentSafe),
            ToolExecutionStep::new("git_diff", ToolConcurrency::ConcurrentSafe),
            ToolExecutionStep::new("run_command", ToolConcurrency::Exclusive),
            ToolExecutionStep::new("repo_search", ToolConcurrency::ConcurrentSafe),
            ToolExecutionStep::new("apply_patch", ToolConcurrency::Exclusive),
        ]);

        assert_eq!(
            batches,
            vec![
                ToolExecutionBatch {
                    concurrency: ToolConcurrency::ConcurrentSafe,
                    tools: vec!["read_file".to_owned(), "git_diff".to_owned()]
                },
                ToolExecutionBatch {
                    concurrency: ToolConcurrency::Exclusive,
                    tools: vec!["run_command".to_owned()]
                },
                ToolExecutionBatch {
                    concurrency: ToolConcurrency::ConcurrentSafe,
                    tools: vec!["repo_search".to_owned()]
                },
                ToolExecutionBatch {
                    concurrency: ToolConcurrency::Exclusive,
                    tools: vec!["apply_patch".to_owned()]
                }
            ]
        );
    }

    #[test]
    fn max_tool_use_concurrency_uses_default_and_positive_override() {
        assert_eq!(parse_max_tool_use_concurrency(None), 10);
        assert_eq!(parse_max_tool_use_concurrency(Some("")), 10);
        assert_eq!(parse_max_tool_use_concurrency(Some("0")), 10);
        assert_eq!(parse_max_tool_use_concurrency(Some("4")), 4);
    }

    #[test]
    fn streaming_executor_starts_safe_tools_together_and_honors_exclusive_barrier() {
        let mut state = StreamingToolExecutorState::with_max_concurrency(usize::MAX);

        assert_eq!(
            state.add_tool("read-1", "read_file", ToolConcurrency::ConcurrentSafe),
            vec!["read-1".to_owned()]
        );
        assert_eq!(
            state.add_tool("read-2", "git_diff", ToolConcurrency::ConcurrentSafe),
            vec!["read-2".to_owned()]
        );
        assert!(state
            .add_tool("bash-1", "bash", ToolConcurrency::Exclusive)
            .is_empty());
        assert!(state
            .add_tool("read-3", "repo_search", ToolConcurrency::ConcurrentSafe)
            .is_empty());

        assert!(state
            .complete_tool("read-1", "read one", false, false)
            .is_empty());
        assert_eq!(
            state.complete_tool("read-2", "read two", false, false),
            vec!["bash-1".to_owned()]
        );
        assert!(state
            .complete_tool("bash-1", "bash done", false, false)
            .contains(&"read-3".to_owned()));
    }

    #[test]
    fn streaming_executor_respects_configured_safe_tool_concurrency_cap() {
        let mut state = StreamingToolExecutorState::with_max_concurrency(2);

        assert_eq!(
            state.add_tool("read-1", "read_file", ToolConcurrency::ConcurrentSafe),
            vec!["read-1".to_owned()]
        );
        assert_eq!(
            state.add_tool("read-2", "git_diff", ToolConcurrency::ConcurrentSafe),
            vec!["read-2".to_owned()]
        );
        assert!(state
            .add_tool("read-3", "repo_search", ToolConcurrency::ConcurrentSafe)
            .is_empty());

        assert_eq!(
            state.complete_tool("read-1", "read one", false, false),
            vec!["read-3".to_owned()]
        );
    }

    #[test]
    fn streaming_executor_shell_error_cancels_queued_and_running_siblings() {
        let mut state = StreamingToolExecutorState::with_max_concurrency(usize::MAX);
        state.add_tool("bash-1", "bash", ToolConcurrency::ConcurrentSafe);
        state.add_tool("read-1", "read_file", ToolConcurrency::ConcurrentSafe);
        state.add_tool("read-2", "repo_search", ToolConcurrency::ConcurrentSafe);

        state.complete_tool("bash-1", "exit 1", true, true);
        let updates = state.yield_available();

        assert_eq!(updates.len(), 3);
        assert_eq!(updates[0].tool_id, "bash-1");
        assert_eq!(updates[0].kind, StreamingToolUpdateKind::SyntheticError);
        assert_eq!(updates[1].tool_id, "read-1");
        assert_eq!(updates[1].kind, StreamingToolUpdateKind::SyntheticError);
        assert!(updates[1].content.contains("Cancelled"));
        assert_eq!(updates[2].tool_id, "read-2");
    }

    #[test]
    fn streaming_executor_can_synthesize_missing_results_before_followup() {
        let mut state = StreamingToolExecutorState::with_max_concurrency(usize::MAX);
        state.add_tool("tool-1", "repo_search", ToolConcurrency::ConcurrentSafe);
        state.add_tool("tool-2", "read_file", ToolConcurrency::ConcurrentSafe);

        state.cancel_unfinished();

        let updates = state.yield_available();
        assert_eq!(updates.len(), 2);
        assert!(updates
            .iter()
            .all(|update| update.kind == StreamingToolUpdateKind::SyntheticError));
        assert!(updates.iter().all(|update| update
            .content
            .contains("before this tool produced a result")));
    }
}
