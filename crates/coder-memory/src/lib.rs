use std::{fs, path::Path};

use coder_core::RunId;
use coder_events::CoderEvent;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

const MEMORY_WRITE_PREVIEW_LIMIT: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    User,
    Project,
    Agent,
    Workflow,
    Run,
    RepoFacts,
    KnowledgeHints,
    ExternalDocs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub kind: String,
    pub reference: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub scope: MemoryScope,
    pub key: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<EvidenceRef>,
    pub source_ref: Option<String>,
    #[serde(default = "default_trust_level")]
    pub trust_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMemoryFile {
    pub version: u16,
    #[serde(default)]
    pub records: Vec<MemoryRecord>,
}

pub fn load_project_memory_file(path: impl AsRef<Path>) -> Result<ProjectMemoryFile, MemoryError> {
    let path = path.as_ref();
    let text = fs::read_to_string(path).map_err(|source| MemoryError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let file: ProjectMemoryFile =
        serde_json::from_str(&text).map_err(|source| MemoryError::Parse {
            path: path.display().to_string(),
            source,
        })?;
    if file.version != 1 {
        return Err(MemoryError::UnsupportedVersion(file.version));
    }
    Ok(file)
}

pub fn memory_read_event(run_id: RunId, sequence: u64, records: &[MemoryRecord]) -> CoderEvent {
    CoderEvent::new(
        run_id,
        sequence,
        "memory.read",
        json!({
            "record_count": records.len(),
            "records": records.iter().map(memory_record_summary).collect::<Vec<_>>()
        }),
    )
}

pub fn memory_write_proposed_event(
    run_id: RunId,
    sequence: u64,
    record: &MemoryRecord,
) -> CoderEvent {
    let (preview, truncated) = preview_text(&record.content, MEMORY_WRITE_PREVIEW_LIMIT);
    CoderEvent::new(
        run_id,
        sequence,
        "memory.write.proposed",
        json!({
            "record": memory_record_summary(record),
            "content_preview": preview,
            "content_truncated": truncated
        }),
    )
}

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to parse memory JSON {path}: {source}")]
    Parse {
        path: String,
        source: serde_json::Error,
    },
    #[error("unsupported memory file version: {0}")]
    UnsupportedVersion(u16),
}

fn memory_record_summary(record: &MemoryRecord) -> serde_json::Value {
    json!({
        "id": record.id,
        "scope": record.scope,
        "key": record.key,
        "tags": record.tags,
        "evidence_refs": record.evidence_refs,
        "source_ref": record.source_ref,
        "trust_level": record.trust_level
    })
}

fn preview_text(text: &str, limit: usize) -> (String, bool) {
    let mut chars = text.chars();
    let preview: String = chars.by_ref().take(limit).collect();
    let truncated = chars.next().is_some();
    (preview, truncated)
}

fn default_trust_level() -> String {
    "local".to_owned()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    #[test]
    fn loads_project_memory_file() {
        let path = temp_path("project-memory.json");
        fs::write(
            &path,
            r#"{
              "version": 1,
              "records": [
                {
                  "id": "mem_1",
                  "scope": "project",
                  "key": "architecture",
                  "content": "Rust owns the control plane.",
                  "tags": ["rust"],
                  "evidence_refs": [{"kind": "doc", "reference": "docs/rust-migration-map.md"}],
                  "source_ref": "memory://project/architecture"
                }
              ]
            }"#,
        )
        .unwrap();

        let file = load_project_memory_file(&path).unwrap();

        assert_eq!(file.version, 1);
        assert_eq!(file.records[0].scope, MemoryScope::Project);
        assert_eq!(file.records[0].trust_level, "local");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn memory_read_event_omits_full_content() {
        let record = fixture_record("Secret architecture note");
        let event = memory_read_event(RunId::from_string("run_1"), 3, &[record]);

        assert_eq!(event.kind, "memory.read");
        assert_eq!(event.payload["record_count"], 1);
        assert_eq!(event.payload["records"][0]["key"], "architecture");
        assert!(event.payload.to_string().contains("architecture"));
        assert!(!event
            .payload
            .to_string()
            .contains("Secret architecture note"));
    }

    #[test]
    fn memory_write_proposed_event_uses_bounded_preview() {
        let record = fixture_record(&"x".repeat(600));
        let event = memory_write_proposed_event(RunId::from_string("run_1"), 4, &record);

        assert_eq!(event.kind, "memory.write.proposed");
        assert_eq!(
            event.payload["content_preview"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            MEMORY_WRITE_PREVIEW_LIMIT
        );
        assert_eq!(event.payload["content_truncated"], true);
    }

    fn fixture_record(content: &str) -> MemoryRecord {
        MemoryRecord {
            id: "mem_1".to_owned(),
            scope: MemoryScope::Project,
            key: "architecture".to_owned(),
            content: content.to_owned(),
            tags: vec!["rust".to_owned()],
            evidence_refs: vec![EvidenceRef {
                kind: "doc".to_owned(),
                reference: "docs/rust-migration-map.md".to_owned(),
            }],
            source_ref: Some("memory://project/architecture".to_owned()),
            trust_level: "local".to_owned(),
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("coder-memory-{}-{}-{name}", std::process::id(), id))
    }
}
