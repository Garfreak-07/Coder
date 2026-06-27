use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use coder_core::{FinalReport, RunId, RunState};
use coder_events::{CoderEvent, LargePayloadRef, DEFAULT_LARGE_PAYLOAD_PREVIEW_LIMIT};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;

const MAX_REPO_EVIDENCE_STRING_CHARS: usize = 16_000;
const MAX_REPO_EVIDENCE_LIST_ITEMS: usize = 300;
const MAX_REPO_EVIDENCE_JSON_CHARS: usize = 256_000;
const REPO_EVIDENCE_SECRET_MARKERS: &[&str] = &[
    "deepseek_api_key",
    "llm_api_key",
    "api_key",
    "password",
    "begin rsa",
    "secret_key",
    "private_key",
];

#[derive(Debug, Clone)]
pub struct RunStore {
    root: PathBuf,
}

impl RunStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn write_metadata(&self, state: &RunState) -> Result<(), StoreError> {
        write_json(
            self.safe_run_dir(&state.run_id)?.join("metadata.json"),
            state,
        )
    }

    pub fn read_metadata(&self, run_id: &RunId) -> Result<Option<RunState>, StoreError> {
        read_json_optional(self.safe_run_dir(run_id)?.join("metadata.json"))
    }

    pub fn list_run_summaries(&self) -> Result<Vec<StoredRunSummary>, StoreError> {
        let runs_dir = self.root.join("runs");
        if !runs_dir.exists() {
            return Ok(Vec::new());
        }

        let mut summaries = Vec::new();
        for entry in fs::read_dir(runs_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(run_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if safe_store_segment(&run_name, "run_id").is_err() {
                continue;
            }

            let run_id = RunId::from_string(run_name.clone());
            let metadata = self.read_metadata(&run_id)?;
            let event_count = self.read_events(&run_id)?.len();
            let has_report = self.read_report(&run_id)?.is_some();
            let repo_evidence_count = self.repo_evidence_count(&run_id)?;
            summaries.push(StoredRunSummary {
                run_id: run_name,
                metadata,
                event_count,
                has_report,
                repo_evidence_count,
            });
        }
        summaries.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        Ok(summaries)
    }

    pub fn append_event(&self, run_id: &RunId, event: &CoderEvent) -> Result<(), StoreError> {
        let path = self.safe_run_dir(run_id)?.join("events.jsonl");
        ensure_parent(&path)?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(event.to_jsonl()?.as_bytes())?;
        Ok(())
    }

    pub fn read_events(&self, run_id: &RunId) -> Result<Vec<CoderEvent>, StoreError> {
        let path = self.safe_run_dir(run_id)?.join("events.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if !line.trim().is_empty() {
                events.push(CoderEvent::from_jsonl_line(&line)?);
            }
        }
        Ok(events)
    }

    pub fn write_report(&self, run_id: &RunId, report: &FinalReport) -> Result<String, StoreError> {
        self.write_artifact(run_id, "final-report.json", report)
    }

    pub fn write_repo_evidence(
        &self,
        run_id: &RunId,
        kind: RepoEvidenceKind,
        repo_root: impl Into<String>,
        scope_paths: Vec<String>,
        summary: impl Into<String>,
        payload: Value,
    ) -> Result<RepoEvidenceRef, StoreError> {
        let safe_run_id = safe_store_segment(run_id.as_str(), "run_id")?;
        let evidence_dir = self
            .root
            .join("runs")
            .join(&safe_run_id)
            .join("repo_evidence");
        fs::create_dir_all(&evidence_dir)?;

        let prefix = kind.prefix();
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let ref_id = format!("{prefix}:{suffix}");
        let payload_path = evidence_dir.join(format!("{prefix}-{suffix}.json"));
        let sanitized = sanitize_repo_evidence_payload(payload)?;
        let payload_text = serde_json::to_string_pretty(&sanitized)?;
        if payload_text.chars().count() > MAX_REPO_EVIDENCE_JSON_CHARS {
            return Err(StoreError::RepoEvidencePayloadTooLarge {
                max_chars: MAX_REPO_EVIDENCE_JSON_CHARS,
            });
        }

        fs::write(&payload_path, format!("{payload_text}\n"))?;
        let reference = RepoEvidenceRef {
            ref_id,
            kind,
            repo_root: repo_root.into(),
            scope_paths,
            summary: compact_string(&summary.into(), 500),
            payload_path: payload_path.display().to_string(),
            created_at: OffsetDateTime::now_utc(),
            token_estimate: token_estimate(&payload_text),
        };
        let index_path = evidence_dir.join("index.jsonl");
        let mut index = OpenOptions::new()
            .create(true)
            .append(true)
            .open(index_path)?;
        index.write_all(serde_json::to_string(&reference)?.as_bytes())?;
        index.write_all(b"\n")?;
        Ok(reference)
    }

    pub fn read_repo_evidence(&self, ref_id: &str) -> Result<Value, StoreError> {
        let safe_ref_id = safe_store_segment(ref_id, "ref_id")?;
        let runs_dir = self.root.join("runs");
        if !runs_dir.exists() {
            return Err(StoreError::RepoEvidenceNotFound(safe_ref_id));
        }
        for run_entry in fs::read_dir(runs_dir)? {
            let run_entry = run_entry?;
            let evidence_dir = run_entry.path().join("repo_evidence");
            let index_path = evidence_dir.join("index.jsonl");
            if !index_path.exists() {
                continue;
            }
            let file = fs::File::open(&index_path)?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let record: RepoEvidenceRef = serde_json::from_str(&line)?;
                if record.ref_id != safe_ref_id {
                    continue;
                }
                let payload_path = PathBuf::from(&record.payload_path);
                ensure_path_under(&payload_path, &evidence_dir)?;
                let payload_text = fs::read_to_string(payload_path)?;
                return Ok(serde_json::from_str(&payload_text)?);
            }
        }
        Err(StoreError::RepoEvidenceNotFound(safe_ref_id))
    }

    pub fn read_report(&self, run_id: &RunId) -> Result<Option<FinalReport>, StoreError> {
        read_json_optional(
            self.safe_run_dir(run_id)?
                .join("artifacts")
                .join("final-report.json"),
        )
    }

    pub fn write_artifact<T: Serialize>(
        &self,
        run_id: &RunId,
        name: &str,
        value: &T,
    ) -> Result<String, StoreError> {
        let safe_name = safe_file_name(name)?;
        let path = self
            .safe_run_dir(run_id)?
            .join("artifacts")
            .join(&safe_name);
        write_json(&path, value)?;
        Ok(format!(
            "artifact://runs/{}/artifacts/{safe_name}",
            run_id.as_str()
        ))
    }

    pub fn read_artifact_json(&self, run_id: &RunId, name: &str) -> Result<Value, StoreError> {
        let safe_name = safe_file_name(name)?;
        let path = self
            .safe_run_dir(run_id)?
            .join("artifacts")
            .join(&safe_name);
        if !path.exists() {
            return Err(StoreError::ArtifactNotFound {
                run_id: run_id.as_str().to_owned(),
                name: safe_name,
            });
        }
        let text = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn write_blob(&self, content: &[u8]) -> Result<String, StoreError> {
        let digest = Sha256::digest(content);
        let hex = format!("{digest:x}");
        let path = self.root.join("blobs").join(&hex[..2]).join(&hex);
        ensure_parent(&path)?;
        if !path.exists() {
            fs::write(path, content)?;
        }
        Ok(format!("blob://sha256/{hex}"))
    }

    pub fn read_blob_sha256(&self, digest: &str) -> Result<Vec<u8>, StoreError> {
        let safe_digest = safe_sha256_digest(digest)?;
        let path = self
            .root
            .join("blobs")
            .join(&safe_digest[..2])
            .join(&safe_digest);
        if !path.exists() {
            return Err(StoreError::BlobNotFound(safe_digest));
        }
        Ok(fs::read(path)?)
    }

    pub fn write_large_text_ref(&self, content: &str) -> Result<LargePayloadRef, StoreError> {
        self.write_large_text_ref_with_limit(content, DEFAULT_LARGE_PAYLOAD_PREVIEW_LIMIT)
    }

    pub fn write_large_text_ref_with_limit(
        &self,
        content: &str,
        preview_limit: usize,
    ) -> Result<LargePayloadRef, StoreError> {
        let blob_ref = self.write_blob(content.as_bytes())?;
        Ok(LargePayloadRef::from_text(content, blob_ref, preview_limit))
    }

    pub fn run_dir(&self, run_id: &RunId) -> PathBuf {
        self.root.join("runs").join(run_id.as_str())
    }

    fn safe_run_dir(&self, run_id: &RunId) -> Result<PathBuf, StoreError> {
        let safe_run_id = safe_store_segment(run_id.as_str(), "run_id")?;
        Ok(self.root.join("runs").join(safe_run_id))
    }

    pub fn repo_evidence_count(&self, run_id: &RunId) -> Result<usize, StoreError> {
        let index_path = self
            .safe_run_dir(run_id)?
            .join("repo_evidence")
            .join("index.jsonl");
        if !index_path.exists() {
            return Ok(0);
        }
        let file = fs::File::open(index_path)?;
        let reader = BufReader::new(file);
        let mut count = 0;
        for line in reader.lines() {
            if !line?.trim().is_empty() {
                count += 1;
            }
        }
        Ok(count)
    }
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
    fn prefix(self) -> &'static str {
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

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid file name: {0}")]
    InvalidFileName(String),
    #[error("invalid store segment for {label}: {value}")]
    InvalidStoreSegment { label: String, value: String },
    #[error("repo evidence payload contains secret-like text")]
    RepoEvidenceSecretLikeText,
    #[error("repo evidence payload is over limit {max_chars} chars")]
    RepoEvidencePayloadTooLarge { max_chars: usize },
    #[error("repo evidence not found: {0}")]
    RepoEvidenceNotFound(String),
    #[error("repo evidence payload path escaped repo_evidence directory: {0}")]
    RepoEvidencePathEscape(String),
    #[error("artifact not found: runs/{run_id}/artifacts/{name}")]
    ArtifactNotFound { run_id: String, name: String },
    #[error("invalid blob sha256 digest: {0}")]
    InvalidBlobDigest(String),
    #[error("blob not found: sha256:{0}")]
    BlobNotFound(String),
}

fn write_json(path: impl AsRef<Path>, value: &impl Serialize) -> Result<(), StoreError> {
    let path = path.as_ref();
    ensure_parent(path)?;
    fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn read_json_optional<T: DeserializeOwned>(
    path: impl AsRef<Path>,
) -> Result<Option<T>, StoreError> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&text)?))
}

fn ensure_parent(path: &Path) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn safe_file_name(value: &str) -> Result<String, StoreError> {
    if value.is_empty()
        || value.contains('/')
        || value.contains('\\')
        || value == "."
        || value == ".."
        || !value
            .chars()
            .all(|item| item.is_ascii_alphanumeric() || matches!(item, '_' | '.' | '-'))
    {
        return Err(StoreError::InvalidFileName(value.to_owned()));
    }
    Ok(value.to_owned())
}

fn safe_store_segment(value: &str, label: &str) -> Result<String, StoreError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || !value
            .chars()
            .all(|item| item.is_ascii_alphanumeric() || matches!(item, '_' | '.' | ':' | '-'))
    {
        return Err(StoreError::InvalidStoreSegment {
            label: label.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(value.to_owned())
}

fn safe_sha256_digest(value: &str) -> Result<String, StoreError> {
    if value.len() != 64 || !value.chars().all(|item| item.is_ascii_hexdigit()) {
        return Err(StoreError::InvalidBlobDigest(value.to_owned()));
    }
    Ok(value.to_ascii_lowercase())
}

fn sanitize_repo_evidence_payload(value: Value) -> Result<Value, StoreError> {
    match value {
        Value::Object(object) => object
            .into_iter()
            .map(|(key, value)| Ok((key, sanitize_repo_evidence_payload(value)?)))
            .collect::<Result<Map<String, Value>, StoreError>>()
            .map(Value::Object),
        Value::Array(items) => {
            let omitted_items = items.len().saturating_sub(MAX_REPO_EVIDENCE_LIST_ITEMS);
            let mut sanitized = items
                .into_iter()
                .take(MAX_REPO_EVIDENCE_LIST_ITEMS)
                .map(sanitize_repo_evidence_payload)
                .collect::<Result<Vec<_>, _>>()?;
            if omitted_items > 0 {
                sanitized.push(serde_json::json!({
                    "truncated": true,
                    "omitted_items": omitted_items
                }));
            }
            Ok(Value::Array(sanitized))
        }
        Value::String(text) => {
            reject_secret_like_text(&text)?;
            Ok(Value::String(compact_string(
                &text,
                MAX_REPO_EVIDENCE_STRING_CHARS,
            )))
        }
        other => Ok(other),
    }
}

fn reject_secret_like_text(value: &str) -> Result<(), StoreError> {
    let lowered = value.to_ascii_lowercase();
    if REPO_EVIDENCE_SECRET_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return Err(StoreError::RepoEvidenceSecretLikeText);
    }
    Ok(())
}

fn compact_string(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let mut compacted = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        compacted.truncate(compacted.trim_end().len());
        compacted.push_str("...");
    }
    compacted
}

fn token_estimate(text: &str) -> usize {
    text.chars().count().div_ceil(4).max(1)
}

fn ensure_path_under(path: &Path, root: &Path) -> Result<(), StoreError> {
    let canonical_path = path.canonicalize()?;
    let canonical_root = root.canonicalize()?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(StoreError::RepoEvidencePathEscape(
            path.display().to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use coder_events::CoderEvent;
    use serde_json::json;

    use super::*;

    #[test]
    fn event_log_roundtrips() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");
        let event = CoderEvent::new(
            run_id.clone(),
            1,
            "run.started",
            json!({"workflow_id": "wf"}),
        );

        store.append_event(&run_id, &event).unwrap();
        let events = store.read_events(&run_id).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "run.started");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn blob_refs_are_content_addressed() {
        let root = temp_root();
        let store = RunStore::new(&root);

        let first = store.write_blob(b"same content").unwrap();
        let second = store.write_blob(b"same content").unwrap();

        assert_eq!(first, second);
        assert!(first.starts_with("blob://sha256/"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn large_text_refs_store_full_content_outside_event_payload() {
        let root = temp_root();
        let store = RunStore::new(&root);

        let payload = store
            .write_large_text_ref_with_limit("0123456789", 4)
            .unwrap();

        assert_eq!(payload.preview, "0123");
        assert!(payload.truncated);
        assert!(payload.blob_ref.starts_with("blob://sha256/"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn artifact_names_reject_path_traversal() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");

        let error = store
            .write_artifact(&run_id, "../escape.json", &json!({"bad": true}))
            .unwrap_err();

        assert!(matches!(error, StoreError::InvalidFileName(_)));
        let wildcard_error = store
            .write_artifact(&run_id, "bad*name.json", &json!({"bad": true}))
            .unwrap_err();
        assert!(matches!(wildcard_error, StoreError::InvalidFileName(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn artifact_json_roundtrips_and_reports_missing() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");

        let reference = store
            .write_artifact(&run_id, "summary.json", &json!({"status": "ok"}))
            .unwrap();
        let payload = store.read_artifact_json(&run_id, "summary.json").unwrap();
        let missing = store
            .read_artifact_json(&run_id, "missing.json")
            .unwrap_err();

        assert_eq!(reference, "artifact://runs/run_test/artifacts/summary.json");
        assert_eq!(payload["status"], "ok");
        assert!(matches!(missing, StoreError::ArtifactNotFound { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn blob_reads_by_sha256_digest() {
        let root = temp_root();
        let store = RunStore::new(&root);

        let reference = store.write_blob(b"same content").unwrap();
        let digest = reference.strip_prefix("blob://sha256/").unwrap();
        let loaded = store.read_blob_sha256(digest).unwrap();
        let missing = store
            .read_blob_sha256("0000000000000000000000000000000000000000000000000000000000000000")
            .unwrap_err();
        let invalid = store.read_blob_sha256("../escape").unwrap_err();

        assert_eq!(loaded, b"same content");
        assert!(matches!(missing, StoreError::BlobNotFound(_)));
        assert!(matches!(invalid, StoreError::InvalidBlobDigest(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_store_operations_reject_unsafe_run_segments() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("../escape");
        let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
        state.status = coder_core::RunStatus::Completed;

        let metadata_error = store.write_metadata(&state).unwrap_err();
        let event_error = store
            .append_event(
                &run_id,
                &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
            )
            .unwrap_err();
        let artifact_error = store
            .write_artifact(&run_id, "summary.json", &json!({"bad": true}))
            .unwrap_err();

        assert!(matches!(
            metadata_error,
            StoreError::InvalidStoreSegment { .. }
        ));
        assert!(matches!(
            event_error,
            StoreError::InvalidStoreSegment { .. }
        ));
        assert!(matches!(
            artifact_error,
            StoreError::InvalidStoreSegment { .. }
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn metadata_and_report_roundtrip() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");
        let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
        state.status = coder_core::RunStatus::Completed;
        let report = FinalReport::completed("done").with_evidence("event_log", "eventlog://run");

        store.write_metadata(&state).unwrap();
        store.write_report(&run_id, &report).unwrap();

        let loaded_state = store.read_metadata(&run_id).unwrap().unwrap();
        let loaded_report = store.read_report(&run_id).unwrap().unwrap();
        assert_eq!(loaded_state.status, coder_core::RunStatus::Completed);
        assert_eq!(loaded_report.summary, "done");
        assert_eq!(loaded_report.evidence_refs[0].kind, "event_log");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_run_summaries_reports_counts_and_skips_unsafe_dirs() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run_test");
        let mut state = RunState::new(run_id.clone(), coder_core::WorkflowId::new("workflow"));
        state.status = coder_core::RunStatus::Completed;
        let report = FinalReport::completed("done");

        fs::create_dir_all(root.join("runs").join("bad run")).unwrap();
        store.write_metadata(&state).unwrap();
        store
            .append_event(
                &run_id,
                &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
            )
            .unwrap();
        store.write_report(&run_id, &report).unwrap();
        store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoRead,
                "repo",
                Vec::new(),
                "read",
                json!({"snippet": "safe"}),
            )
            .unwrap();

        let summaries = store.list_run_summaries().unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].run_id, "run_test");
        assert_eq!(summaries[0].metadata.as_ref().unwrap().status, state.status);
        assert_eq!(summaries[0].event_count, 1);
        assert!(summaries[0].has_report);
        assert_eq!(summaries[0].repo_evidence_count, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_run_summaries_is_empty_without_runs_dir() {
        let root = temp_root();
        let store = RunStore::new(&root);

        let summaries = store.list_run_summaries().unwrap();

        assert!(summaries.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repo_evidence_roundtrips_with_index_ref() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");

        let reference = store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoTextSearch,
                "F:/repo",
                vec!["src".to_owned()],
                "Found one hit.",
                json!({"evidence_kind": "repo_evidence", "hits": [{"path": "src/app.py", "line": 1}]}),
            )
            .unwrap();
        let payload = store.read_repo_evidence(&reference.ref_id).unwrap();

        assert!(reference.ref_id.starts_with("repo-text-search:"));
        assert_eq!(reference.kind, RepoEvidenceKind::RepoTextSearch);
        assert!(PathBuf::from(&reference.payload_path)
            .starts_with(root.join("runs").join("run-1").join("repo_evidence")));
        assert_eq!(payload["hits"][0]["path"], "src/app.py");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repo_evidence_rejects_unsafe_segments() {
        let root = temp_root();
        let store = RunStore::new(&root);

        let run_error = store
            .write_repo_evidence(
                &RunId::from_string("../escape"),
                RepoEvidenceKind::RepoRead,
                "repo",
                Vec::new(),
                "bad",
                json!({"text": "safe"}),
            )
            .unwrap_err();
        let ref_error = store.read_repo_evidence("../escape").unwrap_err();

        assert!(matches!(run_error, StoreError::InvalidStoreSegment { .. }));
        assert!(matches!(ref_error, StoreError::InvalidStoreSegment { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repo_evidence_compacts_large_strings_and_lists() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        let items = (0..350)
            .map(|index| json!({"path": format!("src/{index}.rs")}))
            .collect::<Vec<_>>();

        let reference = store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoFileList,
                "repo",
                Vec::new(),
                "large",
                json!({"snippet": "x".repeat(20_000), "items": items}),
            )
            .unwrap();
        let payload = store.read_repo_evidence(&reference.ref_id).unwrap();

        assert!(payload["snippet"].as_str().unwrap().len() < 20_000);
        assert!(payload["snippet"].as_str().unwrap().ends_with("..."));
        assert_eq!(payload["items"].as_array().unwrap().len(), 301);
        assert_eq!(payload["items"][300]["truncated"], true);
        assert_eq!(payload["items"][300]["omitted_items"], 50);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repo_evidence_rejects_secret_like_text() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");

        let error = store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoRead,
                "repo",
                Vec::new(),
                "secret",
                json!({"snippet": "api_key=abc"}),
            )
            .unwrap_err();

        assert!(matches!(error, StoreError::RepoEvidenceSecretLikeText));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repo_evidence_rejects_payload_path_escape() {
        let root = temp_root();
        let store = RunStore::new(&root);
        let run_id = RunId::from_string("run-1");
        let reference = store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoRead,
                "repo",
                Vec::new(),
                "read",
                json!({"snippet": "safe"}),
            )
            .unwrap();
        let outside = root.join("outside.json");
        fs::write(&outside, "{}").unwrap();
        let mut escaped = reference;
        escaped.payload_path = outside.display().to_string();
        let index_path = root
            .join("runs")
            .join("run-1")
            .join("repo_evidence")
            .join("index.jsonl");
        fs::write(
            index_path,
            format!("{}\n", serde_json::to_string(&escaped).unwrap()),
        )
        .unwrap();

        let error = store.read_repo_evidence(&escaped.ref_id).unwrap_err();

        assert!(matches!(error, StoreError::RepoEvidencePathEscape(_)));
        let _ = fs::remove_dir_all(root);
    }

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "coder-store-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
