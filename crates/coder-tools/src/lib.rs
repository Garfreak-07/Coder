use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_MAX_FILE_BYTES: u64 = 64 * 1024;
pub const DEFAULT_MAX_FILE_RESULTS: usize = 200;
pub const DEFAULT_MAX_SEARCH_MATCHES: usize = 50;
pub const DEFAULT_MAX_GIT_OUTPUT_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_PATCH_BYTES: usize = 256 * 1024;

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
    file.by_ref()
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

struct CommandPreview {
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
) -> Result<CommandPreview, RepoToolError> {
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
    Ok(CommandPreview {
        preview: String::from_utf8_lossy(preview_bytes).into_owned(),
        truncated,
    })
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
        fs::write(root.join("src").join("unicode.txt"), "abcédef\n").unwrap();

        let by_lines = read_file_range(&root, "src/app.py", 1, 2, 16_000).unwrap();
        let by_chars = read_file_range(&root, "src/unicode.txt", 1, 120, 4).unwrap();

        assert_eq!(by_lines.end_line, 2);
        assert!(by_lines.truncated);
        assert_eq!(by_chars.text, "abcé");
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
    fn git_status_rejects_non_git_directory() {
        let root = temp_repo();

        let error = git_status(&root).unwrap_err();

        assert!(matches!(error, RepoToolError::GitFailed { .. }));
        let _ = fs::remove_dir_all(root);
    }

    fn temp_repo() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "coder-tools-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn init_git_repo(root: &Path) {
        git(root, &["init"]);
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
