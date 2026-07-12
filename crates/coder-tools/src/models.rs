use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    DEFAULT_COMMAND_TIMEOUT_SECONDS, DEFAULT_MAX_COMMAND_OUTPUT_BYTES, DEFAULT_MAX_FILE_BYTES,
    DEFAULT_MAX_SEARCH_MATCHES,
};

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

#[derive(Debug, Clone)]
pub struct PatchApplyTextRequest {
    pub patch: String,
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
    #[error("invalid inline patch: {0}")]
    PatchInvalid(String),
    #[error("command argv must contain at least one non-empty argument")]
    EmptyCommandArgv,
    #[error("failed to run command: {0}")]
    CommandIo(std::io::Error),
    #[error("live command process limit reached ({0})")]
    CommandProcessLimitReached(usize),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
