use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Path, State},
    Json,
};
use coder_core::RunId;
use coder_store::{CommandBackgroundTaskRecord, RepoEvidenceKind, RepoEvidenceRef, RunStore};
use coder_tools::{
    preview_command, start_command_process, CommandPolicyDecision, CommandProcessHandle,
    CommandProcessOutputState, CommandProcessRequest, CommandRunEvidence,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    ensure_tool_boundary, record_command_events, write_tool_evidence, ApiError, ApiState,
    CommandBackgroundCancelResponse, CommandBackgroundOutputResponse,
    CommandBackgroundStartRequest, CommandBackgroundStartResponse, CommandBackgroundStatusResponse,
    CommandWriteStdinRequest, CommandWriteStdinResponse,
};

#[derive(Debug)]
struct BackgroundOutputSnapshot {
    output: String,
    truncated: bool,
    cursor: u64,
    next_cursor: u64,
    gap: bool,
}

#[derive(Debug)]
pub(super) struct BackgroundCommandTask {
    task_id: String,
    run_id: Option<String>,
    repo_root: String,
    cwd: String,
    argv: Vec<String>,
    command: String,
    approval_key: String,
    policy: CommandPolicyDecision,
    status: String,
    created_at_ms: u64,
    updated_at_ms: u64,
    output_ref: String,
    max_output_bytes: usize,
    process: Option<CommandProcessHandle>,
    result: Option<CommandRunEvidence>,
    evidence_ref: Option<RepoEvidenceRef>,
    error: Option<String>,
}

impl BackgroundCommandTask {
    fn retained_output(&self) -> CommandProcessOutputState {
        if let Some(process) = &self.process {
            return process.retained_output();
        }
        let bytes = self
            .result
            .as_ref()
            .map(|result| result.output.as_bytes().to_vec())
            .unwrap_or_default();
        CommandProcessOutputState {
            total_bytes: bytes.len() as u64,
            bytes,
            start_offset: 0,
            truncated: self
                .result
                .as_ref()
                .is_some_and(|result| result.output_truncated),
        }
    }

    fn to_record(&self) -> CommandBackgroundTaskRecord {
        let output = self.retained_output();
        CommandBackgroundTaskRecord {
            task_id: self.task_id.clone(),
            run_id: self.run_id.clone(),
            repo_root: self.repo_root.clone(),
            cwd: self.cwd.clone(),
            argv: self.argv.clone(),
            command: self.command.clone(),
            approval_key: self.approval_key.clone(),
            policy: serde_json::to_value(&self.policy).unwrap_or(Value::Null),
            status: self.status.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            output_ref: self.output_ref.clone(),
            output_bytes: output.bytes.len() as u64,
            output_start_offset: output.start_offset,
            output_total_bytes: output.total_bytes,
            output_truncated: output.truncated,
            max_output_bytes: self.max_output_bytes,
            result: self
                .result
                .as_ref()
                .and_then(|result| serde_json::to_value(result).ok()),
            evidence_ref: self.evidence_ref.clone(),
            error: self.error.clone(),
        }
    }

    fn status_response_since(&self, cursor: Option<u64>) -> CommandBackgroundStatusResponse {
        let output = if let Some(process) = &self.process {
            let snapshot = process.snapshot(cursor);
            BackgroundOutputSnapshot {
                output: snapshot.output,
                truncated: snapshot.output_truncated,
                cursor: snapshot.output_cursor,
                next_cursor: snapshot.next_output_cursor,
                gap: snapshot.output_gap,
            }
        } else {
            let output = self.retained_output();
            BackgroundOutputSnapshot {
                output: String::from_utf8_lossy(&output.bytes).to_string(),
                truncated: output.truncated,
                cursor: output.start_offset,
                next_cursor: output.total_bytes,
                gap: false,
            }
        };
        CommandBackgroundStatusResponse {
            task_id: self.task_id.clone(),
            status: self.status.clone(),
            command: self.command.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            output_preview: output.output,
            output_truncated: output.truncated,
            output_cursor: output.cursor,
            next_output_cursor: output.next_cursor,
            output_gap: output.gap,
            evidence_ref: self.evidence_ref.clone(),
            result: self.result.clone(),
            error: self.error.clone(),
        }
    }

    fn output_response(&self) -> CommandBackgroundOutputResponse {
        let status = self.status_response_since(None);
        CommandBackgroundOutputResponse {
            task_id: self.task_id.clone(),
            status: self.status.clone(),
            output: status.output_preview,
            output_truncated: status.output_truncated,
            output_cursor: status.output_cursor,
            next_output_cursor: status.next_output_cursor,
            output_gap: status.output_gap,
        }
    }
}

pub(super) async fn start_background_command_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<CommandBackgroundStartRequest>,
) -> Result<Json<CommandBackgroundStartResponse>, ApiError> {
    ensure_tool_boundary("command_background")?;
    Ok(Json(start_background_command_task(&state, request)?))
}

pub(super) fn start_background_command_task(
    state: &ApiState,
    request: CommandBackgroundStartRequest,
) -> Result<CommandBackgroundStartResponse, ApiError> {
    let CommandBackgroundStartRequest {
        repo_root,
        cwd,
        argv,
        timeout_seconds,
        max_output_bytes,
        interactive,
        source,
        sandbox,
        approved,
        run_id,
    } = request;
    let cwd = cwd.unwrap_or_else(|| ".".to_owned());
    let source = source.unwrap_or_else(|| "model".to_owned());
    let sandbox = sandbox.unwrap_or(false);
    let approved = approved.unwrap_or(false);
    let timeout_seconds = effective_background_command_timeout(timeout_seconds);
    let max_output_bytes = effective_background_command_output_limit(max_output_bytes);
    let interactive = interactive.unwrap_or(false);
    let preview = preview_command(&repo_root, &cwd, argv.clone(), &source, sandbox)?;
    let now = unix_time_millis();

    if preview.requires_approval && !approved {
        let task_id = format!("blocked-{}", Uuid::new_v4());
        let output_ref = command_background_output_ref(&task_id);
        let output = format!(
            "Check command requires explicit approval: {}",
            preview.command
        );
        let evidence = CommandRunEvidence {
            repo_root: preview.repo_root.clone(),
            cwd: preview.cwd.clone(),
            argv: preview.argv.clone(),
            command: preview.command.clone(),
            status: "blocked".to_owned(),
            passed: false,
            blocked: true,
            requires_approval: true,
            approval_key: preview.approval_key.clone(),
            returncode: None,
            output,
            output_truncated: false,
            timed_out: false,
            policy: preview.policy.clone(),
            evidence_kind: "command_evidence".to_owned(),
        };
        let evidence_ref = persist_background_command_evidence(
            &state.store,
            run_id.as_deref(),
            &preview.repo_root,
            &evidence,
        )?;
        let response_evidence_ref = evidence_ref.clone();
        let task = BackgroundCommandTask {
            task_id: task_id.clone(),
            run_id: run_id.clone(),
            repo_root: preview.repo_root,
            cwd: preview.cwd,
            argv: preview.argv,
            command: preview.command.clone(),
            approval_key: preview.approval_key,
            policy: preview.policy,
            status: "blocked".to_owned(),
            created_at_ms: now,
            updated_at_ms: now,
            output_ref,
            max_output_bytes,
            process: None,
            result: Some(evidence.clone()),
            evidence_ref,
            error: None,
        };
        persist_background_command_task(&state.store, &task)?;
        return Ok(background_start_response(
            &task_id,
            "blocked",
            &preview.command,
            response_evidence_ref,
        ));
    }

    let process = start_command_process(
        preview.clone(),
        CommandProcessRequest {
            timeout_seconds,
            max_output_bytes,
            source,
            interactive,
            initial_stdin: None,
        },
    )?;
    let task_id = process.process_id().to_owned();
    let output_ref = command_background_output_ref(&task_id);
    let task = Arc::new(Mutex::new(BackgroundCommandTask {
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        repo_root: preview.repo_root.clone(),
        cwd: preview.cwd.clone(),
        argv: preview.argv.clone(),
        command: preview.command.clone(),
        approval_key: preview.approval_key.clone(),
        policy: preview.policy.clone(),
        status: "running".to_owned(),
        created_at_ms: now,
        updated_at_ms: now,
        output_ref,
        max_output_bytes,
        process: Some(process.clone()),
        result: None,
        evidence_ref: None,
        error: None,
    }));
    if let Err(error) = persist_background_command_task(&state.store, &task.lock().unwrap()) {
        let _ = process.cancel();
        return Err(error);
    }
    state
        .background_commands
        .lock()
        .unwrap()
        .insert(task_id.clone(), task.clone());
    spawn_background_command_projection(
        task.clone(),
        state.clone(),
        task_id.clone(),
        run_id,
        process,
    );

    Ok(background_start_response(
        &task_id,
        "running",
        &preview.command,
        None,
    ))
}

pub(super) async fn get_background_command_endpoint(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
) -> Result<Json<CommandBackgroundStatusResponse>, ApiError> {
    ensure_tool_boundary("read_command_output")?;
    Ok(Json(background_command_status(&state, &task_id)?))
}

pub(super) async fn get_background_command_output_endpoint(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
) -> Result<Json<CommandBackgroundOutputResponse>, ApiError> {
    ensure_tool_boundary("read_command_output")?;
    if let Some(task) = find_background_command_task(&state, &task_id) {
        let response = task.lock().unwrap().output_response();
        return Ok(Json(response));
    }
    Ok(Json(recovered_background_command_output(&state, &task_id)?))
}

pub(super) fn background_command_status(
    state: &ApiState,
    task_id: &str,
) -> Result<CommandBackgroundStatusResponse, ApiError> {
    background_command_status_since(state, task_id, None)
}

pub(super) fn background_command_status_since(
    state: &ApiState,
    task_id: &str,
    cursor: Option<u64>,
) -> Result<CommandBackgroundStatusResponse, ApiError> {
    if let Some(task) = find_background_command_task(state, task_id) {
        let response = task.lock().unwrap().status_response_since(cursor);
        return Ok(response);
    }
    recovered_background_command_status(state, task_id, cursor)
}

pub(super) async fn cancel_background_command_endpoint(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
) -> Result<Json<CommandBackgroundCancelResponse>, ApiError> {
    ensure_tool_boundary("cancel_command_background")?;
    let Some(task) = find_background_command_task(&state, &task_id) else {
        return Ok(Json(cancel_durable_background_command_task(
            &state, &task_id,
        )?));
    };
    let mut task = task.lock().unwrap();
    let cancelled = if let Some(process) = &task.process {
        process.cancel()?
    } else {
        task.status == "cancelled"
    };
    task.updated_at_ms = unix_time_millis();
    persist_background_command_task(&state.store, &task)?;
    let status = if cancelled {
        "cancelled".to_owned()
    } else {
        task.status.clone()
    };
    Ok(Json(CommandBackgroundCancelResponse {
        task_id,
        cancelled,
        status,
    }))
}

pub(super) async fn write_background_command_stdin_endpoint(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
    Json(request): Json<CommandWriteStdinRequest>,
) -> Result<Json<CommandWriteStdinResponse>, ApiError> {
    ensure_tool_boundary("write_stdin")?;
    Ok(Json(write_background_command_stdin(
        &state,
        &task_id,
        &request.input,
        request.close_stdin,
    )?))
}

pub(super) fn write_background_command_stdin(
    state: &ApiState,
    task_id: &str,
    input: &str,
    close_stdin: bool,
) -> Result<CommandWriteStdinResponse, ApiError> {
    let task = find_background_command_task(state, task_id)
        .ok_or_else(|| ApiError::not_found(format!("live command process not found: {task_id}")))?;
    let mut task = task.lock().unwrap();
    if task.status != "running" {
        return Err(ApiError::bad_request(format!(
            "command process {task_id} is not running"
        )));
    }
    let process = task
        .process
        .as_ref()
        .ok_or_else(|| ApiError::bad_request(format!("command process {task_id} has exited")))?;
    let bytes_written = process.write_stdin(input, close_stdin)?;
    task.updated_at_ms = unix_time_millis();
    persist_background_command_task(&state.store, &task)?;
    Ok(CommandWriteStdinResponse {
        task_id: task_id.to_owned(),
        status: task.status.clone(),
        bytes_written,
        stdin_closed: close_stdin,
    })
}

fn find_background_command_task(
    state: &ApiState,
    task_id: &str,
) -> Option<Arc<Mutex<BackgroundCommandTask>>> {
    state
        .background_commands
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
}

fn spawn_background_command_projection(
    task: Arc<Mutex<BackgroundCommandTask>>,
    state: ApiState,
    task_id: String,
    run_id: Option<String>,
    process: CommandProcessHandle,
) {
    std::thread::spawn(move || {
        loop {
            let snapshot = process.wait(Some(Duration::from_millis(100)));
            let mut task_guard = task.lock().unwrap();
            task_guard.updated_at_ms = unix_time_millis();
            if let Err(error) = persist_background_command_task(&state.store, &task_guard) {
                task_guard.error = Some(format!(
                    "failed to persist command process output: {}",
                    error.message
                ));
            }
            if snapshot.status != "running" {
                break;
            }
        }

        let Some(evidence) = process.evidence() else {
            let mut task_guard = task.lock().unwrap();
            task_guard.status = "failed".to_owned();
            task_guard.error = Some("command process completed without evidence".to_owned());
            task_guard.updated_at_ms = unix_time_millis();
            let _ = persist_background_command_task(&state.store, &task_guard);
            state.background_commands.lock().unwrap().remove(&task_id);
            return;
        };
        let (status, repo_root) = {
            let task_guard = task.lock().unwrap();
            (evidence.status.clone(), task_guard.repo_root.clone())
        };

        match persist_background_command_evidence(
            &state.store,
            run_id.as_deref(),
            &repo_root,
            &evidence,
        ) {
            Ok(evidence_ref) => {
                let mut task_guard = task.lock().unwrap();
                task_guard.status = status;
                task_guard.result = Some(evidence);
                task_guard.evidence_ref = evidence_ref;
                task_guard.updated_at_ms = unix_time_millis();
                let _ = persist_background_command_task(&state.store, &task_guard);
            }
            Err(error) => {
                let mut task_guard = task.lock().unwrap();
                task_guard.status = status;
                task_guard.result = Some(evidence);
                task_guard.error = Some(format!(
                    "background command completed, but evidence write failed for {}: {}",
                    repo_root, error.message
                ));
                task_guard.updated_at_ms = unix_time_millis();
                let _ = persist_background_command_task(&state.store, &task_guard);
            }
        }
        state.background_commands.lock().unwrap().remove(&task_id);
    });
}

fn persist_background_command_task(
    store: &RunStore,
    task: &BackgroundCommandTask,
) -> Result<(), ApiError> {
    let output = task.retained_output();
    store
        .write_command_background_output_tail(&task.task_id, &output.bytes)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    store
        .write_command_background_task_record(&task.to_record())
        .map(|_| ())
        .map_err(|error| ApiError::internal(error.to_string()))
}

fn recovered_background_command_status(
    state: &ApiState,
    task_id: &str,
    cursor: Option<u64>,
) -> Result<CommandBackgroundStatusResponse, ApiError> {
    let mut record = read_command_background_task_record(state, task_id)?;
    if record.status == "running" {
        record.status = "lost".to_owned();
        record.updated_at_ms = unix_time_millis();
        record.error = Some(
            "background command task was running, but no live process registry exists after restart"
                .to_owned(),
        );
        state
            .store
            .write_command_background_task_record(&record)
            .map_err(|error| ApiError::internal(error.to_string()))?;
    }
    let output = state
        .store
        .read_command_background_output_tail(&record.task_id, record.max_output_bytes)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let store_truncated = output.truncated;
    let result = command_result_from_record(&record);
    let output = recovered_output_snapshot(&record, output.output.as_bytes(), cursor);
    Ok(CommandBackgroundStatusResponse {
        task_id: record.task_id,
        status: record.status,
        command: record.command,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
        output_preview: output.output,
        output_truncated: record.output_truncated || store_truncated,
        output_cursor: output.cursor,
        next_output_cursor: output.next_cursor,
        output_gap: output.gap,
        evidence_ref: record.evidence_ref,
        result,
        error: record.error,
    })
}

fn recovered_background_command_output(
    state: &ApiState,
    task_id: &str,
) -> Result<CommandBackgroundOutputResponse, ApiError> {
    let mut record = read_command_background_task_record(state, task_id)?;
    if record.status == "running" {
        record.status = "lost".to_owned();
        record.updated_at_ms = unix_time_millis();
        record.error = Some(
            "background command task was running, but no live process registry exists after restart"
                .to_owned(),
        );
        state
            .store
            .write_command_background_task_record(&record)
            .map_err(|error| ApiError::internal(error.to_string()))?;
    }
    let output = state
        .store
        .read_command_background_output_tail(&record.task_id, record.max_output_bytes)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let store_truncated = output.truncated;
    let output = recovered_output_snapshot(&record, output.output.as_bytes(), None);
    Ok(CommandBackgroundOutputResponse {
        task_id: record.task_id,
        status: record.status,
        output: output.output,
        output_truncated: record.output_truncated || store_truncated,
        output_cursor: output.cursor,
        next_output_cursor: output.next_cursor,
        output_gap: output.gap,
    })
}

fn recovered_output_snapshot(
    record: &CommandBackgroundTaskRecord,
    bytes: &[u8],
    requested_cursor: Option<u64>,
) -> BackgroundOutputSnapshot {
    let total_bytes = record.output_total_bytes.max(
        record
            .output_start_offset
            .saturating_add(bytes.len() as u64),
    );
    let start_offset = if record.output_total_bytes == 0 {
        total_bytes.saturating_sub(bytes.len() as u64)
    } else {
        record.output_start_offset
    };
    let requested_cursor = requested_cursor.unwrap_or(start_offset);
    let gap = requested_cursor < start_offset;
    let cursor = requested_cursor.clamp(start_offset, total_bytes);
    let relative = cursor.saturating_sub(start_offset) as usize;
    BackgroundOutputSnapshot {
        output: String::from_utf8_lossy(&bytes[relative.min(bytes.len())..]).to_string(),
        truncated: record.output_truncated,
        cursor,
        next_cursor: total_bytes,
        gap,
    }
}

fn cancel_durable_background_command_task(
    state: &ApiState,
    task_id: &str,
) -> Result<CommandBackgroundCancelResponse, ApiError> {
    let mut record = read_command_background_task_record(state, task_id)?;
    let mut cancelled = false;
    match record.status.as_str() {
        "running" => {
            record.status = "lost".to_owned();
            record.updated_at_ms = unix_time_millis();
            record.error = Some(
                "background command task could not be cancelled after restart because no live process handle exists"
                    .to_owned(),
            );
            state
                .store
                .write_command_background_task_record(&record)
                .map_err(|error| ApiError::internal(error.to_string()))?;
        }
        "cancelled" => {
            cancelled = true;
        }
        _ => {}
    }
    Ok(CommandBackgroundCancelResponse {
        task_id: record.task_id,
        cancelled,
        status: record.status,
    })
}

fn read_command_background_task_record(
    state: &ApiState,
    task_id: &str,
) -> Result<CommandBackgroundTaskRecord, ApiError> {
    state
        .store
        .read_command_background_task_record(task_id)
        .map_err(|error| ApiError::internal(error.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("background command task not found: {task_id}")))
}

fn command_result_from_record(record: &CommandBackgroundTaskRecord) -> Option<CommandRunEvidence> {
    record
        .result
        .clone()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn persist_background_command_evidence(
    store: &RunStore,
    run_id: Option<&str>,
    repo_root: &str,
    evidence: &CommandRunEvidence,
) -> Result<Option<RepoEvidenceRef>, ApiError> {
    let evidence_ref = write_tool_evidence(
        store,
        run_id,
        RepoEvidenceKind::RepoTest,
        repo_root,
        "Ran background command through Rust tool endpoint.",
        json!({
            "evidence_kind": "command_evidence",
            "operation": "command_background",
            "result": serde_json::to_value(evidence).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    if let (Some(run_id), Some(reference)) = (run_id, &evidence_ref) {
        record_command_events(
            store,
            &RunId::from_string(run_id.to_owned()),
            evidence,
            reference,
        )?;
    }
    Ok(evidence_ref)
}

fn background_start_response(
    task_id: &str,
    status: &str,
    command: &str,
    evidence_ref: Option<RepoEvidenceRef>,
) -> CommandBackgroundStartResponse {
    CommandBackgroundStartResponse {
        task_id: task_id.to_owned(),
        status: status.to_owned(),
        command: command.to_owned(),
        status_url: format!("/api/v3/tools/command/background/{task_id}"),
        output_url: format!("/api/v3/tools/command/background/{task_id}/output"),
        cancel_url: format!("/api/v3/tools/command/background/{task_id}"),
        evidence_ref,
    }
}

fn command_background_output_ref(task_id: &str) -> String {
    format!("background-task-output://commands/{task_id}.output")
}

fn effective_background_command_timeout(requested: Option<u64>) -> Option<u64> {
    requested.map(|timeout| timeout.clamp(1, coder_tools::MAX_COMMAND_TIMEOUT_SECONDS))
}

fn effective_background_command_output_limit(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(coder_tools::DEFAULT_MAX_COMMAND_OUTPUT_BYTES)
        .clamp(1, coder_tools::DEFAULT_MAX_COMMAND_OUTPUT_BYTES)
}

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
