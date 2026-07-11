use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const DEFAULT_MAX_FILE_BYTES: u64 = 64 * 1024;
pub const DEFAULT_MAX_FILE_RESULTS: usize = 200;
pub const DEFAULT_MAX_SEARCH_MATCHES: usize = 50;
pub const DEFAULT_MAX_GIT_OUTPUT_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_PATCH_BYTES: usize = 256 * 1024;
pub const DEFAULT_MAX_WRITE_FILE_BYTES: usize = 2 * 1024 * 1024;
pub const DEFAULT_COMMAND_TIMEOUT_SECONDS: u64 = 120;
pub const MAX_COMMAND_TIMEOUT_SECONDS: u64 = 600;
// Mirrors Claude Code's EndTruncatingAccumulator default (2 ** 21). This keeps
// command evidence useful for long test/build failures while still bounding
// memory and HTTP payload size.
pub const DEFAULT_MAX_COMMAND_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

const SKIPPED_DIRS: &[&str] = &[
    ".git",
    ".coder",
    ".venv",
    "venv",
    "node_modules",
    "target",
    "dist",
    "build",
    ".cache",
    "__pycache__",
];

const SENSITIVE_FILE_NAMES: &[&str] = &[
    ".env",
    ".local-env.ps1",
    "credentials",
    "id_rsa",
    "id_ed25519",
];
const SENSITIVE_FILE_SUFFIXES: &[&str] = &[".pem", ".p12", ".pfx", ".key"];
const ALWAYS_DENIED_DIRS: &[&str] = &[
    ".git", ".ssh", ".aws", ".kube", ".azure", ".gnupg", ".docker",
];
const SHELL_META_CHARS: &[&str] = &["&&", "||", "|", ";", ">", "<", "$(", "`"];
const HIGH_RISK_COMMAND_TOKENS: &[&str] = &[
    "rm", "del", "rmdir", "format", "sudo", "chmod", "chown", "curl", "wget", "ssh", "scp",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoToolConfig {
    pub max_file_bytes: u64,
    pub max_search_matches: usize,
}

impl Default for RepoToolConfig {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_search_matches: DEFAULT_MAX_SEARCH_MATCHES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoFileEvidence {
    pub path: String,
    pub size_bytes: u64,
    pub content: String,
    pub evidence_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoReadSnippet {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
    pub truncated: bool,
    pub evidence_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoSearchMatch {
    pub path: String,
    pub line: usize,
    pub preview: String,
    pub evidence_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoFileRef {
    pub path: String,
    pub normalized_path: String,
    pub size_bytes: u64,
    pub language: Option<String>,
    pub evidence_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStatusEvidence {
    pub repo_root: String,
    pub porcelain_v1: String,
    pub truncated: bool,
    pub evidence_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitDiffEvidence {
    pub repo_root: String,
    pub preview: String,
    pub truncated: bool,
    pub evidence_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchPreviewEvidence {
    pub repo_root: String,
    pub files: Vec<PatchFilePreview>,
    pub file_count: usize,
    pub hunk_count: usize,
    pub additions: usize,
    pub deletions: usize,
    pub truncated: bool,
    pub evidence_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchFilePreview {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub status: String,
    pub hunks: usize,
    pub additions: usize,
    pub deletions: usize,
    pub target_exists: bool,
}

#[derive(Debug, Clone)]
pub struct PatchApplyRequest {
    pub patch_file: PathBuf,
    pub max_patch_bytes: usize,
    pub source: String,
    pub approved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchApplyEvidence {
    pub repo_root: String,
    pub patch_file: String,
    pub status: String,
    pub applied: bool,
    pub requires_approval: bool,
    pub approval_key: String,
    pub reason: String,
    pub preview: PatchPreviewEvidence,
    pub evidence_kind: String,
}

#[derive(Debug, Clone)]
pub struct FileWriteRequest {
    pub path: PathBuf,
    pub content: String,
    pub max_bytes: usize,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct FileEditRequest {
    pub path: PathBuf,
    pub old_string: String,
    pub new_string: String,
    pub replace_all: bool,
    pub max_bytes: usize,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct FileEditReplacement {
    pub old_string: String,
    pub new_string: String,
    pub replace_all: bool,
}

#[derive(Debug, Clone)]
pub struct FileEditBatchRequest {
    pub path: PathBuf,
    pub edits: Vec<FileEditReplacement>,
    pub max_bytes: usize,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWriteEvidence {
    pub repo_root: String,
    pub path: String,
    pub status: String,
    pub bytes_written: usize,
    pub created: bool,
    pub source: String,
    pub evidence_kind: String,
}

#[derive(Debug, Clone)]
pub struct CommandRunRequest {
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    pub stdin: Option<String>,
    pub timeout_seconds: u64,
    pub max_output_bytes: usize,
    pub source: String,
    pub sandbox: bool,
    pub approved: bool,
}

impl Default for CommandRunRequest {
    fn default() -> Self {
        Self {
            cwd: PathBuf::from("."),
            argv: Vec::new(),
            stdin: None,
            timeout_seconds: DEFAULT_COMMAND_TIMEOUT_SECONDS,
            max_output_bytes: DEFAULT_MAX_COMMAND_OUTPUT_BYTES,
            source: "model".to_owned(),
            sandbox: false,
            approved: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandPolicyDecision {
    pub allowed: bool,
    pub requires_approval: bool,
    pub risk: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRunEvidence {
    pub repo_root: String,
    pub cwd: String,
    pub argv: Vec<String>,
    pub command: String,
    pub status: String,
    pub passed: bool,
    pub blocked: bool,
    pub requires_approval: bool,
    pub approval_key: String,
    pub returncode: Option<i32>,
    pub output: String,
    pub output_truncated: bool,
    pub timed_out: bool,
    pub policy: CommandPolicyDecision,
    pub evidence_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandPreview {
    pub repo_root: String,
    pub cwd: String,
    pub argv: Vec<String>,
    pub command: String,
    pub requires_approval: bool,
    pub approval_key: String,
    pub policy: CommandPolicyDecision,
    pub evidence_kind: String,
}

pub fn read_file(
    repo_root: impl AsRef<Path>,
    requested_path: impl AsRef<Path>,
    config: &RepoToolConfig,
) -> Result<RepoFileEvidence, RepoToolError> {
    let root = canonical_repo_root(repo_root)?;
    let path = resolve_existing_repo_path(&root, requested_path)?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err(RepoToolError::NotAFile(relative_display(&root, &path)));
    }
    let relative_path = relative_display(&root, &path);
    if sensitive_repo_path(&relative_path) {
        return Err(RepoToolError::SensitivePath(relative_path));
    }
    if metadata.len() > config.max_file_bytes {
        return Err(RepoToolError::FileTooLarge {
            path: relative_display(&root, &path),
            size_bytes: metadata.len(),
            max_bytes: config.max_file_bytes,
        });
    }
    let content = fs::read_to_string(&path).map_err(|source| RepoToolError::ReadText {
        path: relative_display(&root, &path),
        source,
    })?;
    Ok(RepoFileEvidence {
        path: relative_display(&root, &path),
        size_bytes: metadata.len(),
        content,
        evidence_kind: "repo_evidence".to_owned(),
    })
}

pub fn read_file_range(
    repo_root: impl AsRef<Path>,
    requested_path: impl AsRef<Path>,
    start_line: usize,
    max_lines: usize,
    max_chars: usize,
) -> Result<RepoReadSnippet, RepoToolError> {
    let root = canonical_repo_root(repo_root)?;
    let path = resolve_existing_repo_path(&root, requested_path)?;
    validate_readable_evidence_path(&root, &path)?;

    let start = start_line.max(1);
    let line_limit = max_lines.clamp(1, 200);
    let char_limit = max_chars.clamp(1, 100_000);
    let last_requested = start + line_limit - 1;
    let relative_path = relative_display(&root, &path);

    let file = fs::File::open(&path)?;
    let mut reader = BufReader::new(file);
    let mut text = String::new();
    let mut chars_used = 0;
    let mut end_line = start;
    let mut truncated = false;

    let mut line_number = 0;
    loop {
        let mut line = String::new();
        let bytes_read = reader
            .read_line(&mut line)
            .map_err(|source| RepoToolError::ReadText {
                path: relative_path.clone(),
                source,
            })?;
        if bytes_read == 0 {
            break;
        }
        line_number += 1;
        if line_number < start {
            continue;
        }
        if line_number > last_requested {
            truncated = true;
            break;
        }
        let remaining = char_limit - chars_used;
        if remaining == 0 {
            truncated = true;
            break;
        }
        let line_chars = line.chars().count();
        if line_chars > remaining {
            text.push_str(&line.chars().take(remaining).collect::<String>());
            end_line = line_number;
            truncated = true;
            break;
        }
        chars_used += line_chars;
        text.push_str(&line);
        end_line = line_number;
    }

    Ok(RepoReadSnippet {
        path: relative_path,
        start_line: start,
        end_line,
        text,
        truncated,
        evidence_kind: "repo_evidence".to_owned(),
    })
}

pub fn find_files(
    repo_root: impl AsRef<Path>,
    query: Option<&str>,
    extensions: &[String],
    max_results: usize,
) -> Result<Vec<RepoFileRef>, RepoToolError> {
    let root = canonical_repo_root(repo_root)?;
    let query = query
        .map(|item| item.trim().to_lowercase())
        .filter(|item| !item.is_empty());
    let extension_filter = normalize_extensions(extensions);
    let mut files = Vec::new();
    let limit = max_results.clamp(1, 1000);
    find_files_in_dir(
        &root,
        &root,
        query.as_deref(),
        &extension_filter,
        limit,
        &mut files,
    )?;
    Ok(files)
}

pub fn search_text(
    repo_root: impl AsRef<Path>,
    query: &str,
    config: &RepoToolConfig,
) -> Result<Vec<RepoSearchMatch>, RepoToolError> {
    if query.trim().is_empty() {
        return Err(RepoToolError::EmptyQuery);
    }
    let root = canonical_repo_root(repo_root)?;
    let mut matches = Vec::new();
    search_dir(&root, &root, query, config, &mut matches)?;
    Ok(matches)
}

pub fn git_status(repo_root: impl AsRef<Path>) -> Result<GitStatusEvidence, RepoToolError> {
    let root = canonical_repo_root(repo_root)?;
    let output = run_git(
        &root,
        &["status", "--porcelain=v1", "--branch"],
        DEFAULT_MAX_GIT_OUTPUT_BYTES,
    )?;
    Ok(GitStatusEvidence {
        repo_root: root.display().to_string(),
        porcelain_v1: output.preview,
        truncated: output.truncated,
        evidence_kind: "repo_evidence".to_owned(),
    })
}

pub fn git_diff(
    repo_root: impl AsRef<Path>,
    max_output_bytes: usize,
) -> Result<GitDiffEvidence, RepoToolError> {
    let root = canonical_repo_root(repo_root)?;
    let output = run_git(
        &root,
        &["diff", "--no-ext-diff", "--no-textconv", "--"],
        max_output_bytes,
    )?;
    Ok(GitDiffEvidence {
        repo_root: root.display().to_string(),
        preview: output.preview,
        truncated: output.truncated,
        evidence_kind: "repo_evidence".to_owned(),
    })
}

pub fn preview_patch_file(
    repo_root: impl AsRef<Path>,
    patch_file: impl AsRef<Path>,
    max_patch_bytes: usize,
) -> Result<PatchPreviewEvidence, RepoToolError> {
    let root = canonical_repo_root(repo_root)?;
    let path = resolve_existing_repo_path(&root, patch_file)?;
    let relative_patch = validate_readable_evidence_path(&root, &path)?;
    let limit = max_patch_bytes.clamp(1, DEFAULT_MAX_PATCH_BYTES);
    let mut file = fs::File::open(&path)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > limit;
    if truncated {
        bytes.truncate(limit);
    }
    let patch_text = String::from_utf8_lossy(&bytes).into_owned();
    let mut evidence = preview_patch_text(&root, &patch_text, truncated)?;
    evidence.repo_root = root.display().to_string();
    if evidence.files.is_empty() {
        return Err(RepoToolError::PatchNoFiles(relative_patch));
    }
    Ok(evidence)
}

pub fn apply_patch_file(
    repo_root: impl AsRef<Path>,
    request: PatchApplyRequest,
) -> Result<PatchApplyEvidence, RepoToolError> {
    let root = canonical_repo_root(repo_root)?;
    let patch_path = resolve_existing_repo_path(&root, &request.patch_file)?;
    let relative_patch = validate_readable_evidence_path(&root, &patch_path)?;
    let preview = preview_patch_file(&root, &request.patch_file, request.max_patch_bytes)?;
    let patch_arg = PathBuf::from(&relative_patch);
    let approval_key = patch_approval_key(&relative_patch, &preview);
    if request.source == "model" && !request.approved {
        return Ok(PatchApplyEvidence {
            repo_root: root.display().to_string(),
            patch_file: relative_patch,
            status: "blocked".to_owned(),
            applied: false,
            requires_approval: true,
            approval_key,
            reason: "Model-generated patch apply requires approval.".to_owned(),
            preview,
            evidence_kind: "patch_apply".to_owned(),
        });
    }

    if let Err(error) = run_git_apply(&root, &patch_arg, true) {
        return Ok(PatchApplyEvidence {
            repo_root: root.display().to_string(),
            patch_file: relative_patch,
            status: "failed".to_owned(),
            applied: false,
            requires_approval: false,
            approval_key,
            reason: error.to_string(),
            preview,
            evidence_kind: "patch_apply".to_owned(),
        });
    }
    if let Err(error) = run_git_apply(&root, &patch_arg, false) {
        return Ok(PatchApplyEvidence {
            repo_root: root.display().to_string(),
            patch_file: relative_patch,
            status: "failed".to_owned(),
            applied: false,
            requires_approval: false,
            approval_key,
            reason: error.to_string(),
            preview,
            evidence_kind: "patch_apply".to_owned(),
        });
    }

    Ok(PatchApplyEvidence {
        repo_root: root.display().to_string(),
        patch_file: relative_patch,
        status: "applied".to_owned(),
        applied: true,
        requires_approval: false,
        approval_key,
        reason: String::new(),
        preview,
        evidence_kind: "patch_apply".to_owned(),
    })
}

pub fn write_text_file(
    repo_root: impl AsRef<Path>,
    request: FileWriteRequest,
) -> Result<FileWriteEvidence, RepoToolError> {
    let root = canonical_repo_root(repo_root)?;
    let (path, relative_path) = resolve_repo_write_path(&root, &request.path)?;
    let max_bytes = request.max_bytes.clamp(1, DEFAULT_MAX_WRITE_FILE_BYTES);
    let bytes = request.content.as_bytes();
    if bytes.len() > max_bytes {
        return Err(RepoToolError::FileTooLarge {
            path: relative_path,
            size_bytes: bytes.len() as u64,
            max_bytes: max_bytes as u64,
        });
    }
    let created = !path.exists();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, bytes)?;
    Ok(FileWriteEvidence {
        repo_root: root.display().to_string(),
        path: relative_path,
        status: "written".to_owned(),
        bytes_written: bytes.len(),
        created,
        source: request.source,
        evidence_kind: "file_write".to_owned(),
    })
}

pub fn edit_text_file(
    repo_root: impl AsRef<Path>,
    request: FileEditRequest,
) -> Result<FileWriteEvidence, RepoToolError> {
    edit_text_file_batch(
        repo_root,
        FileEditBatchRequest {
            path: request.path,
            edits: vec![FileEditReplacement {
                old_string: request.old_string,
                new_string: request.new_string,
                replace_all: request.replace_all,
            }],
            max_bytes: request.max_bytes,
            source: request.source,
        },
    )
}

pub fn edit_text_file_batch(
    repo_root: impl AsRef<Path>,
    request: FileEditBatchRequest,
) -> Result<FileWriteEvidence, RepoToolError> {
    let root = canonical_repo_root(repo_root)?;
    let path = resolve_existing_repo_path(&root, &request.path)?;
    let relative_path = validate_readable_evidence_path(&root, &path)?;
    let max_bytes = request.max_bytes.clamp(1, DEFAULT_MAX_WRITE_FILE_BYTES);
    let metadata = fs::metadata(&path)?;
    if metadata.len() > max_bytes as u64 {
        return Err(RepoToolError::FileTooLarge {
            path: relative_path,
            size_bytes: metadata.len(),
            max_bytes: max_bytes as u64,
        });
    }
    if request.edits.is_empty() {
        return Err(RepoToolError::EditNoOperations(relative_path));
    }
    let content = fs::read_to_string(&path).map_err(|source| RepoToolError::ReadText {
        path: relative_path.clone(),
        source,
    })?;
    let mut edited = content.clone();
    for edit in request.edits {
        if edit.old_string.is_empty() {
            return Err(RepoToolError::EditEmptyOldString(relative_path));
        }
        if edit.old_string == edit.new_string {
            return Err(RepoToolError::EditNoChange(relative_path));
        }
        let matches = edited.match_indices(&edit.old_string).count();
        if matches == 0 {
            return Err(RepoToolError::EditStringNotFound(relative_path));
        }
        if matches > 1 && !edit.replace_all {
            return Err(RepoToolError::EditStringNotUnique {
                path: relative_path,
                matches,
            });
        }
        edited = if edit.replace_all {
            edited.replace(&edit.old_string, &edit.new_string)
        } else {
            edited.replacen(&edit.old_string, &edit.new_string, 1)
        };
    }
    if edited == content {
        return Err(RepoToolError::EditNoChange(relative_path));
    }
    if edited.len() > max_bytes {
        return Err(RepoToolError::FileTooLarge {
            path: relative_path,
            size_bytes: edited.len() as u64,
            max_bytes: max_bytes as u64,
        });
    }
    fs::write(&path, edited.as_bytes())?;
    Ok(FileWriteEvidence {
        repo_root: root.display().to_string(),
        path: relative_path,
        status: "written".to_owned(),
        bytes_written: edited.len(),
        created: false,
        source: request.source,
        evidence_kind: "file_edit".to_owned(),
    })
}

pub fn run_command(
    repo_root: impl AsRef<Path>,
    request: CommandRunRequest,
) -> Result<CommandRunEvidence, RepoToolError> {
    if request.argv.is_empty() || request.argv.iter().any(|item| item.trim().is_empty()) {
        return Err(RepoToolError::EmptyCommandArgv);
    }
    let root = canonical_repo_root(repo_root)?;
    let workdir = resolve_repo_dir(&root, &request.cwd)?;
    let cwd = relative_dir_display(&root, &workdir);
    let command_text = request.argv.join(" ");
    let policy = evaluate_command_policy(&request.argv, &request.source, request.sandbox);
    let approval_key = command_approval_key(&command_text, &cwd);
    if policy.requires_approval && !request.approved {
        return Ok(CommandRunEvidence {
            repo_root: root.display().to_string(),
            cwd,
            argv: request.argv,
            command: command_text.clone(),
            status: "blocked".to_owned(),
            passed: false,
            blocked: true,
            requires_approval: true,
            approval_key,
            returncode: None,
            output: format!("Check command requires explicit approval: {command_text}"),
            output_truncated: false,
            timed_out: false,
            policy,
            evidence_kind: "command_evidence".to_owned(),
        });
    }

    let timeout = effective_command_timeout_seconds(request.timeout_seconds);
    let max_output_bytes = request
        .max_output_bytes
        .clamp(1, DEFAULT_MAX_COMMAND_OUTPUT_BYTES);
    let stdin_mode = if request.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let mut child = Command::new(&request.argv[0])
        .args(&request.argv[1..])
        .current_dir(&workdir)
        .stdin(stdin_mode)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RepoToolError::CommandIo)?;
    if let Some(stdin_text) = request.stdin.as_deref() {
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(error) = stdin.write_all(stdin_text.as_bytes()) {
                if error.kind() != std::io::ErrorKind::BrokenPipe {
                    return Err(RepoToolError::CommandIo(error));
                }
            }
        }
    }
    let started = Instant::now();
    let mut timed_out = false;
    loop {
        if child
            .try_wait()
            .map_err(RepoToolError::CommandIo)?
            .is_some()
        {
            break;
        }
        if started.elapsed() >= Duration::from_secs(timeout) {
            timed_out = true;
            let _ = child.kill();
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().map_err(RepoToolError::CommandIo)?;
    let returncode = output.status.code();
    let mut combined = output.stdout;
    combined.extend_from_slice(&output.stderr);
    let (output_text, output_truncated) = bounded_tail_text(&combined, max_output_bytes);
    let passed = !timed_out && output.status.success();
    let status = if timed_out {
        "timeout"
    } else if passed {
        "completed"
    } else {
        "failed"
    };

    Ok(CommandRunEvidence {
        repo_root: root.display().to_string(),
        cwd,
        argv: request.argv,
        command: command_text,
        status: status.to_owned(),
        passed,
        blocked: false,
        requires_approval: false,
        approval_key,
        returncode,
        output: output_text,
        output_truncated,
        timed_out,
        policy,
        evidence_kind: "command_evidence".to_owned(),
    })
}

fn effective_command_timeout_seconds(requested: u64) -> u64 {
    requested.clamp(1, MAX_COMMAND_TIMEOUT_SECONDS)
}

pub fn preview_command(
    repo_root: impl AsRef<Path>,
    cwd: impl AsRef<Path>,
    argv: Vec<String>,
    source: &str,
    sandbox: bool,
) -> Result<CommandPreview, RepoToolError> {
    if argv.is_empty() || argv.iter().any(|item| item.trim().is_empty()) {
        return Err(RepoToolError::EmptyCommandArgv);
    }
    let root = canonical_repo_root(repo_root)?;
    let workdir = resolve_repo_dir(&root, cwd)?;
    let cwd = relative_dir_display(&root, &workdir);
    let command = argv.join(" ");
    let policy = evaluate_command_policy(&argv, source, sandbox);
    let approval_key = command_approval_key(&command, &cwd);
    Ok(CommandPreview {
        repo_root: root.display().to_string(),
        cwd,
        argv,
        command,
        requires_approval: policy.requires_approval,
        approval_key,
        policy,
        evidence_kind: "command_preview".to_owned(),
    })
}

pub fn evaluate_command_policy(
    argv: &[String],
    source: &str,
    sandbox: bool,
) -> CommandPolicyDecision {
    let text = argv.join(" ");
    let lower = text.to_lowercase();
    if contains_high_risk_command(&lower) {
        return CommandPolicyDecision {
            allowed: true,
            requires_approval: true,
            risk: "high".to_owned(),
            reason: "Command contains high-risk token.".to_owned(),
        };
    }
    if SHELL_META_CHARS.iter().any(|meta| text.contains(meta)) {
        return CommandPolicyDecision {
            allowed: true,
            requires_approval: !sandbox,
            risk: "medium".to_owned(),
            reason: "Shell-like command boundary requires approval outside sandbox.".to_owned(),
        };
    }
    if source == "model" && !sandbox {
        return CommandPolicyDecision {
            allowed: true,
            requires_approval: true,
            risk: "medium".to_owned(),
            reason: "Model-generated command requires approval outside sandbox.".to_owned(),
        };
    }
    CommandPolicyDecision {
        allowed: true,
        requires_approval: false,
        risk: "low".to_owned(),
        reason: String::new(),
    }
}

pub fn command_approval_key(command: &str, cwd: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cwd.as_bytes());
    hasher.update([0]);
    hasher.update(command.as_bytes());
    format!("cmd:{:x}", hasher.finalize())
}

pub fn patch_approval_key(patch_file: &str, preview: &PatchPreviewEvidence) -> String {
    let mut hasher = Sha256::new();
    hasher.update(patch_file.as_bytes());
    hasher.update([0]);
    for file in &preview.files {
        if let Some(path) = &file.old_path {
            hasher.update(path.as_bytes());
        }
        hasher.update([0]);
        if let Some(path) = &file.new_path {
            hasher.update(path.as_bytes());
        }
        hasher.update([0]);
        hasher.update(file.status.as_bytes());
        hasher.update([0]);
    }
    hasher.update(preview.additions.to_string().as_bytes());
    hasher.update([0]);
    hasher.update(preview.deletions.to_string().as_bytes());
    format!("patch:{:x}", hasher.finalize())
}

fn preview_patch_text(
    root: &Path,
    patch_text: &str,
    truncated: bool,
) -> Result<PatchPreviewEvidence, RepoToolError> {
    let mut files = Vec::new();
    let mut current: Option<PatchFilePreview> = None;
    for line in patch_text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(file) = current.take() {
                files.push(file);
            }
            let paths = rest.split_whitespace().collect::<Vec<_>>();
            current = Some(PatchFilePreview::from_paths(
                root,
                paths.first().copied(),
                paths.get(1).copied(),
            )?);
            continue;
        }
        if let Some(path) = line.strip_prefix("--- ") {
            let file = current.get_or_insert_with(PatchFilePreview::empty);
            file.set_old_path(root, path)?;
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            let file = current.get_or_insert_with(PatchFilePreview::empty);
            file.set_new_path(root, path)?;
            continue;
        }
        if line.starts_with("@@") {
            let file = current.get_or_insert_with(PatchFilePreview::empty);
            file.hunks += 1;
            continue;
        }
        if line.starts_with('+') && !line.starts_with("+++") {
            if let Some(file) = current.as_mut() {
                file.additions += 1;
            }
            continue;
        }
        if line.starts_with('-') && !line.starts_with("---") {
            if let Some(file) = current.as_mut() {
                file.deletions += 1;
            }
        }
    }
    if let Some(file) = current.take() {
        files.push(file);
    }
    for file in &mut files {
        file.finish_status(root)?;
    }
    let hunk_count = files.iter().map(|file| file.hunks).sum();
    let additions = files.iter().map(|file| file.additions).sum();
    let deletions = files.iter().map(|file| file.deletions).sum();
    Ok(PatchPreviewEvidence {
        repo_root: root.display().to_string(),
        file_count: files.len(),
        files,
        hunk_count,
        additions,
        deletions,
        truncated,
        evidence_kind: "repo_evidence".to_owned(),
    })
}

fn find_files_in_dir(
    root: &Path,
    dir: &Path,
    query: Option<&str>,
    extension_filter: &[String],
    limit: usize,
    files: &mut Vec<RepoFileRef>,
) -> Result<(), RepoToolError> {
    if files.len() >= limit {
        return Ok(());
    }
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        if files.len() >= limit {
            break;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            if should_skip_dir(&path) {
                continue;
            }
            find_files_in_dir(root, &path, query, extension_filter, limit, files)?;
            continue;
        }
        if file_type.is_file() {
            let relative_path = relative_display(root, &path);
            if sensitive_repo_path(&relative_path) {
                continue;
            }
            if let Some(query) = query {
                if !relative_path.to_lowercase().contains(query) {
                    continue;
                }
            }
            if !extension_filter.is_empty() {
                let suffix = path
                    .extension()
                    .and_then(|item| item.to_str())
                    .map(|item| format!(".{}", item.to_lowercase()))
                    .unwrap_or_default();
                if !extension_filter.contains(&suffix) {
                    continue;
                }
            }
            let metadata = fs::metadata(&path)?;
            files.push(RepoFileRef {
                path: relative_path.clone(),
                normalized_path: relative_path.clone(),
                size_bytes: metadata.len(),
                language: language_for_path(&relative_path).map(str::to_owned),
                evidence_kind: "repo_evidence".to_owned(),
            });
        }
    }
    Ok(())
}

fn search_dir(
    root: &Path,
    dir: &Path,
    query: &str,
    config: &RepoToolConfig,
    matches: &mut Vec<RepoSearchMatch>,
) -> Result<(), RepoToolError> {
    if matches.len() >= config.max_search_matches {
        return Ok(());
    }
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        if matches.len() >= config.max_search_matches {
            break;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            if should_skip_dir(&path) {
                continue;
            }
            search_dir(root, &path, query, config, matches)?;
            continue;
        }
        if file_type.is_file() {
            search_file(root, &path, query, config, matches)?;
        }
    }
    Ok(())
}

fn search_file(
    root: &Path,
    path: &Path,
    query: &str,
    config: &RepoToolConfig,
    matches: &mut Vec<RepoSearchMatch>,
) -> Result<(), RepoToolError> {
    if sensitive_repo_path(&relative_display(root, path)) {
        return Ok(());
    }
    let metadata = fs::metadata(path)?;
    if metadata.len() > config.max_file_bytes {
        return Ok(());
    }
    let Ok(content) = fs::read_to_string(path) else {
        return Ok(());
    };
    for (index, line) in content.lines().enumerate() {
        if matches.len() >= config.max_search_matches {
            break;
        }
        if line.contains(query) {
            matches.push(RepoSearchMatch {
                path: relative_display(root, path),
                line: index + 1,
                preview: line.trim().to_owned(),
                evidence_kind: "repo_evidence".to_owned(),
            });
        }
    }
    Ok(())
}

struct CommandOutputPreview {
    preview: String,
    truncated: bool,
}

impl PatchFilePreview {
    fn empty() -> Self {
        Self {
            old_path: None,
            new_path: None,
            status: "modified".to_owned(),
            hunks: 0,
            additions: 0,
            deletions: 0,
            target_exists: false,
        }
    }

    fn from_paths(
        root: &Path,
        old_path: Option<&str>,
        new_path: Option<&str>,
    ) -> Result<Self, RepoToolError> {
        let mut file = Self::empty();
        if let Some(old_path) = old_path {
            file.set_old_path(root, old_path)?;
        }
        if let Some(new_path) = new_path {
            file.set_new_path(root, new_path)?;
        }
        Ok(file)
    }

    fn set_old_path(&mut self, root: &Path, path: &str) -> Result<(), RepoToolError> {
        self.old_path = normalize_patch_path(root, path)?;
        Ok(())
    }

    fn set_new_path(&mut self, root: &Path, path: &str) -> Result<(), RepoToolError> {
        self.new_path = normalize_patch_path(root, path)?;
        Ok(())
    }

    fn finish_status(&mut self, root: &Path) -> Result<(), RepoToolError> {
        self.status = match (&self.old_path, &self.new_path) {
            (None, Some(_)) => "added",
            (Some(_), None) => "deleted",
            (Some(old_path), Some(new_path)) if old_path != new_path => "renamed",
            _ => "modified",
        }
        .to_owned();
        let target = self.new_path.as_ref().or(self.old_path.as_ref());
        self.target_exists = target.map(|path| root.join(path).exists()).unwrap_or(false);
        Ok(())
    }
}

fn run_git(
    root: &Path,
    args: &[&str],
    max_output_bytes: usize,
) -> Result<CommandOutputPreview, RepoToolError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("-c")
        .arg("diff.external=")
        .arg("-c")
        .arg("core.pager=")
        .args(args)
        .output()
        .map_err(RepoToolError::GitIo)?;
    if !output.status.success() {
        return Err(RepoToolError::GitFailed {
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let truncated = output.stdout.len() > max_output_bytes;
    let preview_bytes = if truncated {
        &output.stdout[..max_output_bytes]
    } else {
        &output.stdout
    };
    Ok(CommandOutputPreview {
        preview: String::from_utf8_lossy(preview_bytes).into_owned(),
        truncated,
    })
}

fn run_git_apply(root: &Path, patch_path: &Path, check: bool) -> Result<(), RepoToolError> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .arg("apply")
        .arg("--whitespace=nowarn");
    if check {
        command.arg("--check");
    }
    let output = command
        .arg("--")
        .arg(patch_path)
        .output()
        .map_err(RepoToolError::GitIo)?;
    if !output.status.success() {
        return Err(RepoToolError::GitFailed {
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

fn contains_high_risk_command(lower: &str) -> bool {
    lower
        .split_whitespace()
        .any(|token| HIGH_RISK_COMMAND_TOKENS.contains(&token))
}

fn bounded_tail_text(bytes: &[u8], max_bytes: usize) -> (String, bool) {
    let truncated = bytes.len() > max_bytes;
    let slice = if truncated {
        &bytes[bytes.len() - max_bytes..]
    } else {
        bytes
    };
    (String::from_utf8_lossy(slice).into_owned(), truncated)
}

fn canonical_repo_root(repo_root: impl AsRef<Path>) -> Result<PathBuf, RepoToolError> {
    let root =
        fs::canonicalize(repo_root.as_ref()).map_err(|source| RepoToolError::InvalidRoot {
            path: repo_root.as_ref().display().to_string(),
            source,
        })?;
    if !root.is_dir() {
        return Err(RepoToolError::InvalidRootKind(root.display().to_string()));
    }
    Ok(root)
}

fn resolve_repo_dir(
    root: &Path,
    requested_path: impl AsRef<Path>,
) -> Result<PathBuf, RepoToolError> {
    let requested = requested_path.as_ref();
    if requested.is_absolute() {
        return Err(RepoToolError::PathOutsideRepo(
            requested.display().to_string(),
        ));
    }
    let resolved =
        fs::canonicalize(root.join(requested)).map_err(|source| RepoToolError::PathNotFound {
            path: requested.display().to_string(),
            source,
        })?;
    if !resolved.starts_with(root) {
        return Err(RepoToolError::PathOutsideRepo(
            requested.display().to_string(),
        ));
    }
    if !resolved.is_dir() {
        return Err(RepoToolError::NotADirectory(relative_display(
            root, &resolved,
        )));
    }
    Ok(resolved)
}

fn normalize_patch_path(root: &Path, raw_path: &str) -> Result<Option<String>, RepoToolError> {
    let trimmed = raw_path.trim().trim_matches('"');
    if trimmed == "/dev/null" {
        return Ok(None);
    }
    let without_prefix = trimmed
        .strip_prefix("a/")
        .or_else(|| trimmed.strip_prefix("b/"))
        .unwrap_or(trimmed);
    let path = Path::new(without_prefix);
    if path.is_absolute() {
        return Err(RepoToolError::PathOutsideRepo(without_prefix.to_owned()));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => {
                parts.push(part.to_string_lossy().to_string());
            }
            std::path::Component::CurDir => {}
            _ => {
                return Err(RepoToolError::PathOutsideRepo(without_prefix.to_owned()));
            }
        }
    }
    if parts.is_empty() {
        return Err(RepoToolError::PathOutsideRepo(without_prefix.to_owned()));
    }
    let normalized = parts.join("/");
    if sensitive_repo_path(&normalized) {
        return Err(RepoToolError::SensitivePath(normalized));
    }
    let candidate = root.join(&normalized);
    if !candidate.starts_with(root) {
        return Err(RepoToolError::PathOutsideRepo(normalized));
    }
    Ok(Some(normalized))
}

fn relative_dir_display(root: &Path, path: &Path) -> String {
    let relative = relative_display(root, path);
    if relative.is_empty() {
        ".".to_owned()
    } else {
        relative
    }
}

fn resolve_existing_repo_path(
    root: &Path,
    requested_path: impl AsRef<Path>,
) -> Result<PathBuf, RepoToolError> {
    let requested = requested_path.as_ref();
    if requested.is_absolute() {
        return Err(RepoToolError::PathOutsideRepo(
            requested.display().to_string(),
        ));
    }
    let resolved =
        fs::canonicalize(root.join(requested)).map_err(|source| RepoToolError::PathNotFound {
            path: requested.display().to_string(),
            source,
        })?;
    if !resolved.starts_with(root) {
        return Err(RepoToolError::PathOutsideRepo(
            requested.display().to_string(),
        ));
    }
    Ok(resolved)
}

fn resolve_repo_write_path(
    root: &Path,
    requested_path: impl AsRef<Path>,
) -> Result<(PathBuf, String), RepoToolError> {
    let requested = requested_path.as_ref();
    if requested.is_absolute() {
        return Err(RepoToolError::PathOutsideRepo(
            requested.display().to_string(),
        ));
    }

    let mut parts = Vec::new();
    for component in requested.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            Component::CurDir => {}
            _ => {
                return Err(RepoToolError::PathOutsideRepo(
                    requested.display().to_string(),
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(RepoToolError::PathOutsideRepo(
            requested.display().to_string(),
        ));
    }
    let relative_path = parts.join("/");
    if sensitive_repo_path(&relative_path) {
        return Err(RepoToolError::SensitivePath(relative_path));
    }
    let resolved = root.join(&relative_path);
    if !resolved.starts_with(root) {
        return Err(RepoToolError::PathOutsideRepo(relative_path));
    }
    if resolved.exists() && !resolved.is_file() {
        return Err(RepoToolError::NotAFile(relative_path));
    }
    Ok((resolved, relative_path))
}

fn validate_readable_evidence_path(root: &Path, path: &Path) -> Result<String, RepoToolError> {
    let metadata = fs::metadata(path)?;
    let relative_path = relative_display(root, path);
    if !metadata.is_file() {
        return Err(RepoToolError::NotAFile(relative_path));
    }
    if sensitive_repo_path(&relative_path) {
        return Err(RepoToolError::SensitivePath(relative_path));
    }
    let mut file = fs::File::open(path)?;
    let mut sample = [0_u8; 4096];
    let bytes_read = file.read(&mut sample)?;
    if sample[..bytes_read].contains(&0) {
        return Err(RepoToolError::BinaryFile(relative_path));
    }
    Ok(relative_path)
}

fn should_skip_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            let lower = name.to_lowercase();
            SKIPPED_DIRS.contains(&lower.as_str()) || ALWAYS_DENIED_DIRS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

fn sensitive_repo_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_lowercase();
    let parts = normalized.split('/').collect::<Vec<_>>();
    if parts.iter().any(|part| ALWAYS_DENIED_DIRS.contains(part)) {
        return true;
    }
    let Some(name) = parts.last() else {
        return false;
    };
    if SENSITIVE_FILE_NAMES.contains(name) || name.starts_with(".env.") {
        return true;
    }
    if SENSITIVE_FILE_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return true;
    }
    name.contains("private_key")
        || name.contains("private-key")
        || name.contains("secret_key")
        || name.contains("secret-key")
}

fn normalize_extensions(extensions: &[String]) -> Vec<String> {
    extensions
        .iter()
        .map(|item| item.trim().to_lowercase())
        .filter(|item| !item.is_empty())
        .map(|item| {
            if item.starts_with('.') {
                item
            } else {
                format!(".{item}")
            }
        })
        .collect()
}

fn language_for_path(path: &str) -> Option<&'static str> {
    match Path::new(path)
        .extension()
        .and_then(|item| item.to_str())
        .map(|item| item.to_lowercase())
        .as_deref()
    {
        Some("py") => Some("python"),
        Some("ts") => Some("typescript"),
        Some("tsx") => Some("typescriptreact"),
        Some("js") => Some("javascript"),
        Some("jsx") => Some("javascriptreact"),
        Some("md") => Some("markdown"),
        Some("json") => Some("json"),
        Some("yml") | Some("yaml") => Some("yaml"),
        Some("rs") => Some("rust"),
        Some("toml") => Some("toml"),
        _ => None,
    }
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[derive(Debug, Error)]
pub enum RepoToolError {
    #[error("invalid repo root {path}: {source}")]
    InvalidRoot {
        path: String,
        source: std::io::Error,
    },
    #[error("repo root is not a directory: {0}")]
    InvalidRootKind(String),
    #[error("path not found in repo {path}: {source}")]
    PathNotFound {
        path: String,
        source: std::io::Error,
    },
    #[error("path escapes repo root: {0}")]
    PathOutsideRepo(String),
    #[error("path is not a file: {0}")]
    NotAFile(String),
    #[error("path is not a directory: {0}")]
    NotADirectory(String),
    #[error("path is sensitive and cannot be read as repo evidence: {0}")]
    SensitivePath(String),
    #[error("binary files cannot be read as repo evidence: {0}")]
    BinaryFile(String),
    #[error("file {path} is {size_bytes} bytes, over limit {max_bytes}")]
    FileTooLarge {
        path: String,
        size_bytes: u64,
        max_bytes: u64,
    },
    #[error("cannot edit {0}: old_string must not be empty")]
    EditEmptyOldString(String),
    #[error("cannot edit {0}: at least one edit is required")]
    EditNoOperations(String),
    #[error("cannot edit {0}: old_string and new_string are identical")]
    EditNoChange(String),
    #[error("cannot edit {0}: old_string was not found")]
    EditStringNotFound(String),
    #[error("cannot edit {path}: old_string matched {matches} times; provide unique context or set replace_all=true")]
    EditStringNotUnique { path: String, matches: usize },
    #[error("failed to read text from {path}: {source}")]
    ReadText {
        path: String,
        source: std::io::Error,
    },
    #[error("search query must not be empty")]
    EmptyQuery,
    #[error("failed to run git: {0}")]
    GitIo(std::io::Error),
    #[error("git command failed with status {status:?}: {stderr}")]
    GitFailed { status: Option<i32>, stderr: String },
    #[error("patch file has no unified diff file entries: {0}")]
    PatchNoFiles(String),
    #[error("command argv must contain at least one non-empty argument")]
    EmptyCommandArgv,
    #[error("failed to run command: {0}")]
    CommandIo(std::io::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_file_returns_repo_evidence() {
        let root = temp_repo();
        fs::write(root.join("src.txt"), "hello repo").unwrap();

        let evidence = read_file(&root, "src.txt", &RepoToolConfig::default()).unwrap();

        assert_eq!(evidence.path, "src.txt");
        assert_eq!(evidence.content, "hello repo");
        assert_eq!(evidence.evidence_kind, "repo_evidence");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_file_rejects_path_escape() {
        let root = temp_repo();
        let outside = root.parent().unwrap().join("outside.txt");
        fs::write(&outside, "outside").unwrap();

        let error = read_file(&root, "../outside.txt", &RepoToolConfig::default()).unwrap_err();

        assert!(matches!(error, RepoToolError::PathOutsideRepo(_)));
        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_text_file_creates_repo_file() {
        let root = temp_repo();

        let evidence = write_text_file(
            &root,
            FileWriteRequest {
                path: PathBuf::from("docs/readme.md"),
                content: "hello\n".to_owned(),
                max_bytes: DEFAULT_MAX_WRITE_FILE_BYTES,
                source: "test".to_owned(),
            },
        )
        .unwrap();

        assert_eq!(evidence.path, "docs/readme.md");
        assert!(evidence.created);
        assert_eq!(evidence.status, "written");
        assert_eq!(
            fs::read_to_string(root.join("docs").join("readme.md")).unwrap(),
            "hello\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_text_file_rejects_escape_and_sensitive_paths() {
        let root = temp_repo();

        let escaped = write_text_file(
            &root,
            FileWriteRequest {
                path: PathBuf::from("../outside.txt"),
                content: "nope".to_owned(),
                max_bytes: DEFAULT_MAX_WRITE_FILE_BYTES,
                source: "test".to_owned(),
            },
        )
        .unwrap_err();
        let sensitive = write_text_file(
            &root,
            FileWriteRequest {
                path: PathBuf::from(".env"),
                content: "SECRET=value".to_owned(),
                max_bytes: DEFAULT_MAX_WRITE_FILE_BYTES,
                source: "test".to_owned(),
            },
        )
        .unwrap_err();

        assert!(matches!(escaped, RepoToolError::PathOutsideRepo(_)));
        assert!(matches!(sensitive, RepoToolError::SensitivePath(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn edit_text_file_replaces_one_exact_unique_string() {
        let root = temp_repo();
        fs::write(
            root.join("main.js"),
            "const before = true;\nconst plantType = 'sunflower';\nconst after = true;\n",
        )
        .unwrap();

        let evidence = edit_text_file(
            &root,
            FileEditRequest {
                path: PathBuf::from("main.js"),
                old_string: "const plantType = 'sunflower';".to_owned(),
                new_string: "const plantType = 's';".to_owned(),
                replace_all: false,
                max_bytes: DEFAULT_MAX_WRITE_FILE_BYTES,
                source: "test".to_owned(),
            },
        )
        .unwrap();

        assert!(!evidence.created);
        assert_eq!(evidence.evidence_kind, "file_edit");
        assert_eq!(
            fs::read_to_string(root.join("main.js")).unwrap(),
            "const before = true;\nconst plantType = 's';\nconst after = true;\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn edit_text_file_requires_unique_context_unless_replace_all_is_set() {
        let root = temp_repo();
        fs::write(root.join("main.js"), "plant();\nplant();\n").unwrap();
        let request = || FileEditRequest {
            path: PathBuf::from("main.js"),
            old_string: "plant();".to_owned(),
            new_string: "grow();".to_owned(),
            replace_all: false,
            max_bytes: DEFAULT_MAX_WRITE_FILE_BYTES,
            source: "test".to_owned(),
        };

        let error = edit_text_file(&root, request()).unwrap_err();
        assert!(matches!(
            error,
            RepoToolError::EditStringNotUnique { matches: 2, .. }
        ));

        let mut replace_all = request();
        replace_all.replace_all = true;
        edit_text_file(&root, replace_all).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("main.js")).unwrap(),
            "grow();\ngrow();\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn edit_text_file_batch_applies_sequential_edits_with_one_write() {
        let root = temp_repo();
        fs::write(root.join("main.js"), "const value = 1;\nshow(value);\n").unwrap();

        let evidence = edit_text_file_batch(
            &root,
            FileEditBatchRequest {
                path: PathBuf::from("main.js"),
                edits: vec![
                    FileEditReplacement {
                        old_string: "const value = 1;".to_owned(),
                        new_string: "const score = 2;".to_owned(),
                        replace_all: false,
                    },
                    FileEditReplacement {
                        old_string: "show(value);".to_owned(),
                        new_string: "show(score);".to_owned(),
                        replace_all: false,
                    },
                ],
                max_bytes: 1024,
                source: "test".to_owned(),
            },
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(root.join("main.js")).unwrap(),
            "const score = 2;\nshow(score);\n"
        );
        assert_eq!(evidence.evidence_kind, "file_edit");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn edit_text_file_batch_is_atomic_when_a_later_edit_fails() {
        let root = temp_repo();
        let original = "const value = 1;\nshow(value);\n";
        fs::write(root.join("main.js"), original).unwrap();

        let error = edit_text_file_batch(
            &root,
            FileEditBatchRequest {
                path: PathBuf::from("main.js"),
                edits: vec![
                    FileEditReplacement {
                        old_string: "const value = 1;".to_owned(),
                        new_string: "const score = 2;".to_owned(),
                        replace_all: false,
                    },
                    FileEditReplacement {
                        old_string: "missing();".to_owned(),
                        new_string: "show(score);".to_owned(),
                        replace_all: false,
                    },
                ],
                max_bytes: 1024,
                source: "test".to_owned(),
            },
        )
        .unwrap_err();

        assert!(matches!(error, RepoToolError::EditStringNotFound(_)));
        assert_eq!(fs::read_to_string(root.join("main.js")).unwrap(), original);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_file_rejects_large_files() {
        let root = temp_repo();
        fs::write(root.join("large.txt"), "123456").unwrap();
        let config = RepoToolConfig {
            max_file_bytes: 3,
            max_search_matches: 10,
        };

        let error = read_file(&root, "large.txt", &config).unwrap_err();

        assert!(matches!(error, RepoToolError::FileTooLarge { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_file_range_returns_line_refs() {
        let root = temp_repo();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("app.py"), "one\ntwo\nthree\n").unwrap();

        let snippet = read_file_range(&root, "src/app.py", 2, 1, 16_000).unwrap();

        assert_eq!(snippet.path, "src/app.py");
        assert_eq!(snippet.start_line, 2);
        assert_eq!(snippet.end_line, 2);
        assert_eq!(snippet.text, "two\n");
        assert!(snippet.truncated);
        assert_eq!(snippet.evidence_kind, "repo_evidence");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_file_range_reports_line_and_char_truncation() {
        let root = temp_repo();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("app.py"), "one\ntwo\nthree\n").unwrap();
        fs::write(root.join("src").join("unicode.txt"), "abc\u{e9}def\n").unwrap();

        let by_lines = read_file_range(&root, "src/app.py", 1, 2, 16_000).unwrap();
        let by_chars = read_file_range(&root, "src/unicode.txt", 1, 120, 4).unwrap();

        assert_eq!(by_lines.end_line, 2);
        assert!(by_lines.truncated);
        assert_eq!(by_chars.text, "abc\u{e9}");
        assert!(by_chars.truncated);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_file_range_rejects_sensitive_and_binary_files() {
        let root = temp_repo();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join(".env"), "SECRET=value\n").unwrap();
        fs::write(root.join("src").join("bin.dat"), b"abc\0def").unwrap();

        let sensitive = read_file_range(&root, ".env", 1, 120, 16_000).unwrap_err();
        let binary = read_file_range(&root, "src/bin.dat", 1, 120, 16_000).unwrap_err();

        assert!(matches!(sensitive, RepoToolError::SensitivePath(_)));
        assert!(matches!(binary, RepoToolError::BinaryFile(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_files_returns_repo_evidence_and_filters_results() {
        let root = temp_repo();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join(".coder")).unwrap();
        fs::write(root.join("src").join("app.py"), "app\n").unwrap();
        fs::write(root.join("src").join("app.rs"), "app\n").unwrap();
        fs::write(root.join("docs").join("app.md"), "app\n").unwrap();
        fs::write(root.join(".coder").join("app.py"), "hidden\n").unwrap();
        fs::write(root.join(".env"), "SECRET=value\n").unwrap();

        let files = find_files(&root, Some("app"), &[String::from("py")], 10).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/app.py");
        assert_eq!(files[0].normalized_path, "src/app.py");
        assert_eq!(files[0].language.as_deref(), Some("python"));
        assert_eq!(files[0].evidence_kind, "repo_evidence");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_files_skips_sensitive_paths_and_bounds_results() {
        let root = temp_repo();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".ssh")).unwrap();
        fs::write(root.join(".env"), "SECRET=value\n").unwrap();
        fs::write(root.join(".ssh").join("config"), "secret\n").unwrap();
        fs::write(root.join("src").join("a.txt"), "a\n").unwrap();
        fs::write(root.join("src").join("b.txt"), "b\n").unwrap();

        let files = find_files(&root, None, &[], 1).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/a.txt");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_text_returns_matches_and_skips_hidden_runtime_dirs() {
        let root = temp_repo();
        fs::write(root.join("src.txt"), "first\nneedle here\n").unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".git").join("ignored.txt"), "needle hidden").unwrap();
        fs::create_dir(root.join("node_modules")).unwrap();
        fs::write(root.join("node_modules").join("ignored.txt"), "needle deps").unwrap();

        let matches = search_text(&root, "needle", &RepoToolConfig::default()).unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, "src.txt");
        assert_eq!(matches[0].line, 2);
        assert_eq!(matches[0].evidence_kind, "repo_evidence");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repo_text_evidence_skips_sensitive_files() {
        let root = temp_repo();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join(".env"), "needle secret\n").unwrap();
        fs::write(root.join("src").join("app.py"), "needle safe\n").unwrap();

        let matches = search_text(&root, "needle", &RepoToolConfig::default()).unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, "src/app.py");
        let error = read_file(&root, ".env", &RepoToolConfig::default()).unwrap_err();
        assert!(matches!(error, RepoToolError::SensitivePath(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_text_rejects_empty_query() {
        let root = temp_repo();

        let error = search_text(&root, "  ", &RepoToolConfig::default()).unwrap_err();

        assert!(matches!(error, RepoToolError::EmptyQuery));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn git_status_returns_branch_and_worktree_evidence() {
        let root = temp_repo();
        init_git_repo(&root);
        fs::write(root.join("untracked.txt"), "new evidence\n").unwrap();

        let evidence = git_status(&root).unwrap();

        assert!(evidence
            .porcelain_v1
            .lines()
            .any(|line| line.starts_with("## ")));
        assert!(evidence.porcelain_v1.contains("?? untracked.txt"));
        assert!(!evidence.truncated);
        assert_eq!(evidence.evidence_kind, "repo_evidence");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn git_diff_returns_bounded_preview() {
        let root = temp_repo();
        init_git_repo(&root);
        fs::write(root.join("tracked.txt"), "base\n").unwrap();
        git(&root, &["add", "tracked.txt"]);
        fs::write(root.join("tracked.txt"), "changed\n").unwrap();

        let evidence = git_diff(&root, 4096).unwrap();

        assert!(evidence.preview.contains("diff --git"));
        assert!(evidence.preview.contains("-base"));
        assert!(evidence.preview.contains("+changed"));
        assert!(!evidence.truncated);

        let truncated = git_diff(&root, 24).unwrap();
        assert!(truncated.truncated);
        assert_eq!(truncated.preview.len(), 24);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn patch_preview_summarizes_unified_diff() {
        let root = temp_repo();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("app.py"), "base\n").unwrap();
        let patch = "\
diff --git a/src/app.py b/src/app.py
--- a/src/app.py
+++ b/src/app.py
@@ -1 +1 @@
-base
+changed
";

        let evidence = preview_patch_text(&root, patch, false).unwrap();

        assert_eq!(evidence.file_count, 1);
        assert_eq!(evidence.hunk_count, 1);
        assert_eq!(evidence.additions, 1);
        assert_eq!(evidence.deletions, 1);
        assert_eq!(evidence.files[0].new_path.as_deref(), Some("src/app.py"));
        assert_eq!(evidence.files[0].status, "modified");
        assert!(evidence.files[0].target_exists);
        assert_eq!(evidence.evidence_kind, "repo_evidence");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn patch_preview_file_reads_repo_patch() {
        let root = temp_repo();
        fs::write(root.join("tracked.txt"), "base\n").unwrap();
        fs::write(
            root.join("change.patch"),
            "\
diff --git a/tracked.txt b/tracked.txt
index df967b9..5ea2ed4 100644
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
        )
        .unwrap();

        let evidence = preview_patch_file(&root, "change.patch", DEFAULT_MAX_PATCH_BYTES).unwrap();

        assert_eq!(evidence.file_count, 1);
        assert_eq!(evidence.files[0].new_path.as_deref(), Some("tracked.txt"));
        assert!(!evidence.truncated);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn patch_apply_blocks_model_patch_without_approval() {
        let root = temp_repo();
        fs::write(root.join("tracked.txt"), "base\n").unwrap();
        fs::write(
            root.join("change.patch"),
            "\
diff --git a/tracked.txt b/tracked.txt
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
        )
        .unwrap();

        let evidence = apply_patch_file(
            &root,
            PatchApplyRequest {
                patch_file: PathBuf::from("change.patch"),
                max_patch_bytes: DEFAULT_MAX_PATCH_BYTES,
                source: "model".to_owned(),
                approved: false,
            },
        )
        .unwrap();

        assert_eq!(evidence.status, "blocked");
        assert!(!evidence.applied);
        assert!(evidence.requires_approval);
        assert!(evidence.approval_key.starts_with("patch:"));
        assert_eq!(
            fs::read_to_string(root.join("tracked.txt")).unwrap(),
            "base\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn patch_apply_applies_approved_patch() {
        let root = temp_repo();
        init_git_repo(&root);
        fs::write(root.join("tracked.txt"), "base\n").unwrap();
        fs::write(
            root.join("change.patch"),
            "\
diff --git a/tracked.txt b/tracked.txt
index df967b9..5ea2ed4 100644
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
        )
        .unwrap();

        let evidence = apply_patch_file(
            &root,
            PatchApplyRequest {
                patch_file: PathBuf::from("change.patch"),
                max_patch_bytes: DEFAULT_MAX_PATCH_BYTES,
                source: "model".to_owned(),
                approved: true,
            },
        )
        .unwrap();

        assert_eq!(evidence.status, "applied");
        assert!(evidence.applied);
        assert!(!evidence.requires_approval);
        assert_eq!(
            fs::read_to_string(root.join("tracked.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "changed\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn patch_preview_rejects_path_escape_and_sensitive_targets() {
        let root = temp_repo();
        let escaped = "\
diff --git a/src/app.py b/../escape.py
--- a/src/app.py
+++ b/../escape.py
@@ -1 +1 @@
-base
+changed
";
        let sensitive = "\
diff --git a/.env b/.env
--- a/.env
+++ b/.env
@@ -1 +1 @@
-safe
+unsafe
";

        let escaped_error = preview_patch_text(&root, escaped, false).unwrap_err();
        let sensitive_error = preview_patch_text(&root, sensitive, false).unwrap_err();

        assert!(matches!(escaped_error, RepoToolError::PathOutsideRepo(_)));
        assert!(matches!(sensitive_error, RepoToolError::SensitivePath(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_command_executes_discovered_argv_without_shell() {
        let root = temp_repo();

        let evidence = run_command(
            &root,
            CommandRunRequest {
                argv: platform_echo_args("argv-ok"),
                source: "discovered".to_owned(),
                ..CommandRunRequest::default()
            },
        )
        .unwrap();

        assert!(evidence.passed);
        assert_eq!(evidence.status, "completed");
        assert!(!evidence.requires_approval);
        assert!(evidence.output.contains("argv-ok"));
        assert_eq!(evidence.policy.risk, "low");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_command_blocks_model_command_without_approval() {
        let root = temp_repo();

        let evidence = run_command(
            &root,
            CommandRunRequest {
                argv: platform_echo_args("blocked"),
                source: "model".to_owned(),
                ..CommandRunRequest::default()
            },
        )
        .unwrap();

        assert_eq!(evidence.status, "blocked");
        assert!(evidence.blocked);
        assert!(evidence.requires_approval);
        assert_eq!(evidence.policy.risk, "medium");
        assert!(evidence.approval_key.starts_with("cmd:"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn command_timeout_uses_claude_default_and_max_bounds() {
        assert_eq!(
            effective_command_timeout_seconds(DEFAULT_COMMAND_TIMEOUT_SECONDS),
            120
        );
        assert_eq!(effective_command_timeout_seconds(0), 1);
        assert_eq!(
            effective_command_timeout_seconds(MAX_COMMAND_TIMEOUT_SECONDS + 1),
            600
        );
    }

    #[test]
    fn command_preview_reports_approval_key_without_running() {
        let root = temp_repo();

        let preview =
            preview_command(&root, ".", platform_echo_args("preview"), "model", false).unwrap();

        assert_eq!(preview.cwd, ".");
        assert!(preview.requires_approval);
        assert_eq!(preview.policy.risk, "medium");
        assert_eq!(
            preview.approval_key,
            command_approval_key(&preview.command, ".")
        );
        assert_eq!(preview.evidence_kind, "command_preview");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn command_preview_allows_discovered_sandbox_command() {
        let root = temp_repo();

        let preview = preview_command(
            &root,
            ".",
            platform_echo_args("preview"),
            "discovered",
            true,
        )
        .unwrap();

        assert!(!preview.requires_approval);
        assert_eq!(preview.policy.risk, "low");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_command_reports_nonzero_exit() {
        let root = temp_repo();

        let evidence = run_command(
            &root,
            CommandRunRequest {
                argv: platform_exit_args(7),
                source: "discovered".to_owned(),
                approved: true,
                ..CommandRunRequest::default()
            },
        )
        .unwrap();

        assert!(!evidence.passed);
        assert_eq!(evidence.status, "failed");
        assert_eq!(evidence.returncode, Some(7));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_command_writes_stdin_to_child_process() {
        let root = temp_repo();

        let evidence = run_command(
            &root,
            CommandRunRequest {
                argv: platform_cat_args(),
                stdin: Some("stdin-ok\n".to_owned()),
                source: "discovered".to_owned(),
                approved: true,
                ..CommandRunRequest::default()
            },
        )
        .unwrap();

        assert!(evidence.passed);
        assert_eq!(evidence.status, "completed");
        assert!(evidence.output.contains("stdin-ok"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_command_rejects_cwd_escape() {
        let root = temp_repo();

        let error = run_command(
            &root,
            CommandRunRequest {
                cwd: PathBuf::from(".."),
                argv: platform_echo_args("nope"),
                source: "discovered".to_owned(),
                ..CommandRunRequest::default()
            },
        )
        .unwrap_err();

        assert!(matches!(error, RepoToolError::PathOutsideRepo(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn git_status_rejects_non_git_directory() {
        let root = temp_repo();

        let error = git_status(&root).unwrap_err();

        assert!(matches!(error, RepoToolError::GitFailed { .. }));
        let _ = fs::remove_dir_all(root);
    }

    fn temp_repo() -> PathBuf {
        static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = test_tmp_root().join(format!("coder-tools-{}-{}", std::process::id(), id));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_tmp_root() -> PathBuf {
        std::env::var_os("CODER_TEST_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }

    fn init_git_repo(root: &Path) {
        git(root, &["init"]);
    }

    fn platform_echo_args(text: &str) -> Vec<String> {
        if cfg!(windows) {
            vec![
                "cmd.exe".to_owned(),
                "/C".to_owned(),
                "echo".to_owned(),
                text.to_owned(),
            ]
        } else {
            vec!["sh".to_owned(), "-c".to_owned(), format!("printf {text}")]
        }
    }

    fn platform_exit_args(code: i32) -> Vec<String> {
        if cfg!(windows) {
            vec![
                "cmd.exe".to_owned(),
                "/C".to_owned(),
                "exit".to_owned(),
                "/B".to_owned(),
                code.to_string(),
            ]
        } else {
            vec!["sh".to_owned(), "-c".to_owned(), format!("exit {code}")]
        }
    }

    fn platform_cat_args() -> Vec<String> {
        if cfg!(windows) {
            vec!["cmd.exe".to_owned(), "/C".to_owned(), "more".to_owned()]
        } else {
            vec!["sh".to_owned(), "-c".to_owned(), "cat".to_owned()]
        }
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
