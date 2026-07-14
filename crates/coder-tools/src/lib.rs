use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Component, Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};

mod catalog;
mod command_process;
mod inline_patch;

pub use catalog::{
    builtin_tool, builtin_tools, canonical_builtin_tool_name, BuiltinToolDefinition,
    ToolConcurrencyClass, ToolPermission, MODEL_MAX_FILE_EDITS,
};
pub use command_process::{
    start_command_process, CommandProcessHandle, CommandProcessOutputState, CommandProcessRequest,
    CommandProcessSnapshot, MAX_LIVE_COMMAND_PROCESSES,
};
pub use inline_patch::{apply_patch_text, APPLY_PATCH_LARK_GRAMMAR};

pub const DEFAULT_MAX_FILE_BYTES: u64 = 64 * 1024;
pub const DEFAULT_MAX_FILE_RESULTS: usize = 200;
pub const DEFAULT_MAX_SEARCH_MATCHES: usize = 50;
pub const DEFAULT_MAX_GIT_OUTPUT_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_PATCH_BYTES: usize = 256 * 1024;
pub const DEFAULT_MAX_WRITE_FILE_BYTES: usize = 2 * 1024 * 1024;
pub const DEFAULT_COMMAND_TIMEOUT_SECONDS: u64 = 120;
pub const MAX_COMMAND_TIMEOUT_SECONDS: u64 = 600;
// Match Codex unified exec's raw output hard cap.
pub const DEFAULT_MAX_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;

const MODEL_PROVIDER_CREDENTIAL_ENV_KEYS: &[&str] = &[
    "CODER_API_KEY",
    "LLM_API_KEY",
    "OPENAI_API_KEY",
    "DEEPSEEK_API_KEY",
    "MOONSHOT_API_KEY",
    "DASHSCOPE_API_KEY",
    "GROQ_API_KEY",
    "OPENROUTER_API_KEY",
    "TOGETHER_API_KEY",
    "MISTRAL_API_KEY",
    "PERPLEXITY_API_KEY",
    "XAI_API_KEY",
    "GEMINI_API_KEY",
    "OLLAMA_API_KEY",
];

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

mod models;
pub use models::*;

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
        .filter(|item| !item.is_empty() && !is_match_all_file_query(item));
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

fn is_match_all_file_query(query: &str) -> bool {
    matches!(query, "*" | "*.*" | "**/*" | "**\\*")
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
    let preview = preview_command(
        repo_root,
        &request.cwd,
        request.argv,
        &request.source,
        request.sandbox,
    )?;
    if preview.requires_approval && !request.approved {
        return Ok(CommandRunEvidence {
            repo_root: preview.repo_root,
            cwd: preview.cwd,
            argv: preview.argv,
            command: preview.command.clone(),
            status: "blocked".to_owned(),
            passed: false,
            blocked: true,
            requires_approval: true,
            approval_key: preview.approval_key,
            returncode: None,
            output: format!(
                "Check command requires explicit approval: {}",
                preview.command
            ),
            output_truncated: false,
            timed_out: false,
            policy: preview.policy,
            evidence_kind: "command_evidence".to_owned(),
        });
    }
    let process = start_command_process(
        preview,
        CommandProcessRequest {
            timeout_seconds: Some(effective_command_timeout_seconds(request.timeout_seconds)),
            max_output_bytes: request.max_output_bytes,
            source: request.source,
            interactive: false,
            initial_stdin: request.stdin,
        },
    )?;
    process.wait(None);
    process.evidence().ok_or_else(|| {
        RepoToolError::CommandIo(std::io::Error::other(
            "command process did not produce terminal evidence",
        ))
    })
}

pub fn configure_model_command_environment(command: &mut Command, source: &str) {
    if source != "model" {
        return;
    }
    for name in MODEL_PROVIDER_CREDENTIAL_ENV_KEYS {
        command.env_remove(name);
    }
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
    _sandbox: bool,
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
            requires_approval: true,
            risk: "medium".to_owned(),
            reason: "Shell-like command boundary requires approval.".to_owned(),
        };
    }
    if source == "model" {
        return CommandPolicyDecision {
            allowed: true,
            requires_approval: true,
            risk: "medium".to_owned(),
            reason: "Model-generated command requires host approval.".to_owned(),
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

#[cfg(test)]
mod tests;
