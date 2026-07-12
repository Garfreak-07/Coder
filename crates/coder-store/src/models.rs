use std::path::{Path, PathBuf};

use coder_core::{FinalReport, RunState};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use time::OffsetDateTime;

use crate::MAX_DURABLE_JSONL_PAGE_LIMIT;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalStoreLayout {
    pub root: PathBuf,
    pub sessions: PathBuf,
    pub runs: PathBuf,
    pub background_tasks: PathBuf,
    pub timeline: PathBuf,
    pub blobs: PathBuf,
    pub artifacts: PathBuf,
    pub settings: PathBuf,
    pub checkpoints: PathBuf,
    pub changesets: PathBuf,
    pub repo_index: PathBuf,
    pub plugin_cache: PathBuf,
    pub skill_cache: PathBuf,
    pub logs: PathBuf,
    pub tmp: PathBuf,
}

impl LocalStoreLayout {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            sessions: root.join("sessions"),
            runs: root.join("runs"),
            background_tasks: root.join("background-tasks"),
            timeline: root.join("timeline"),
            blobs: root.join("blobs"),
            artifacts: root.join("artifacts"),
            settings: root.join("settings"),
            checkpoints: root.join("checkpoints"),
            changesets: root.join("changesets"),
            repo_index: root.join("repo-index"),
            plugin_cache: root.join("plugin-cache"),
            skill_cache: root.join("skill-cache"),
            logs: root.join("logs"),
            tmp: root.join("tmp"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionJsonlRecord {
    pub session_id: String,
    pub sequence: u64,
    pub kind: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubagentTranscriptRecord {
    pub run_id: String,
    pub agent_id: String,
    pub sequence: u64,
    #[serde(default)]
    pub parent_sequence: Option<u64>,
    pub kind: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentReplacementRecord {
    pub kind: String,
    #[serde(rename = "toolUseId")]
    pub tool_use_id: String,
    pub replacement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunContentReplacementEntry {
    pub run_id: String,
    pub sequence: u64,
    pub kind: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default)]
    pub replacements: Vec<ContentReplacementRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubagentMetadata {
    pub agent_type: String,
    pub parent_agent_id: String,
    pub parent_harness_id: String,
    pub invocation_kind: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub terminal_record_kind: Option<String>,
    #[serde(default)]
    pub last_sequence: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub transcript_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentBackgroundTaskRecord {
    pub task_id: String,
    pub run_id: String,
    pub agent_id: String,
    pub status: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub metadata_ref: String,
    pub transcript_ref: String,
    #[serde(default)]
    pub report: Option<FinalReport>,
    #[serde(default)]
    pub event_count: usize,
    #[serde(default)]
    pub events_truncated: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandBackgroundTaskRecord {
    pub task_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    pub repo_root: String,
    pub cwd: String,
    pub argv: Vec<String>,
    pub command: String,
    pub approval_key: String,
    pub policy: Value,
    pub status: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub output_ref: String,
    #[serde(default)]
    pub output_bytes: u64,
    #[serde(default)]
    pub output_start_offset: u64,
    #[serde(default)]
    pub output_total_bytes: u64,
    #[serde(default)]
    pub output_truncated: bool,
    pub max_output_bytes: usize,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub evidence_ref: Option<RepoEvidenceRef>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBackgroundOutputTail {
    pub output: String,
    pub bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurableJsonlPageOptions {
    pub after_sequence: Option<u64>,
    pub limit: usize,
    pub tail: bool,
}

impl DurableJsonlPageOptions {
    pub fn new(limit: usize) -> Result<Self, StoreError> {
        Self::with_after_sequence(None, limit)
    }

    pub fn with_after_sequence(
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<Self, StoreError> {
        if limit == 0 || limit > MAX_DURABLE_JSONL_PAGE_LIMIT {
            return Err(StoreError::DurableJsonlPageLimitOutOfRange {
                limit,
                max: MAX_DURABLE_JSONL_PAGE_LIMIT,
            });
        }
        Ok(Self {
            after_sequence,
            limit,
            tail: false,
        })
    }

    pub fn tail(limit: usize) -> Result<Self, StoreError> {
        if limit == 0 || limit > MAX_DURABLE_JSONL_PAGE_LIMIT {
            return Err(StoreError::DurableJsonlPageLimitOutOfRange {
                limit,
                max: MAX_DURABLE_JSONL_PAGE_LIMIT,
            });
        }
        Ok(Self {
            after_sequence: None,
            limit,
            tail: true,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DurableJsonlPage<T> {
    pub records: Vec<T>,
    pub total_records: usize,
    pub matching_records: usize,
    pub returned_records: usize,
    pub truncated: bool,
    pub next_after_sequence: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheBucketUsage {
    pub entries: usize,
    pub bytes: u64,
    pub scanned_entries: usize,
    pub entry_scan_limit: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheCleanupSummary {
    pub directories: Vec<String>,
    pub entries: usize,
    pub bytes: u64,
    pub entry_scan_limit: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactionCircuitState {
    pub scope_id: String,
    pub max_consecutive_failures: u8,
    pub consecutive_failures: u8,
    pub circuit_breaker_open: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoEvidenceKind {
    RepoFileList,
    RepoTextSearch,
    RepoRead,
    RepoTest,
    RepoDiff,
}

impl RepoEvidenceKind {
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            Self::RepoFileList => "repo-file-list",
            Self::RepoTextSearch => "repo-text-search",
            Self::RepoRead => "repo-read",
            Self::RepoTest => "repo-test",
            Self::RepoDiff => "repo-diff",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoEvidenceRef {
    pub ref_id: String,
    pub kind: RepoEvidenceKind,
    pub repo_root: String,
    #[serde(default)]
    pub scope_paths: Vec<String>,
    pub summary: String,
    pub payload_path: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub token_estimate: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRunSummary {
    pub run_id: String,
    pub metadata: Option<RunState>,
    pub event_count: usize,
    pub has_report: bool,
    pub repo_evidence_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCheckpointRef {
    pub name: String,
    pub checkpoint_ref: String,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("run not found: {0}")]
    RunNotFound(String),
    #[error("invalid file name: {0}")]
    InvalidFileName(String),
    #[error("invalid store segment for {label}: {value}")]
    InvalidStoreSegment { label: String, value: String },
    #[error("repo evidence payload contains secret-like text")]
    RepoEvidenceSecretLikeText,
    #[error("session JSONL record contains secret-like text")]
    SessionRecordSecretLikeText,
    #[error("repo evidence payload is over limit {max_chars} chars")]
    RepoEvidencePayloadTooLarge { max_chars: usize },
    #[error("durable read over limit: {path} is {bytes} bytes, max {max_bytes} bytes")]
    DurableReadLimitExceeded {
        path: String,
        bytes: u64,
        max_bytes: u64,
    },
    #[error("durable JSONL page limit {limit} is out of range, max {max}")]
    DurableJsonlPageLimitOutOfRange { limit: usize, max: usize },
    #[error("repo evidence not found: {0}")]
    RepoEvidenceNotFound(String),
    #[error("repo evidence payload path escaped repo_evidence directory: {0}")]
    RepoEvidencePathEscape(String),
    #[error("artifact not found: runs/{run_id}/artifacts/{name}")]
    ArtifactNotFound { run_id: String, name: String },
    #[error("checkpoint not found: runs/{run_id}/checkpoints/{name}")]
    CheckpointNotFound { run_id: String, name: String },
    #[error("invalid blob sha256 digest: {0}")]
    InvalidBlobDigest(String),
    #[error("blob not found: sha256:{0}")]
    BlobNotFound(String),
}
