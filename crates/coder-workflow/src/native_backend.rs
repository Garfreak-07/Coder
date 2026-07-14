use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use coder_config::{HarnessSpec, PermissionPolicy, VerificationPolicy};
use coder_core::{FinalReport, RunId};
use coder_harness::{
    HarnessBackend, HarnessError, HarnessRunEvent, HarnessRunRequest, HarnessRunResult,
};
use coder_store::{RepoEvidenceKind, RepoEvidenceRef, RunStore};
use coder_tools::{
    apply_patch_file, builtin_tool, canonical_builtin_tool_name, find_files, git_diff, git_status,
    preview_command, preview_patch_file, read_file, read_file_range, run_command, search_text,
    CommandRunRequest, PatchApplyRequest as ToolPatchApplyRequest, RepoToolConfig,
};
use serde_json::{json, Value};

use crate::tool_execution::{
    max_tool_use_concurrency_from_env, partition_tool_steps, StreamingToolExecutorState,
    StreamingToolUpdate, ToolConcurrency, ToolExecutionBatch, ToolExecutionStep,
};
use crate::{subagent_context, SubagentInvocationKind, SubagentRunInput, SubagentRuntime};

static NATIVE_REPO_EVIDENCE_WRITE_LOCK: Mutex<()> = Mutex::new(());
const NATIVE_SUBAGENT_MAX_DEPTH: u32 = 1;

fn concise_join(items: &[String], max_chars: usize) -> String {
    let joined = items
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if joined.chars().count() <= max_chars {
        joined
    } else {
        joined.chars().take(max_chars).collect()
    }
}

#[derive(Debug, Clone)]
pub struct DeterministicNativeBackend {
    store: RunStore,
}

impl DeterministicNativeBackend {
    pub fn new(store: RunStore) -> Self {
        Self { store }
    }
}

#[derive(Debug)]
struct NativeToolRunState {
    events: Vec<HarnessRunEvent>,
    evidence_refs: Vec<coder_core::EvidenceRef>,
    patch_refs: Vec<String>,
    changed_files: BTreeSet<String>,
    checks: Vec<String>,
    blockers: Vec<String>,
    failures: Vec<String>,
    completed_tools: usize,
}

impl NativeToolRunState {
    fn new(started_event: HarnessRunEvent) -> Self {
        Self {
            events: vec![started_event],
            evidence_refs: Vec::new(),
            patch_refs: Vec::new(),
            changed_files: BTreeSet::new(),
            checks: Vec::new(),
            blockers: Vec::new(),
            failures: Vec::new(),
            completed_tools: 0,
        }
    }

    fn empty() -> Self {
        Self {
            events: Vec::new(),
            evidence_refs: Vec::new(),
            patch_refs: Vec::new(),
            changed_files: BTreeSet::new(),
            checks: Vec::new(),
            blockers: Vec::new(),
            failures: Vec::new(),
            completed_tools: 0,
        }
    }

    fn merge(&mut self, other: Self) {
        self.events.extend(other.events);
        self.evidence_refs.extend(other.evidence_refs);
        self.patch_refs.extend(other.patch_refs);
        self.changed_files.extend(other.changed_files);
        self.checks.extend(other.checks);
        self.blockers.extend(other.blockers);
        self.failures.extend(other.failures);
        self.completed_tools += other.completed_tools;
    }

    fn complete(&mut self, check: impl Into<String>) {
        self.checks.push(check.into());
        self.completed_tools += 1;
    }

    fn fail(&mut self, tool: &str, error: impl ToString) {
        let error = error.to_string();
        self.failures.push(format!("{tool} failed: {error}"));
        self.events.push(native_tool_failure_event(tool, error));
    }

    fn add_evidence_ref(&mut self, reference: &RepoEvidenceRef) {
        self.evidence_refs.push(repo_evidence_ref(reference));
    }
}

struct NativeToolExecutionContext<'a> {
    store: &'a RunStore,
    request: &'a HarnessRunRequest,
    repo_root: &'a str,
    candidate_file: Option<PathBuf>,
    patch_file: Option<PathBuf>,
    patch_file_resolved: bool,
}

impl<'a> NativeToolExecutionContext<'a> {
    fn new(store: &'a RunStore, request: &'a HarnessRunRequest, repo_root: &'a str) -> Self {
        Self {
            store,
            request,
            repo_root,
            candidate_file: native_candidate_file(repo_root, &request.task),
            patch_file: None,
            patch_file_resolved: false,
        }
    }

    fn patch_file(&mut self) -> Option<&PathBuf> {
        if !self.patch_file_resolved {
            self.patch_file = native_patch_file(self.repo_root, &self.request.task);
            self.patch_file_resolved = true;
        }
        self.patch_file.as_ref()
    }
}

#[async_trait]
impl HarnessBackend for DeterministicNativeBackend {
    async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        let repo_root = if request.repo_root.trim().is_empty() {
            ".".to_owned()
        } else {
            request.repo_root.clone()
        };
        let tools = native_selected_tools(&request);
        let execution_steps = native_tool_execution_steps(&tools);
        let execution_batches = native_tool_execution_batches_from_steps(&execution_steps);
        let max_tool_use_concurrency = max_tool_use_concurrency_from_env();
        let started_event = HarnessRunEvent::new(
            "backend.native_rust.started",
            json!({
                "backend": "native-rust",
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "tools": tools.iter().cloned().collect::<Vec<_>>(),
                "max_tool_use_concurrency": max_tool_use_concurrency,
                "tool_execution_mode": "streaming_state_machine",
                "execution_batches": native_tool_execution_batches_value(&execution_batches)
            }),
        );
        let mut state = NativeToolRunState::new(started_event);
        let mut execution_context =
            NativeToolExecutionContext::new(&self.store, &request, &repo_root);

        execute_native_tool_streaming_plan(
            &execution_steps,
            &mut execution_context,
            &mut state,
            max_tool_use_concurrency,
        )
        .await?;

        if native_executor_required_side_effect_missing(&request, &tools, &state) {
            state.blockers.push(
                "executor task requested repository changes, but native tool execution produced no changed files or patch evidence"
                    .to_owned(),
            );
        }
        let status = native_terminal_status(&request, &state);
        let completed_tools = state.completed_tools;
        let blocker_summary = state.blockers.join("; ");
        let failure_summary = state.failures.join("; ");
        let mut report = match status {
            "blocked" => FinalReport::blocked(
                "Native Rust backend stopped before side effects.",
                blocker_summary,
            ),
            "failed" => FinalReport::failed(
                "Native Rust backend could not complete requested tool work.",
                failure_summary,
            ),
            _ => FinalReport::completed(format!(
                "Native Rust backend completed {} tool operation(s).",
                completed_tools
            )),
        };
        let failures = state.failures;
        report.checks = state.checks;
        report.evidence_refs = state.evidence_refs;
        report.patch_refs = state.patch_refs;
        report.changed_files = state.changed_files.into_iter().collect();
        if !failures.is_empty() && status != "failed" {
            report.next_steps = failures;
        }
        let mut events = state.events;
        let react_events = native_react_lifecycle_events(&request, &events, status);
        if !react_events.is_empty() {
            let mut ordered_events = Vec::with_capacity(events.len() + react_events.len());
            if !events.is_empty() {
                ordered_events.push(events.remove(0));
            }
            ordered_events.extend(react_events);
            ordered_events.extend(events);
            events = ordered_events;
        }
        events.push(HarnessRunEvent::new(
            format!("backend.native_rust.{status}"),
            json!({
                "backend": "native-rust",
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "status": status,
                "completed_tools": completed_tools
            }),
        ));
        Ok(HarnessRunResult {
            status: status.to_owned(),
            report: Some(report),
            events,
        })
    }
}

#[derive(Debug)]
struct NativeToolOutcome {
    tool_id: String,
    tool: String,
    state: NativeToolRunState,
}

async fn execute_native_tool_streaming_plan(
    steps: &[ToolExecutionStep],
    context: &mut NativeToolExecutionContext<'_>,
    state: &mut NativeToolRunState,
    max_tool_use_concurrency: usize,
) -> Result<(), HarnessError> {
    let mut executor = StreamingToolExecutorState::with_max_concurrency(max_tool_use_concurrency);
    let mut steps_by_id = BTreeMap::new();
    let mut ready_ids = Vec::new();

    for (index, step) in steps.iter().enumerate() {
        let tool_id = format!("native-{index}-{}", step.tool);
        steps_by_id.insert(tool_id.clone(), step.clone());
        ready_ids.extend(executor.add_tool(tool_id, step.tool.clone(), step.concurrency));
    }

    while !ready_ids.is_empty() {
        for tool_id in &ready_ids {
            let step = steps_by_id.get(tool_id).ok_or_else(|| {
                HarnessError::Failed(format!("native tool step disappeared: {tool_id}"))
            })?;
            state
                .events
                .push(native_streaming_tool_started_event(tool_id, step));
        }

        let outcomes = execute_native_started_tools(
            &ready_ids,
            &steps_by_id,
            context,
            max_tool_use_concurrency,
        )
        .await?;
        let mut next_ready_ids = Vec::new();
        for outcome in outcomes {
            let is_error = !outcome.state.failures.is_empty();
            let summary = native_streaming_tool_summary(&outcome.tool, &outcome.state);
            state.merge(outcome.state);
            next_ready_ids.extend(executor.complete_tool(
                &outcome.tool_id,
                summary,
                is_error,
                false,
            ));
            for update in executor.yield_available() {
                state
                    .events
                    .push(native_streaming_tool_update_event(update));
            }
        }
        ready_ids = next_ready_ids;
    }

    for update in executor.yield_available() {
        state
            .events
            .push(native_streaming_tool_update_event(update));
    }
    Ok(())
}

async fn execute_native_started_tools(
    ready_ids: &[String],
    steps_by_id: &BTreeMap<String, ToolExecutionStep>,
    context: &mut NativeToolExecutionContext<'_>,
    max_tool_use_concurrency: usize,
) -> Result<Vec<NativeToolOutcome>, HarnessError> {
    let all_concurrent_safe = ready_ids.iter().all(|tool_id| {
        steps_by_id
            .get(tool_id)
            .is_some_and(|step| step.concurrency == ToolConcurrency::ConcurrentSafe)
    });
    if all_concurrent_safe && ready_ids.len() > 1 && max_tool_use_concurrency > 1 {
        return execute_native_started_tools_concurrently(ready_ids, steps_by_id, context);
    }

    let mut outcomes = Vec::with_capacity(ready_ids.len());
    for tool_id in ready_ids {
        let step = steps_by_id.get(tool_id).ok_or_else(|| {
            HarnessError::Failed(format!("native tool step disappeared: {tool_id}"))
        })?;
        let mut local_state = NativeToolRunState::empty();
        execute_native_tool_step(&step.tool, context, &mut local_state).await?;
        outcomes.push(NativeToolOutcome {
            tool_id: tool_id.clone(),
            tool: step.tool.clone(),
            state: local_state,
        });
    }
    Ok(outcomes)
}

fn execute_native_started_tools_concurrently(
    ready_ids: &[String],
    steps_by_id: &BTreeMap<String, ToolExecutionStep>,
    context: &NativeToolExecutionContext<'_>,
) -> Result<Vec<NativeToolOutcome>, HarnessError> {
    let outcomes = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(ready_ids.len());
        for tool_id in ready_ids {
            let step = steps_by_id.get(tool_id).ok_or_else(|| {
                HarnessError::Failed(format!("native tool step disappeared: {tool_id}"))
            })?;
            let tool_id = tool_id.clone();
            let tool = step.tool.clone();
            let store = context.store;
            let request = context.request;
            let repo_root = context.repo_root;
            handles.push(scope.spawn(move || {
                let mut local_context = NativeToolExecutionContext::new(store, request, repo_root);
                let mut local_state = NativeToolRunState::empty();
                execute_native_tool_step_sync(&tool, &mut local_context, &mut local_state)?;
                Ok(NativeToolOutcome {
                    tool_id,
                    tool,
                    state: local_state,
                })
            }));
        }

        let mut outcomes = Vec::with_capacity(handles.len());
        for handle in handles {
            match handle.join() {
                Ok(result) => outcomes.push(result),
                Err(_) => outcomes.push(Err(HarnessError::Failed(
                    "native concurrent tool panicked".to_owned(),
                ))),
            }
        }
        Ok::<_, HarnessError>(outcomes)
    })?;

    outcomes.into_iter().collect()
}

fn native_streaming_tool_started_event(tool_id: &str, step: &ToolExecutionStep) -> HarnessRunEvent {
    HarnessRunEvent::new(
        "tool.execution.started",
        json!({
            "tool_id": tool_id,
            "tool": step.tool,
            "concurrency": step.concurrency.as_str(),
            "executor": "streaming_state_machine"
        }),
    )
}

fn native_streaming_tool_update_event(update: StreamingToolUpdate) -> HarnessRunEvent {
    let kind = update.kind.as_str();
    HarnessRunEvent::new(
        "tool.execution.update",
        json!({
            "tool_id": update.tool_id,
            "tool": update.tool_name,
            "kind": kind,
            "summary": truncate_public(&update.content, 240),
            "executor": "streaming_state_machine"
        }),
    )
}

fn native_streaming_tool_summary(tool: &str, state: &NativeToolRunState) -> String {
    if !state.failures.is_empty() {
        return format!("{tool}: {}", concise_join(&state.failures, 240));
    }
    if !state.blockers.is_empty() {
        return format!("{tool}: {}", concise_join(&state.blockers, 240));
    }
    if let Some(check) = state.checks.last() {
        return check.clone();
    }
    format!("{tool}: no-op")
}

fn native_permission_decision_payload(request: &HarnessRunRequest, permission: &str) -> Value {
    let policy_contract = request
        .backend_context
        .pointer("/coder/permissions/contract")
        .cloned()
        .unwrap_or(Value::Null);
    let decision = request
        .backend_context
        .pointer("/coder/permissions/decisions")
        .and_then(Value::as_array)
        .and_then(|decisions| {
            decisions
                .iter()
                .find(|decision| decision["permission"].as_str() == Some(permission))
        })
        .cloned();
    let found = decision.is_some();
    json!({
        "contract": "coder.tool_permission_decision.v1",
        "policy_contract": policy_contract,
        "permission": permission,
        "status": if found { "found" } else { "missing" },
        "decision": decision.unwrap_or(Value::Null)
    })
}

async fn execute_native_tool_step(
    tool: &str,
    context: &mut NativeToolExecutionContext<'_>,
    state: &mut NativeToolRunState,
) -> Result<(), HarnessError> {
    if tool == "agent_subagent" {
        return execute_native_subagent_tool(context, state).await;
    }
    execute_native_tool_step_sync(tool, context, state)
}

async fn execute_native_subagent_tool(
    context: &mut NativeToolExecutionContext<'_>,
    state: &mut NativeToolRunState,
) -> Result<(), HarnessError> {
    let parent_depth = native_subagent_parent_depth(context.request);
    if parent_depth >= NATIVE_SUBAGENT_MAX_DEPTH {
        state.events.push(native_tool_skipped_event(
            "agent_subagent",
            "subagent depth limit reached",
        ));
        return Ok(());
    }

    let harness = native_subagent_harness(context.request);
    let backend: Arc<dyn HarnessBackend> =
        Arc::new(DeterministicNativeBackend::new(context.store.clone()));
    let runtime = SubagentRuntime::new(context.store.clone());
    let output = runtime
        .run(SubagentRunInput {
            backend,
            run_id: &context.request.run_id,
            workflow_id: &context.request.workflow_id,
            node_id: &context.request.node_id,
            parent_agent_id: &context.request.agent_id,
            parent_harness_id: &context.request.harness_id,
            harness: &harness,
            repo_root: context.repo_root,
            task: &native_subagent_task(&context.request.task),
            backend_context: &context.request.backend_context,
            agent_id: None,
            subagent_name: Some("native-worker"),
            is_built_in: false,
            invoking_request_id: Some("native-agent-subagent"),
            invocation_kind: SubagentInvocationKind::Spawn,
            parent_query_depth: parent_depth,
            parent_sequence: None,
        })
        .await?;

    let status = output.result.status.clone();
    let agent_id = output.agent_id.clone();
    let metadata_ref = output.metadata_ref.clone();
    let transcript_ref = output.transcript_ref.clone();
    state.events.push(
        HarnessRunEvent::new(
            "native.tool.completed",
            json!({
                "tool": "agent_subagent",
                "status": status,
                "agent_id": agent_id,
                "metadata_ref": metadata_ref,
                "transcript_ref": transcript_ref,
                "inherited_tools": harness.tools,
                "parent_query_depth": parent_depth,
                "max_depth": NATIVE_SUBAGENT_MAX_DEPTH
            }),
        )
        .with_ref("subagent_metadata", output.metadata_ref)
        .with_ref("subagent_transcript", output.transcript_ref),
    );

    match status.as_str() {
        "completed" | "ready" | "finish" => {
            state.complete(format!("agent_subagent: child {status}"));
        }
        "blocked" => {
            state
                .blockers
                .push("agent_subagent child returned blocked".to_owned());
        }
        "failed" => {
            state
                .failures
                .push("agent_subagent child returned failed".to_owned());
        }
        other => {
            state.complete(format!("agent_subagent: child returned {other}"));
        }
    }
    Ok(())
}

fn native_subagent_harness(request: &HarnessRunRequest) -> HarnessSpec {
    let harness_context = request.backend_context.pointer("/coder/harness");
    let parent_tools = harness_context
        .and_then(|harness| harness.get("selected_tools"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let tools = subagent_context::subagent_inheritable_tools(&parent_tools);
    let permissions = harness_context
        .and_then(|harness| harness.get("permissions"))
        .cloned()
        .and_then(|value| serde_json::from_value::<PermissionPolicy>(value).ok())
        .unwrap_or_default();
    let verification = harness_context
        .and_then(|harness| harness.get("verification"))
        .cloned()
        .and_then(|value| serde_json::from_value::<VerificationPolicy>(value).ok())
        .unwrap_or_default();
    HarnessSpec {
        backend: "native-rust".to_owned(),
        tools,
        permissions,
        verification,
    }
}

fn native_subagent_parent_depth(request: &HarnessRunRequest) -> u32 {
    [
        "/coder_subagent/context/query_tracking/depth",
        "/coder/subagent/context/query_tracking/depth",
    ]
    .iter()
    .find_map(|pointer| request.backend_context.pointer(pointer))
    .and_then(Value::as_u64)
    .and_then(|value| u32::try_from(value).ok())
    .unwrap_or(0)
}

fn native_subagent_task(task: &str) -> String {
    format!("Subagent helper task for native executor:\n{task}")
}

fn execute_native_tool_step_sync(
    tool: &str,
    context: &mut NativeToolExecutionContext<'_>,
    state: &mut NativeToolRunState,
) -> Result<(), HarnessError> {
    match tool {
        "repo_find_files" => match find_files(context.repo_root, None, &[], 50) {
            Ok(files) => {
                let file_count = files.len();
                let reference = write_native_repo_evidence(
                    context.store,
                    &context.request.run_id,
                    RepoEvidenceKind::RepoFileList,
                    context.repo_root,
                    format!("Native Rust backend found {file_count} repo file(s)."),
                    json!({
                        "evidence_kind": "repo_evidence",
                        "operation": "find_files",
                        "files": files
                    }),
                )?;
                state.add_evidence_ref(&reference);
                state.events.push(native_tool_event(
                    "repo_find_files",
                    "completed",
                    json!({ "file_count": file_count }),
                    Some(&reference),
                ));
                state.complete("repo_find_files: completed");
            }
            Err(error) => state.fail("repo_find_files", error),
        },
        "repo_search_text" => {
            let query = native_search_query(&context.request.task);
            match search_text(context.repo_root, &query, &RepoToolConfig::default()) {
                Ok(matches) => {
                    let match_count = matches.len();
                    let reference = write_native_repo_evidence(
                        context.store,
                        &context.request.run_id,
                        RepoEvidenceKind::RepoTextSearch,
                        context.repo_root,
                        format!("Native Rust backend found {match_count} text match(es)."),
                        json!({
                            "evidence_kind": "repo_evidence",
                            "operation": "search_text",
                            "query": query,
                            "matches": matches
                        }),
                    )?;
                    state.add_evidence_ref(&reference);
                    state.events.push(native_tool_event(
                        "repo_search_text",
                        "completed",
                        json!({ "match_count": match_count }),
                        Some(&reference),
                    ));
                    state.complete("repo_search_text: completed");
                }
                Err(error) => state.fail("repo_search_text", error),
            }
        }
        "repo_read_file" => {
            let Some(path) = context.candidate_file.clone() else {
                state.events.push(native_tool_skipped_event(
                    "repo_read_file",
                    "no safe readable candidate file found",
                ));
                return Ok(());
            };
            match read_file(context.repo_root, &path, &RepoToolConfig::default()) {
                Ok(file) => {
                    let file_path = file.path.clone();
                    let reference = write_native_repo_evidence(
                        context.store,
                        &context.request.run_id,
                        RepoEvidenceKind::RepoRead,
                        context.repo_root,
                        format!("Native Rust backend read {file_path}."),
                        json!({
                            "evidence_kind": "repo_evidence",
                            "operation": "read_file",
                            "file": file
                        }),
                    )?;
                    state.add_evidence_ref(&reference);
                    state.events.push(native_tool_event(
                        "repo_read_file",
                        "completed",
                        json!({ "path": file_path }),
                        Some(&reference),
                    ));
                    state.complete(format!("repo_read_file: {file_path}"));
                }
                Err(error) => state.fail("repo_read_file", error),
            }
        }
        "repo_read_file_range" => {
            let Some(path) = context.candidate_file.clone() else {
                return Ok(());
            };
            match read_file_range(context.repo_root, &path, 1, 80, 16_000) {
                Ok(snippet) => {
                    let snippet_path = snippet.path.clone();
                    let reference = write_native_repo_evidence(
                        context.store,
                        &context.request.run_id,
                        RepoEvidenceKind::RepoRead,
                        context.repo_root,
                        format!(
                            "Native Rust backend read {snippet_path}:1-{}.",
                            snippet.end_line
                        ),
                        json!({
                            "evidence_kind": "repo_evidence",
                            "operation": "read_file_range",
                            "snippet": snippet
                        }),
                    )?;
                    state.add_evidence_ref(&reference);
                    state.events.push(native_tool_event(
                        "repo_read_file_range",
                        "completed",
                        json!({ "path": snippet_path }),
                        Some(&reference),
                    ));
                    state.complete(format!("repo_read_file_range: {snippet_path}"));
                }
                Err(error) => state.fail("repo_read_file_range", error),
            }
        }
        "git_status" => match git_status(context.repo_root) {
            Ok(status) => {
                let reference = write_native_repo_evidence(
                    context.store,
                    &context.request.run_id,
                    RepoEvidenceKind::RepoDiff,
                    context.repo_root,
                    "Native Rust backend captured git status.",
                    json!({
                        "evidence_kind": "repo_evidence",
                        "operation": "git_status",
                        "status": status
                    }),
                )?;
                state.add_evidence_ref(&reference);
                state.events.push(native_tool_event(
                    "git_status",
                    "completed",
                    json!({}),
                    Some(&reference),
                ));
                state.complete("git_status: completed");
            }
            Err(error) => {
                state
                    .events
                    .push(native_tool_failure_event("git_status", error.to_string()));
            }
        },
        "git_diff" => {
            match git_diff(context.repo_root, coder_tools::DEFAULT_MAX_GIT_OUTPUT_BYTES) {
                Ok(diff) => {
                    let reference = write_native_repo_evidence(
                        context.store,
                        &context.request.run_id,
                        RepoEvidenceKind::RepoDiff,
                        context.repo_root,
                        "Native Rust backend captured git diff.",
                        json!({
                            "evidence_kind": "repo_evidence",
                            "operation": "git_diff",
                            "diff": diff
                        }),
                    )?;
                    state.add_evidence_ref(&reference);
                    state.events.push(native_tool_event(
                        "git_diff",
                        "completed",
                        json!({}),
                        Some(&reference),
                    ));
                    state.complete("git_diff: completed");
                }
                Err(error) => {
                    state
                        .events
                        .push(native_tool_failure_event("git_diff", error.to_string()));
                }
            }
        }
        "command_preview" => {
            let Some(argv) = native_command_args(&context.request.task) else {
                return Ok(());
            };
            match preview_command(context.repo_root, ".", argv, "model", false) {
                Ok(preview) => {
                    state.events.push(HarnessRunEvent::new(
                        "native.tool.completed",
                        json!({
                            "tool": "command_preview",
                            "status": "completed",
                            "command": preview.command,
                            "requires_approval": preview.requires_approval,
                            "approval_key": preview.approval_key,
                            "policy": preview.policy
                        }),
                    ));
                    state.complete("command_preview: completed");
                }
                Err(error) => state.fail("command_preview", error),
            }
        }
        "command_run" => {
            let Some(argv) = native_command_args(&context.request.task) else {
                return Ok(());
            };
            match run_command(
                context.repo_root,
                CommandRunRequest {
                    argv,
                    source: "model".to_owned(),
                    approved: false,
                    ..CommandRunRequest::default()
                },
            ) {
                Ok(output) => {
                    let blocked = output.blocked;
                    let requires_approval = output.requires_approval;
                    let reference = write_native_repo_evidence(
                        context.store,
                        &context.request.run_id,
                        RepoEvidenceKind::RepoTest,
                        context.repo_root,
                        format!("Native Rust command {}: {}.", output.status, output.command),
                        json!({
                            "evidence_kind": "command_evidence",
                            "operation": "command_run",
                            "result": output
                        }),
                    )?;
                    state.add_evidence_ref(&reference);
                    let event_kind = if blocked && requires_approval {
                        "approval.requested"
                    } else {
                        "native.tool.completed"
                    };
                    let permission_decision =
                        native_permission_decision_payload(context.request, "run_commands");
                    state.events.push(
                        HarnessRunEvent::new(
                            event_kind,
                            json!({
                                "tool": "command_run",
                                "approval_type": if blocked && requires_approval { "command" } else { "" },
                                "status": if blocked { "blocked" } else { "completed" },
                                "requires_approval": requires_approval,
                                "required_permission": "run_commands",
                                "permission_decision": permission_decision,
                                "evidence_ref": reference.ref_id
                            }),
                        )
                        .with_ref(
                            "command_evidence",
                            format!("repo-evidence://{}", reference.ref_id),
                        ),
                    );
                    if blocked && requires_approval {
                        state
                            .blockers
                            .push("command_run requires approval".to_owned());
                    } else {
                        state.complete("command_run: completed");
                    }
                }
                Err(error) => state.fail("command_run", error),
            }
        }
        "patch_preview" => {
            let Some(path) = context.patch_file().cloned() else {
                state.events.push(native_tool_skipped_event(
                    "patch_preview",
                    "no patch file found",
                ));
                return Ok(());
            };
            match preview_patch_file(
                context.repo_root,
                &path,
                coder_tools::DEFAULT_MAX_PATCH_BYTES,
            ) {
                Ok(preview) => {
                    let touched = preview
                        .files
                        .iter()
                        .filter_map(|file| file.new_path.clone().or_else(|| file.old_path.clone()))
                        .collect::<Vec<_>>();
                    for path in &touched {
                        state.changed_files.insert(path.clone());
                    }
                    let reference = write_native_repo_evidence(
                        context.store,
                        &context.request.run_id,
                        RepoEvidenceKind::RepoDiff,
                        context.repo_root,
                        format!(
                            "Native Rust backend previewed patch touching {} file(s).",
                            preview.file_count
                        ),
                        json!({
                            "evidence_kind": "repo_evidence",
                            "operation": "patch_preview",
                            "preview": preview
                        }),
                    )?;
                    state
                        .patch_refs
                        .push(format!("repo-evidence://{}", reference.ref_id));
                    state.add_evidence_ref(&reference);
                    state.events.push(native_tool_event(
                        "patch_preview",
                        "completed",
                        json!({ "files": touched }),
                        Some(&reference),
                    ));
                    state.complete("patch_preview: completed");
                }
                Err(error) => state.fail("patch_preview", error),
            }
        }
        "patch_apply" => {
            let Some(path) = context.patch_file().cloned() else {
                return Ok(());
            };
            match apply_patch_file(
                context.repo_root,
                ToolPatchApplyRequest {
                    patch_file: path,
                    max_patch_bytes: coder_tools::DEFAULT_MAX_PATCH_BYTES,
                    source: "model".to_owned(),
                    approved: false,
                },
            ) {
                Ok(result) => {
                    let blocked = result.requires_approval;
                    let reference = write_native_repo_evidence(
                        context.store,
                        &context.request.run_id,
                        RepoEvidenceKind::RepoDiff,
                        context.repo_root,
                        format!(
                            "Native Rust patch apply {}: {} file(s).",
                            result.status, result.preview.file_count
                        ),
                        json!({
                            "evidence_kind": "patch_apply",
                            "operation": "patch_apply",
                            "result": result
                        }),
                    )?;
                    state
                        .patch_refs
                        .push(format!("repo-evidence://{}", reference.ref_id));
                    state.add_evidence_ref(&reference);
                    let permission_decision =
                        native_permission_decision_payload(context.request, "write_files");
                    state.events.push(
                        HarnessRunEvent::new(
                            if blocked {
                                "approval.requested"
                            } else {
                                "native.tool.completed"
                            },
                            json!({
                                "tool": "patch_apply",
                                "approval_type": if blocked { "patch_apply" } else { "" },
                                "status": if blocked { "blocked" } else { "completed" },
                                "requires_approval": blocked,
                                "required_permission": "write_files",
                                "permission_decision": permission_decision,
                                "evidence_ref": reference.ref_id
                            }),
                        )
                        .with_ref(
                            "patch_evidence",
                            format!("repo-evidence://{}", reference.ref_id),
                        ),
                    );
                    if blocked {
                        state
                            .blockers
                            .push("patch_apply requires approval".to_owned());
                    } else {
                        state.complete("patch_apply: completed");
                    }
                }
                Err(error) => state.fail("patch_apply", error),
            }
        }
        _ => {}
    }
    Ok(())
}

fn native_react_lifecycle_events(
    request: &HarnessRunRequest,
    source_events: &[HarnessRunEvent],
    terminal_status: &str,
) -> Vec<HarnessRunEvent> {
    let action_events = source_events
        .iter()
        .filter(|event| native_event_has_tool_action(event))
        .collect::<Vec<_>>();
    let mut events = Vec::new();
    let mut previous_observation: Option<String> = None;
    for (index, event) in action_events.iter().enumerate() {
        let step = index + 1;
        let tool_name = event_payload_string(&event.payload, "tool")
            .unwrap_or_else(|| "native_tool".to_owned());
        let status = native_public_tool_status(event);
        let observation = native_observation_summary(event, &tool_name, &status);
        let next_tool = action_events
            .get(index + 1)
            .and_then(|next| event_payload_string(&next.payload, "tool"));
        let reasoning_summary = if let Some(previous) = &previous_observation {
            format!(
                "Use the previous observation to choose the next harness action: {}",
                truncate_public(previous, 180)
            )
        } else {
            format!(
                "Select the first harness action for executor task: {}",
                truncate_public(&request.task, 180)
            )
        };

        events.push(HarnessRunEvent::new(
            "executor.reasoning_summary",
            json!({
                "run_id": request.run_id.as_str(),
                "workflow_id": request.workflow_id,
                "backend": "native-rust",
                "step": step,
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "summary": reasoning_summary,
                "previous_observation": previous_observation
            }),
        ));
        events.push(HarnessRunEvent::new(
            "executor.action_selected",
            json!({
                "run_id": request.run_id.as_str(),
                "workflow_id": request.workflow_id,
                "backend": "native-rust",
                "step": step,
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "tool_name": tool_name,
                "action": "run_harness_tool",
                "permission_boundary": "harness",
                "allowed_by_harness": true,
                "status": "selected"
            }),
        ));
        if event.kind != "native.tool.skipped" {
            events.push(HarnessRunEvent::new(
                "tool.started",
                json!({
                    "run_id": request.run_id.as_str(),
                    "workflow_id": request.workflow_id,
                    "backend": "native-rust",
                    "step": step,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "tool_name": tool_name,
                    "status": "started"
                }),
            ));
        }
        events.push(copy_event_refs(
            HarnessRunEvent::new(
                "tool.completed",
                json!({
                    "run_id": request.run_id.as_str(),
                    "workflow_id": request.workflow_id,
                    "backend": "native-rust",
                    "step": step,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "tool_name": tool_name,
                    "status": status,
                    "summary": observation,
                    "evidence_ref": first_event_ref_uri(event)
                }),
            ),
            event,
        ));
        events.push(copy_event_refs(
            HarnessRunEvent::new(
                "observation.recorded",
                json!({
                    "run_id": request.run_id.as_str(),
                    "workflow_id": request.workflow_id,
                    "backend": "native-rust",
                    "step": step,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "tool_name": tool_name,
                    "summary": observation,
                    "evidence_ref": first_event_ref_uri(event)
                }),
            ),
            event,
        ));
        events.push(HarnessRunEvent::new(
            "executor.next_step",
            json!({
                "run_id": request.run_id.as_str(),
                "workflow_id": request.workflow_id,
                "backend": "native-rust",
                "step": step,
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "based_on_observation": observation,
                "next_action": if next_tool.is_some() { "continue" } else { "finalize" },
                "next_tool": next_tool
            }),
        ));
        previous_observation = Some(observation);
    }
    if !action_events.is_empty() {
        events.push(HarnessRunEvent::new(
            executor_terminal_event_kind(terminal_status),
            json!({
                "run_id": request.run_id.as_str(),
                "workflow_id": request.workflow_id,
                "backend": "native-rust",
                "step": action_events.len(),
                "node_id": request.node_id,
                "agent_id": request.agent_id,
                "harness_id": request.harness_id,
                "status": terminal_status,
                "summary": format!(
                    "Executor {} after {} harness action(s).",
                    terminal_status,
                    action_events.len()
                )
            }),
        ));
    }
    events
}

pub(crate) fn native_selected_tools(request: &HarnessRunRequest) -> BTreeSet<String> {
    request
        .backend_context
        .pointer("/coder/harness/selected_tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn native_tool_execution_steps(tools: &BTreeSet<String>) -> Vec<ToolExecutionStep> {
    native_tool_execution_order()
        .iter()
        .filter(|(tool, _)| native_tool_enabled(tools, tool))
        .map(|(tool, concurrency)| ToolExecutionStep::new(*tool, *concurrency))
        .collect()
}

fn native_tool_execution_batches_from_steps(
    steps: &[ToolExecutionStep],
) -> Vec<ToolExecutionBatch> {
    partition_tool_steps(steps.iter().cloned())
}

fn native_tool_execution_batches_value(batches: &[ToolExecutionBatch]) -> Value {
    Value::Array(
        batches
            .iter()
            .map(|batch| {
                json!({
                    "concurrency": batch.concurrency.as_str(),
                    "tools": batch.tools
                })
            })
            .collect(),
    )
}

fn native_tool_execution_order() -> &'static [(&'static str, ToolConcurrency)] {
    &[
        ("repo_find_files", ToolConcurrency::ConcurrentSafe),
        ("repo_search_text", ToolConcurrency::ConcurrentSafe),
        ("repo_read_file", ToolConcurrency::ConcurrentSafe),
        ("repo_read_file_range", ToolConcurrency::ConcurrentSafe),
        ("git_status", ToolConcurrency::ConcurrentSafe),
        ("git_diff", ToolConcurrency::ConcurrentSafe),
        ("agent_subagent", ToolConcurrency::Exclusive),
        ("command_preview", ToolConcurrency::ConcurrentSafe),
        ("command_run", ToolConcurrency::Exclusive),
        ("patch_preview", ToolConcurrency::ConcurrentSafe),
        ("patch_apply", ToolConcurrency::Exclusive),
    ]
}

fn native_terminal_status(
    _request: &HarnessRunRequest,
    state: &NativeToolRunState,
) -> &'static str {
    if !state.blockers.is_empty() {
        "blocked"
    } else if state.completed_tools == 0 && !state.failures.is_empty() {
        "failed"
    } else {
        "completed"
    }
}

fn native_executor_required_side_effect_missing(
    request: &HarnessRunRequest,
    tools: &BTreeSet<String>,
    state: &NativeToolRunState,
) -> bool {
    native_task_requires_repository_changes(request)
        && native_selected_tools_include_side_effect(tools)
        && state.changed_files.is_empty()
        && state.patch_refs.is_empty()
}

fn native_selected_tools_include_side_effect(tools: &BTreeSet<String>) -> bool {
    tools.iter().any(|tool| {
        matches!(
            canonical_builtin_tool_name(tool),
            Some("patch_apply" | "patch_preview" | "command_run")
        )
    })
}

fn native_tool_enabled(tools: &BTreeSet<String>, canonical: &str) -> bool {
    if tools.is_empty() {
        return matches!(
            canonical,
            "repo_find_files" | "repo_read_file_range" | "git_status" | "git_diff"
        );
    }
    let Some(definition) = builtin_tool(canonical) else {
        return false;
    };
    tools
        .iter()
        .any(|selected| canonical_builtin_tool_name(selected) == Some(definition.name))
}

fn write_native_repo_evidence(
    store: &RunStore,
    run_id: &RunId,
    kind: RepoEvidenceKind,
    repo_root: &str,
    summary: impl Into<String>,
    payload: Value,
) -> Result<RepoEvidenceRef, HarnessError> {
    let _guard = NATIVE_REPO_EVIDENCE_WRITE_LOCK
        .lock()
        .map_err(|_| HarnessError::Failed("native evidence write lock poisoned".to_owned()))?;
    store
        .write_repo_evidence(run_id, kind, repo_root, Vec::new(), summary, payload)
        .map_err(|error| HarnessError::Failed(error.to_string()))
}

fn repo_evidence_ref(reference: &RepoEvidenceRef) -> coder_core::EvidenceRef {
    coder_core::EvidenceRef {
        kind: "repo_evidence".to_owned(),
        reference: format!("repo-evidence://{}", reference.ref_id),
    }
}

fn native_event_has_tool_action(event: &HarnessRunEvent) -> bool {
    matches!(
        event.kind.as_str(),
        "native.tool.completed"
            | "native.tool.failed"
            | "native.tool.skipped"
            | "approval.requested"
    ) && event.payload.get("tool").and_then(Value::as_str).is_some()
}

fn native_public_tool_status(event: &HarnessRunEvent) -> String {
    if event.kind == "approval.requested" {
        return "blocked".to_owned();
    }
    event_payload_string(&event.payload, "status").unwrap_or_else(|| {
        if event.kind.ends_with(".failed") {
            "failed".to_owned()
        } else {
            "completed".to_owned()
        }
    })
}

fn native_observation_summary(event: &HarnessRunEvent, tool_name: &str, status: &str) -> String {
    if let Some(error) = event_payload_string(&event.payload, "error") {
        return format!("{tool_name} {status}: {}", truncate_public(&error, 220));
    }
    if let Some(evidence_ref) = first_event_ref_uri(event) {
        return format!("{tool_name} {status}; evidence recorded at {evidence_ref}");
    }
    if let Some(reason) = event_payload_string(&event.payload, "reason") {
        return format!("{tool_name} {status}: {}", truncate_public(&reason, 220));
    }
    format!("{tool_name} {status}.")
}

pub(crate) fn executor_terminal_event_kind(status: &str) -> &'static str {
    match status {
        "blocked" => "executor.blocked",
        "failed" => "executor.failed",
        "cancelled" | "canceled" => "executor.failed",
        _ => "executor.completed",
    }
}

fn event_payload_string(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn first_event_ref_uri(event: &HarnessRunEvent) -> Option<String> {
    event
        .refs
        .first()
        .map(|reference| reference.uri.clone())
        .or_else(|| {
            event_payload_string(&event.payload, "evidence_ref").map(|reference| {
                if reference.contains("://") {
                    reference
                } else {
                    format!("repo-evidence://{reference}")
                }
            })
        })
}

fn copy_event_refs(mut target: HarnessRunEvent, source: &HarnessRunEvent) -> HarnessRunEvent {
    for reference in &source.refs {
        target = target.with_ref(reference.label.clone(), reference.uri.clone());
    }
    target
}

pub(crate) fn truncate_public(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    let mut output = trimmed.chars().take(max_chars).collect::<String>();
    if trimmed.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn native_tool_event(
    tool: &str,
    status: &str,
    mut payload: Value,
    reference: Option<&RepoEvidenceRef>,
) -> HarnessRunEvent {
    if let Some(object) = payload.as_object_mut() {
        object.insert("tool".to_owned(), Value::String(tool.to_owned()));
        object.insert("status".to_owned(), Value::String(status.to_owned()));
        if let Some(reference) = reference {
            object.insert(
                "evidence_ref".to_owned(),
                Value::String(reference.ref_id.clone()),
            );
        }
    }
    let event = HarnessRunEvent::new("native.tool.completed", payload);
    if let Some(reference) = reference {
        event.with_ref(
            "repo_evidence",
            format!("repo-evidence://{}", reference.ref_id),
        )
    } else {
        event
    }
}

fn native_tool_failure_event(tool: &str, error: String) -> HarnessRunEvent {
    HarnessRunEvent::new(
        "native.tool.failed",
        json!({
            "tool": tool,
            "status": "failed",
            "error": error
        }),
    )
}

fn native_tool_skipped_event(tool: &str, reason: &str) -> HarnessRunEvent {
    HarnessRunEvent::new(
        "native.tool.skipped",
        json!({
            "tool": tool,
            "status": "skipped",
            "reason": reason
        }),
    )
}

fn native_search_query(task: &str) -> String {
    for marker in ['"', '\''] {
        let mut parts = task.split(marker);
        let _ = parts.next();
        if let Some(quoted) = parts.next() {
            let candidate = quoted.trim();
            if !candidate.is_empty() {
                return candidate.to_owned();
            }
        }
    }
    if task.to_ascii_lowercase().contains("todo") {
        "TODO".to_owned()
    } else {
        "fn ".to_owned()
    }
}

fn native_candidate_file(repo_root: &str, task: &str) -> Option<PathBuf> {
    if let Some(path) = native_path_token(task, &[".rs", ".py", ".ts", ".tsx", ".js", ".md"]) {
        return Some(path);
    }
    for preferred in ["README.md", "readme.md", "Cargo.toml", "package.json"] {
        if read_file_range(repo_root, preferred, 1, 1, 256).is_ok() {
            return Some(PathBuf::from(preferred));
        }
    }
    find_files(repo_root, None, &[], 20)
        .ok()
        .and_then(|files| files.into_iter().next())
        .map(|file| PathBuf::from(file.path))
}

fn native_patch_file(repo_root: &str, task: &str) -> Option<PathBuf> {
    if let Some(path) = native_path_token(task, &[".patch", ".diff"]) {
        return Some(path);
    }
    find_files(
        repo_root,
        None,
        &[String::from("patch"), String::from("diff")],
        20,
    )
    .ok()
    .and_then(|files| files.into_iter().next())
    .map(|file| PathBuf::from(file.path))
}

fn native_path_token(task: &str, suffixes: &[&str]) -> Option<PathBuf> {
    task.split_whitespace()
        .map(|token| {
            token.trim_matches(|ch: char| {
                ch == '"' || ch == '\'' || ch == '`' || ch == ',' || ch == ';' || ch == '.'
            })
        })
        .find(|token| suffixes.iter().any(|suffix| token.ends_with(suffix)))
        .map(PathBuf::from)
}

fn native_command_args(task: &str) -> Option<Vec<String>> {
    let lower = task.to_ascii_lowercase();
    let marker = lower.find("command:").or_else(|| lower.find("run:"))?;
    let command_start = marker + task[marker..].find(':')? + 1;
    let args = task[command_start..]
        .split_whitespace()
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if args.is_empty() {
        None
    } else {
        Some(args)
    }
}

fn native_task_requires_repository_changes(request: &HarnessRunRequest) -> bool {
    request
        .backend_context
        .pointer("/coder/task_context/execution_mode")
        .and_then(Value::as_str)
        == Some("write")
}

#[derive(Debug, Clone, Copy, Default)]
pub enum NativeMockOutcome {
    #[default]
    Completed,
    Blocked,
    Failed,
}

#[derive(Debug, Default)]
pub struct NativeMockBackend {
    outcome: NativeMockOutcome,
}

impl NativeMockBackend {
    pub fn new(outcome: NativeMockOutcome) -> Self {
        Self { outcome }
    }
}

#[async_trait]
impl HarnessBackend for NativeMockBackend {
    async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        let status = match self.outcome {
            NativeMockOutcome::Completed => "completed",
            NativeMockOutcome::Blocked => "blocked",
            NativeMockOutcome::Failed => "failed",
        };
        let summary = format!(
            "Native mock backend processed node '{}' for task '{}'.",
            request.node_id, request.task
        );
        let report = match self.outcome {
            NativeMockOutcome::Completed => FinalReport::completed(summary),
            NativeMockOutcome::Blocked => {
                FinalReport::blocked(summary, "native mock backend requested blocked outcome")
            }
            NativeMockOutcome::Failed => {
                FinalReport::failed(summary, "native mock backend requested failed outcome")
            }
        };
        Ok(HarnessRunResult {
            status: status.to_owned(),
            report: Some(report),
            events: vec![HarnessRunEvent::new(
                format!("backend.native_mock.{status}"),
                json!({
                    "backend": "native-rust",
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "status": status
                }),
            )],
        })
    }
}
