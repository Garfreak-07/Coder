use std::{
    fs,
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Path, State},
    Json,
};
use coder_core::RunId;
use coder_store::{CommandBackgroundTaskRecord, RepoEvidenceKind, RepoEvidenceRef, RunStore};
use coder_tools::{preview_command, CommandPolicyDecision, CommandRunEvidence};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    ensure_tool_boundary, record_command_events, write_tool_evidence, ApiError, ApiState,
    CommandBackgroundCancelResponse, CommandBackgroundOutputResponse,
    CommandBackgroundStartRequest, CommandBackgroundStartResponse, CommandBackgroundStatusResponse,
};

#[derive(Debug)]
struct BackgroundOutputTail {
    bytes: Vec<u8>,
    max_bytes: usize,
    truncated: bool,
}

impl BackgroundOutputTail {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes: max_bytes.clamp(1, coder_tools::DEFAULT_MAX_COMMAND_OUTPUT_BYTES),
            truncated: false,
        }
    }

    fn append(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        if chunk.len() >= self.max_bytes {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&chunk[chunk.len() - self.max_bytes..]);
            self.truncated = true;
            return;
        }
        let overflow = self
            .bytes
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(self.max_bytes);
        if overflow > 0 {
            self.bytes.drain(0..overflow);
            self.truncated = true;
        }
        self.bytes.extend_from_slice(chunk);
    }

    fn snapshot(&self) -> (String, bool) {
        (
            String::from_utf8_lossy(&self.bytes).to_string(),
            self.truncated,
        )
    }

    fn snapshot_bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }
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
    output: BackgroundOutputTail,
    output_ref: String,
    max_output_bytes: usize,
    child: Option<Child>,
    cancel_requested: bool,
    result: Option<CommandRunEvidence>,
    evidence_ref: Option<RepoEvidenceRef>,
    error: Option<String>,
}

impl BackgroundCommandTask {
    fn to_record(&self) -> CommandBackgroundTaskRecord {
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
            output_bytes: self.output.byte_len(),
            output_truncated: self.output.truncated,
            max_output_bytes: self.max_output_bytes,
            result: self
                .result
                .as_ref()
                .and_then(|result| serde_json::to_value(result).ok()),
            evidence_ref: self.evidence_ref.clone(),
            error: self.error.clone(),
        }
    }

    fn status_response(&self) -> CommandBackgroundStatusResponse {
        let (output_preview, output_truncated) = self.output.snapshot();
        CommandBackgroundStatusResponse {
            task_id: self.task_id.clone(),
            status: self.status.clone(),
            command: self.command.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            output_preview,
            output_truncated,
            evidence_ref: self.evidence_ref.clone(),
            result: self.result.clone(),
            error: self.error.clone(),
        }
    }

    fn output_response(&self) -> CommandBackgroundOutputResponse {
        let (output, output_truncated) = self.output.snapshot();
        CommandBackgroundOutputResponse {
            task_id: self.task_id.clone(),
            status: self.status.clone(),
            output,
            output_truncated,
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
    let preview = preview_command(&repo_root, &cwd, argv.clone(), &source, sandbox)?;
    let task_id = Uuid::new_v4().to_string();
    let output_ref = command_background_output_ref(&task_id);
    let now = unix_time_millis();

    if preview.requires_approval && !approved {
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
        let mut task = BackgroundCommandTask {
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
            output: BackgroundOutputTail::new(max_output_bytes),
            output_ref,
            max_output_bytes,
            child: None,
            cancel_requested: false,
            result: Some(evidence.clone()),
            evidence_ref,
            error: None,
        };
        task.output.append(evidence.output.as_bytes());
        persist_background_command_task(&state.store, &task)?;
        return Ok(background_start_response(
            &task_id,
            "blocked",
            &preview.command,
            response_evidence_ref,
        ));
    }

    let (repo_root_path, workdir, cwd_display) = resolve_background_command_dir(&repo_root, &cwd)?;
    let mut child = Command::new(&preview.argv[0])
        .args(&preview.argv[1..])
        .current_dir(&workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            ApiError::internal(format!("failed to spawn background command: {error}"))
        })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let task = Arc::new(Mutex::new(BackgroundCommandTask {
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        repo_root: repo_root_path.display().to_string(),
        cwd: cwd_display,
        argv: preview.argv.clone(),
        command: preview.command.clone(),
        approval_key: preview.approval_key.clone(),
        policy: preview.policy.clone(),
        status: "running".to_owned(),
        created_at_ms: now,
        updated_at_ms: now,
        output: BackgroundOutputTail::new(max_output_bytes),
        output_ref,
        max_output_bytes,
        child: Some(child),
        cancel_requested: false,
        result: None,
        evidence_ref: None,
        error: None,
    }));
    if let Err(error) = persist_background_command_task(&state.store, &task.lock().unwrap()) {
        if let Some(child) = task.lock().unwrap().child.as_mut() {
            let _ = child.kill();
        }
        return Err(error);
    }
    state
        .background_commands
        .lock()
        .unwrap()
        .insert(task_id.clone(), task.clone());
    let mut output_readers = Vec::new();
    if let Some(reader) = spawn_background_output_reader(state.store.clone(), task.clone(), stdout)
    {
        output_readers.push(reader);
    }
    if let Some(reader) = spawn_background_output_reader(state.store.clone(), task.clone(), stderr)
    {
        output_readers.push(reader);
    }
    spawn_background_command_worker(
        task.clone(),
        state.clone(),
        task_id.clone(),
        run_id,
        timeout_seconds,
        output_readers,
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
    if let Some(task) = find_background_command_task(state, task_id) {
        let response = task.lock().unwrap().status_response();
        return Ok(response);
    }
    recovered_background_command_status(state, task_id)
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
    let mut cancelled = false;
    match task.status.as_str() {
        "running" => {
            task.cancel_requested = true;
            task.status = "cancelled".to_owned();
            task.updated_at_ms = unix_time_millis();
            if let Some(child) = task.child.as_mut() {
                let _ = child.kill();
            }
            persist_background_command_task(&state.store, &task)?;
            cancelled = true;
        }
        "cancelled" => {
            cancelled = true;
        }
        _ => {}
    }
    let status = task.status.clone();
    let terminal = status != "running";
    drop(task);
    if terminal {
        state.background_commands.lock().unwrap().remove(&task_id);
    }
    Ok(Json(CommandBackgroundCancelResponse {
        task_id,
        cancelled,
        status,
    }))
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

fn spawn_background_output_reader(
    store: RunStore,
    task: Arc<Mutex<BackgroundCommandTask>>,
    stream: Option<impl Read + Send + 'static>,
) -> Option<std::thread::JoinHandle<()>> {
    let mut stream = stream?;
    Some(std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let mut task = task.lock().unwrap();
                    task.output.append(&buffer[..read]);
                    task.updated_at_ms = unix_time_millis();
                    if let Err(error) = persist_background_command_task(&store, &task) {
                        task.error = Some(format!(
                            "failed to persist background command output: {}",
                            error.message
                        ));
                    }
                }
                Err(error) => {
                    let mut task = task.lock().unwrap();
                    task.error = Some(format!("failed to read background command output: {error}"));
                    task.updated_at_ms = unix_time_millis();
                    let _ = persist_background_command_task(&store, &task);
                    break;
                }
            }
        }
    }))
}

fn spawn_background_command_worker(
    task: Arc<Mutex<BackgroundCommandTask>>,
    state: ApiState,
    task_id: String,
    run_id: Option<String>,
    timeout_seconds: Option<u64>,
    output_readers: Vec<std::thread::JoinHandle<()>>,
) {
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let mut timed_out = false;
        let mut returncode = None;
        let mut passed = false;
        loop {
            let mut task_guard = task.lock().unwrap();
            let cancel_requested = task_guard.cancel_requested;
            if let Some(child) = task_guard.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        returncode = status.code();
                        passed = !timed_out && !cancel_requested && status.success();
                        task_guard.child = None;
                        break;
                    }
                    Ok(None) => {
                        if let Some(timeout_seconds) = timeout_seconds {
                            if !cancel_requested
                                && !timed_out
                                && started.elapsed() >= Duration::from_secs(timeout_seconds)
                            {
                                timed_out = true;
                                let _ = child.kill();
                            }
                        }
                    }
                    Err(error) => {
                        task_guard.error =
                            Some(format!("failed to poll background command status: {error}"));
                        task_guard.child = None;
                        break;
                    }
                }
            } else {
                break;
            }
            drop(task_guard);
            std::thread::sleep(Duration::from_millis(25));
        }
        for reader in output_readers {
            let _ = reader.join();
        }

        let (status, repo_root, evidence, output_repo_root) = {
            let task_guard = task.lock().unwrap();
            let cancelled = task_guard.cancel_requested;
            let status = if cancelled {
                "cancelled"
            } else if timed_out {
                "timeout"
            } else if passed {
                "completed"
            } else {
                "failed"
            };
            let (output, output_truncated) = task_guard.output.snapshot();
            let evidence = CommandRunEvidence {
                repo_root: task_guard.repo_root.clone(),
                cwd: task_guard.cwd.clone(),
                argv: task_guard.argv.clone(),
                command: task_guard.command.clone(),
                status: status.to_owned(),
                passed,
                blocked: false,
                requires_approval: false,
                approval_key: task_guard.approval_key.clone(),
                returncode,
                output,
                output_truncated,
                timed_out,
                policy: task_guard.policy.clone(),
                evidence_kind: "command_evidence".to_owned(),
            };
            (
                status.to_owned(),
                task_guard.repo_root.clone(),
                evidence,
                task_guard.repo_root.clone(),
            )
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
                    output_repo_root, error.message
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
    store
        .write_command_background_output_tail(&task.task_id, &task.output.snapshot_bytes())
        .map_err(|error| ApiError::internal(error.to_string()))?;
    store
        .write_command_background_task_record(&task.to_record())
        .map(|_| ())
        .map_err(|error| ApiError::internal(error.to_string()))
}

fn recovered_background_command_status(
    state: &ApiState,
    task_id: &str,
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
    let result = command_result_from_record(&record);
    Ok(CommandBackgroundStatusResponse {
        task_id: record.task_id,
        status: record.status,
        command: record.command,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
        output_preview: output.output,
        output_truncated: record.output_truncated || output.truncated,
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
    Ok(CommandBackgroundOutputResponse {
        task_id: record.task_id,
        status: record.status,
        output: output.output,
        output_truncated: record.output_truncated || output.truncated,
    })
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

fn resolve_background_command_dir(
    repo_root: &str,
    cwd: &str,
) -> Result<(PathBuf, PathBuf, String), ApiError> {
    let root = fs::canonicalize(repo_root)
        .map_err(|error| ApiError::bad_request(format!("invalid repo root: {error}")))?;
    if !root.is_dir() {
        return Err(ApiError::bad_request(format!(
            "repo root is not a directory: {}",
            root.display()
        )));
    }
    let workdir = fs::canonicalize(root.join(cwd))
        .map_err(|error| ApiError::bad_request(format!("invalid command cwd: {error}")))?;
    if !workdir.starts_with(&root) {
        return Err(ApiError::bad_request(format!(
            "command cwd is outside repo: {}",
            workdir.display()
        )));
    }
    if !workdir.is_dir() {
        return Err(ApiError::bad_request(format!(
            "command cwd is not a directory: {}",
            workdir.display()
        )));
    }
    let cwd_display = workdir
        .strip_prefix(&root)
        .ok()
        .and_then(|path| {
            if path.as_os_str().is_empty() {
                None
            } else {
                Some(path.to_string_lossy().replace('\\', "/"))
            }
        })
        .unwrap_or_else(|| ".".to_owned());
    Ok((root, workdir, cwd_display))
}

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
