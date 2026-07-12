use std::{
    io::{Read, Write},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

use super::{
    configure_model_command_environment, resolve_repo_dir, CommandPreview, CommandRunEvidence,
    RepoToolError, DEFAULT_MAX_COMMAND_OUTPUT_BYTES, MAX_COMMAND_TIMEOUT_SECONDS,
};

pub const MAX_LIVE_COMMAND_PROCESSES: usize = 64;
static LIVE_COMMAND_PROCESSES: AtomicUsize = AtomicUsize::new(0);

struct CommandProcessPermit;

impl Drop for CommandProcessPermit {
    fn drop(&mut self) {
        LIVE_COMMAND_PROCESSES.fetch_sub(1, Ordering::AcqRel);
    }
}

fn reserve_process_slot() -> Result<CommandProcessPermit, RepoToolError> {
    if try_reserve_process_slot(&LIVE_COMMAND_PROCESSES, MAX_LIVE_COMMAND_PROCESSES) {
        Ok(CommandProcessPermit)
    } else {
        Err(RepoToolError::CommandProcessLimitReached(
            MAX_LIVE_COMMAND_PROCESSES,
        ))
    }
}

fn try_reserve_process_slot(counter: &AtomicUsize, limit: usize) -> bool {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        if current >= limit {
            return false;
        }
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommandProcessRequest {
    pub timeout_seconds: Option<u64>,
    pub max_output_bytes: usize,
    pub source: String,
    pub interactive: bool,
    pub initial_stdin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandProcessSnapshot {
    pub process_id: String,
    pub status: String,
    pub output: String,
    pub output_truncated: bool,
    pub output_cursor: u64,
    pub next_output_cursor: u64,
    pub output_gap: bool,
    pub returncode: Option<i32>,
    pub timed_out: bool,
    pub cancel_requested: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandProcessOutputState {
    pub bytes: Vec<u8>,
    pub start_offset: u64,
    pub total_bytes: u64,
    pub truncated: bool,
}

#[derive(Debug)]
struct OutputTail {
    bytes: Vec<u8>,
    max_bytes: usize,
    start_offset: u64,
    total_bytes: u64,
    truncated: bool,
}

impl OutputTail {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes: max_bytes.clamp(1, DEFAULT_MAX_COMMAND_OUTPUT_BYTES),
            start_offset: 0,
            total_bytes: 0,
            truncated: false,
        }
    }

    fn append(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        self.total_bytes = self.total_bytes.saturating_add(chunk.len() as u64);
        if chunk.len() >= self.max_bytes {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&chunk[chunk.len() - self.max_bytes..]);
            self.start_offset = self.total_bytes.saturating_sub(self.max_bytes as u64);
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
            self.start_offset = self.start_offset.saturating_add(overflow as u64);
            self.truncated = true;
        }
        self.bytes.extend_from_slice(chunk);
    }

    fn snapshot(&self, requested_cursor: Option<u64>) -> CommandProcessOutputState {
        let requested_cursor = requested_cursor.unwrap_or(self.start_offset);
        let cursor = requested_cursor.clamp(self.start_offset, self.total_bytes);
        let relative = cursor.saturating_sub(self.start_offset) as usize;
        CommandProcessOutputState {
            bytes: self.bytes[relative.min(self.bytes.len())..].to_vec(),
            start_offset: cursor,
            total_bytes: self.total_bytes,
            truncated: self.truncated,
        }
    }

    fn retained(&self) -> CommandProcessOutputState {
        CommandProcessOutputState {
            bytes: self.bytes.clone(),
            start_offset: self.start_offset,
            total_bytes: self.total_bytes,
            truncated: self.truncated,
        }
    }
}

#[derive(Debug)]
struct CommandProcessState {
    preview: CommandPreview,
    status: String,
    child: Option<Child>,
    output: OutputTail,
    returncode: Option<i32>,
    timed_out: bool,
    cancel_requested: bool,
    error: Option<String>,
}

#[derive(Debug)]
struct SharedCommandProcess {
    process_id: String,
    state: Mutex<CommandProcessState>,
    changed: Condvar,
}

#[derive(Debug, Clone)]
pub struct CommandProcessHandle {
    shared: Arc<SharedCommandProcess>,
}

impl CommandProcessHandle {
    pub fn process_id(&self) -> &str {
        &self.shared.process_id
    }

    pub fn snapshot(&self, cursor: Option<u64>) -> CommandProcessSnapshot {
        let state = self.shared.state.lock().unwrap();
        snapshot_state(&self.shared.process_id, &state, cursor)
    }

    pub fn retained_output(&self) -> CommandProcessOutputState {
        self.shared.state.lock().unwrap().output.retained()
    }

    pub fn evidence(&self) -> Option<CommandRunEvidence> {
        let state = self.shared.state.lock().unwrap();
        if state.status == "running" {
            return None;
        }
        let output = state.output.retained();
        Some(CommandRunEvidence {
            repo_root: state.preview.repo_root.clone(),
            cwd: state.preview.cwd.clone(),
            argv: state.preview.argv.clone(),
            command: state.preview.command.clone(),
            status: state.status.clone(),
            passed: state.status == "completed",
            blocked: false,
            requires_approval: false,
            approval_key: state.preview.approval_key.clone(),
            returncode: state.returncode,
            output: String::from_utf8_lossy(&output.bytes).to_string(),
            output_truncated: output.truncated,
            timed_out: state.timed_out,
            policy: state.preview.policy.clone(),
            evidence_kind: "command_evidence".to_owned(),
        })
    }

    pub fn wait(&self, timeout: Option<Duration>) -> CommandProcessSnapshot {
        let started = Instant::now();
        let mut state = self.shared.state.lock().unwrap();
        while state.status == "running" {
            match timeout.map(|timeout| timeout.saturating_sub(started.elapsed())) {
                Some(remaining) if remaining.is_zero() => break,
                Some(remaining) => {
                    let (next, result) =
                        self.shared.changed.wait_timeout(state, remaining).unwrap();
                    state = next;
                    if result.timed_out() {
                        break;
                    }
                }
                None => state = self.shared.changed.wait(state).unwrap(),
            }
        }
        snapshot_state(&self.shared.process_id, &state, None)
    }

    pub fn write_stdin(&self, input: &str, close_stdin: bool) -> Result<usize, RepoToolError> {
        let mut state = self.shared.state.lock().unwrap();
        if state.status != "running" {
            return Err(RepoToolError::CommandIo(std::io::Error::other(
                "command process is not running",
            )));
        }
        let child = state.child.as_mut().ok_or_else(|| {
            RepoToolError::CommandIo(std::io::Error::other("command process has exited"))
        })?;
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            RepoToolError::CommandIo(std::io::Error::other(
                "command process was not started as interactive",
            ))
        })?;
        stdin
            .write_all(input.as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(RepoToolError::CommandIo)?;
        if close_stdin {
            child.stdin.take();
        }
        Ok(input.len())
    }

    pub fn cancel(&self) -> Result<bool, RepoToolError> {
        let mut state = self.shared.state.lock().unwrap();
        if state.status != "running" {
            return Ok(state.cancel_requested);
        }
        state.cancel_requested = true;
        if let Some(child) = state.child.as_mut() {
            terminate_child_process_tree(child).map_err(RepoToolError::CommandIo)?;
        }
        self.shared.changed.notify_all();
        Ok(true)
    }
}

pub fn start_command_process(
    preview: CommandPreview,
    request: CommandProcessRequest,
) -> Result<CommandProcessHandle, RepoToolError> {
    let permit = reserve_process_slot()?;
    let root = super::canonical_repo_root(&preview.repo_root)?;
    let workdir = resolve_repo_dir(&root, &preview.cwd)?;
    let interactive = request.interactive || request.initial_stdin.is_some();
    let mut command = Command::new(&preview.argv[0]);
    command
        .args(&preview.argv[1..])
        .current_dir(workdir)
        .stdin(if interactive {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_command_process_group(&mut command);
    configure_model_command_environment(&mut command, &request.source);
    let mut child = command.spawn().map_err(RepoToolError::CommandIo)?;
    let process_id = Uuid::new_v4().to_string();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    if let Some(input) = request.initial_stdin.as_deref() {
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(error) = stdin.write_all(input.as_bytes()) {
                if error.kind() != std::io::ErrorKind::BrokenPipe {
                    let _ = terminate_child_process_tree(&mut child);
                    return Err(RepoToolError::CommandIo(error));
                }
            }
        }
    }
    let shared = Arc::new(SharedCommandProcess {
        process_id,
        state: Mutex::new(CommandProcessState {
            preview,
            status: "running".to_owned(),
            child: Some(child),
            output: OutputTail::new(request.max_output_bytes),
            returncode: None,
            timed_out: false,
            cancel_requested: false,
            error: None,
        }),
        changed: Condvar::new(),
    });
    let mut readers = Vec::new();
    if let Some(stdout) = stdout {
        readers.push(spawn_output_reader(shared.clone(), stdout));
    }
    if let Some(stderr) = stderr {
        readers.push(spawn_output_reader(shared.clone(), stderr));
    }
    spawn_process_worker(shared.clone(), request.timeout_seconds, readers, permit);
    Ok(CommandProcessHandle { shared })
}

fn snapshot_state(
    process_id: &str,
    state: &CommandProcessState,
    cursor: Option<u64>,
) -> CommandProcessSnapshot {
    let requested_cursor = cursor.unwrap_or(state.output.start_offset);
    let output = state.output.snapshot(cursor);
    CommandProcessSnapshot {
        process_id: process_id.to_owned(),
        status: state.status.clone(),
        output: String::from_utf8_lossy(&output.bytes).to_string(),
        output_truncated: output.truncated,
        output_cursor: output.start_offset,
        next_output_cursor: output.total_bytes,
        output_gap: requested_cursor < state.output.start_offset,
        returncode: state.returncode,
        timed_out: state.timed_out,
        cancel_requested: state.cancel_requested,
        error: state.error.clone(),
    }
}

fn spawn_output_reader(
    shared: Arc<SharedCommandProcess>,
    mut stream: impl Read + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    shared.state.lock().unwrap().output.append(&buffer[..read]);
                    shared.changed.notify_all();
                }
                Err(error) => {
                    shared.state.lock().unwrap().error = Some(error.to_string());
                    shared.changed.notify_all();
                    break;
                }
            }
        }
    })
}

fn spawn_process_worker(
    shared: Arc<SharedCommandProcess>,
    timeout_seconds: Option<u64>,
    readers: Vec<std::thread::JoinHandle<()>>,
    _permit: CommandProcessPermit,
) {
    std::thread::spawn(move || {
        let timeout_seconds =
            timeout_seconds.map(|value| value.clamp(1, MAX_COMMAND_TIMEOUT_SECONDS));
        let started = Instant::now();
        loop {
            let mut state = shared.state.lock().unwrap();
            let timed_out = timeout_seconds
                .is_some_and(|timeout| started.elapsed() >= Duration::from_secs(timeout));
            if timed_out && !state.timed_out && !state.cancel_requested {
                state.timed_out = true;
                if let Some(child) = state.child.as_mut() {
                    let _ = terminate_child_process_tree(child);
                }
            }
            let Some(child) = state.child.as_mut() else {
                break;
            };
            match child.try_wait() {
                Ok(Some(status)) => {
                    state.returncode = status.code();
                    state.child = None;
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    state.error = Some(error.to_string());
                    state.child = None;
                    break;
                }
            }
            drop(state);
            std::thread::sleep(Duration::from_millis(25));
        }
        for reader in readers {
            let _ = reader.join();
        }
        let mut state = shared.state.lock().unwrap();
        state.status = if state.cancel_requested {
            "cancelled"
        } else if state.timed_out {
            "timeout"
        } else if state.error.is_none() && state.returncode == Some(0) {
            "completed"
        } else {
            "failed"
        }
        .to_owned();
        shared.changed.notify_all();
    });
}

#[cfg(unix)]
fn configure_command_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // SAFETY: pre_exec only calls the async-signal-safe setpgid syscall.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn configure_command_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_child_process_tree(child: &mut Child) -> std::io::Result<()> {
    let result = unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
    if result == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return child.kill();
        }
    }
    Ok(())
}

#[cfg(windows)]
fn terminate_child_process_tree(child: &mut Child) -> std::io::Result<()> {
    let pid = child.id().to_string();
    match Command::new("taskkill")
        .args(["/PID", pid.as_str(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        _ => child.kill(),
    }
}

#[cfg(not(any(unix, windows)))]
fn terminate_child_process_tree(child: &mut Child) -> std::io::Result<()> {
    child.kill()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_tail_returns_only_unobserved_bytes_and_reports_gaps() {
        let mut output = OutputTail::new(4);
        output.append(b"abcdef");
        let first = output.snapshot(Some(0));
        assert_eq!(first.bytes, b"cdef");
        assert_eq!(first.start_offset, 2);
        assert_eq!(first.total_bytes, 6);
        assert!(first.truncated);

        let second = output.snapshot(Some(first.total_bytes));
        assert!(second.bytes.is_empty());
        assert_eq!(second.start_offset, 6);
    }

    #[test]
    fn process_slot_reservation_never_exceeds_the_limit() {
        let counter = AtomicUsize::new(0);
        assert!(try_reserve_process_slot(&counter, 2));
        assert!(try_reserve_process_slot(&counter, 2));
        assert!(!try_reserve_process_slot(&counter, 2));
        assert_eq!(counter.load(Ordering::Acquire), 2);
    }
}
