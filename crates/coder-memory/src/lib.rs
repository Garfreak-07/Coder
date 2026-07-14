use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use coder_core::RunId;
use coder_events::CoderEvent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const MEMORY_WRITE_PREVIEW_LIMIT: usize = 512;
const KNOWLEDGE_CHUNK_CHAR_LIMIT: usize = 3200;
const KNOWLEDGE_RESULT_PREVIEW_LIMIT: usize = 512;
const SECRET_MARKERS: &[&str] = &[
    "deepseek_api_key",
    "llm_api_key",
    "api_key",
    "password",
    "token",
    "begin rsa",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMemoryRole {
    Conversation,
    Task,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPurpose {
    CodingKnowledge,
    ProjectRules,
    ConversationContext,
    ExecutionContext,
    PersonaStyle,
    HistoricalEvidence,
    TaskCheckpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAllowedContext {
    AssistantMessage,
    ConversationState,
    TaskContext,
    ExecutionPrompt,
    FinalReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySensitivity {
    Public,
    Project,
    Private,
    Secret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTrustLevel {
    Source,
    UserConfirmed,
    SystemRecorded,
    ModelInferred,
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
    #[serde(default = "default_memory_trust_level")]
    pub trust_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMemoryFile {
    pub version: u16,
    #[serde(default)]
    pub records: Vec<MemoryRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryAcl {
    pub allowed_agents: Vec<AgentMemoryRole>,
    pub allowed_contexts: Vec<MemoryAllowedContext>,
    #[serde(default = "default_project_sensitivity")]
    pub sensitivity: MemorySensitivity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSource {
    pub source_id: String,
    pub kind: String,
    pub uri: String,
    pub title: String,
    #[serde(default = "default_owner_scope")]
    pub owner_scope: String,
    pub content_hash: String,
    pub imported_at: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeChunk {
    pub chunk_id: String,
    pub source_id: String,
    pub title: String,
    pub text: String,
    pub summary: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub purpose: Vec<MemoryPurpose>,
    pub acl: MemoryAcl,
    #[serde(default = "default_project_sensitivity")]
    pub sensitivity: MemorySensitivity,
    #[serde(default = "default_source_trust_level")]
    pub trust_level: MemoryTrustLevel,
    pub content_hash: String,
    pub embedding_id: Option<String>,
    #[serde(default)]
    pub token_estimate: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeTextImportRequest {
    pub title: String,
    pub text: String,
    #[serde(default = "default_owner_scope")]
    pub owner_scope: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub allowed_agents: Vec<AgentMemoryRole>,
    pub purpose: Vec<MemoryPurpose>,
    #[serde(default)]
    pub allowed_contexts: Vec<MemoryAllowedContext>,
    #[serde(default = "default_project_sensitivity")]
    pub sensitivity: MemorySensitivity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeImportResult {
    pub source: KnowledgeSource,
    pub chunks: Vec<KnowledgeChunk>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalBackendKind {
    #[default]
    Lexical,
    DenseMock,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalIndexResult {
    pub backend: RetrievalBackendKind,
    pub source_id: String,
    pub chunk_count: usize,
}

pub trait RetrievalBackend {
    fn backend(&self) -> RetrievalBackendKind;

    fn index(
        &self,
        source: &KnowledgeSource,
        chunks: &[KnowledgeChunk],
    ) -> Result<RetrievalIndexResult, MemoryError> {
        Ok(RetrievalIndexResult {
            backend: self.backend(),
            source_id: source.source_id.clone(),
            chunk_count: chunks.len(),
        })
    }

    fn retrieve(
        &self,
        chunks: &[KnowledgeChunk],
        request: &KnowledgeRetrievalRequest,
    ) -> Result<Vec<KnowledgeHint>, MemoryError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LexicalRetrievalBackend;

#[derive(Debug, Clone, Copy, Default)]
pub struct DeterministicDenseRetrievalBackend;

#[derive(Debug, Clone, Copy, Default)]
pub struct HybridRetrievalBackend;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRetrievalRequest {
    pub role: AgentMemoryRole,
    pub query: String,
    pub requested_context: MemoryAllowedContext,
    #[serde(default)]
    pub backend: RetrievalBackendKind,
    pub scope: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub token_budget: Option<usize>,
    pub max_results: Option<usize>,
    #[serde(default)]
    pub include_content: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeHint {
    pub id: String,
    pub source_id: String,
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub purpose: Vec<MemoryPurpose>,
    pub evidence_kind: String,
    pub requires_repo_verification: bool,
    pub trust_level: MemoryTrustLevel,
    pub sensitivity: MemorySensitivity,
    pub content_hash: String,
    pub token_estimate: usize,
    pub score: f64,
    #[serde(default)]
    pub backend: RetrievalBackendKind,
    pub content_preview: Option<String>,
    pub content_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRetrievalHit {
    pub source_id: String,
    pub chunk_id: String,
    pub score: f64,
    pub backend: RetrievalBackendKind,
    pub preview: String,
    pub trust_level: MemoryTrustLevel,
    pub evidence_ref: String,
}

impl KnowledgeRetrievalHit {
    pub fn from_hint(hint: &KnowledgeHint) -> Self {
        Self {
            source_id: hint.source_id.clone(),
            chunk_id: hint.id.clone(),
            score: hint.score,
            backend: hint.backend,
            preview: hint
                .content_preview
                .clone()
                .unwrap_or_else(|| hint.summary.clone()),
            trust_level: hint.trust_level,
            evidence_ref: format!("knowledge://{}/{}", hint.source_id, hint.id),
        }
    }
}

#[derive(Debug, Clone)]
pub struct KnowledgeStore {
    root: PathBuf,
}

impl KnowledgeStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn append_source(&self, source: &KnowledgeSource) -> Result<KnowledgeSource, MemoryError> {
        if self
            .list_sources()?
            .iter()
            .any(|existing| existing.source_id == source.source_id)
        {
            return Err(MemoryError::Duplicate(format!(
                "knowledge source already exists: {}",
                source.source_id
            )));
        }
        append_jsonl(&self.sources_path(), source)?;
        Ok(source.clone())
    }

    pub fn append_chunk(&self, chunk: &KnowledgeChunk) -> Result<KnowledgeChunk, MemoryError> {
        validate_knowledge_chunk(chunk)?;
        if self
            .list_chunks(None)?
            .iter()
            .any(|existing| existing.chunk_id == chunk.chunk_id)
        {
            return Err(MemoryError::Duplicate(format!(
                "knowledge chunk already exists: {}",
                chunk.chunk_id
            )));
        }
        append_jsonl(&self.chunks_path(), chunk)?;
        Ok(chunk.clone())
    }

    pub fn list_sources(&self) -> Result<Vec<KnowledgeSource>, MemoryError> {
        read_jsonl(&self.sources_path())
    }

    pub fn list_chunks(&self, source_id: Option<&str>) -> Result<Vec<KnowledgeChunk>, MemoryError> {
        let mut chunks = Vec::new();
        for chunk in read_jsonl::<KnowledgeChunk>(&self.chunks_path())? {
            validate_knowledge_chunk(&chunk)?;
            if source_id.is_none_or(|wanted| wanted == chunk.source_id) {
                chunks.push(chunk);
            }
        }
        Ok(chunks)
    }

    fn sources_path(&self) -> PathBuf {
        self.root.join("knowledge_sources.jsonl")
    }

    fn chunks_path(&self) -> PathBuf {
        self.root.join("knowledge_chunks.jsonl")
    }
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

pub fn append_project_memory_record(
    path: impl AsRef<Path>,
    record: MemoryRecord,
) -> Result<ProjectMemoryFile, MemoryError> {
    validate_memory_record_safety(&record)?;
    let path = path.as_ref();
    let mut file = if path.exists() {
        load_project_memory_file(path)?
    } else {
        ProjectMemoryFile {
            version: 1,
            records: Vec::new(),
        }
    };
    if file.records.iter().any(|existing| existing.id == record.id) {
        return Err(MemoryError::Duplicate(format!(
            "memory record already exists: {}",
            record.id
        )));
    }
    file.records.push(record);
    write_project_memory_file(path, &file)?;
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

pub fn memory_write_confirmed_event(
    run_id: RunId,
    sequence: u64,
    record: &MemoryRecord,
    confirmed_by: AgentMemoryRole,
) -> CoderEvent {
    CoderEvent::new(
        run_id,
        sequence,
        "memory.write.confirmed",
        json!({
            "record": memory_record_summary(record),
            "confirmed_by": confirmed_by,
            "content_persisted": true
        }),
    )
}

pub fn ensure_memory_write_allowed(
    confirmed_by: AgentMemoryRole,
    record: &MemoryRecord,
) -> Result<(), MemoryError> {
    validate_memory_record_safety(record)?;
    if is_long_term_scope(record.scope) && confirmed_by != AgentMemoryRole::Conversation {
        return Err(MemoryError::PolicyViolation(
            "only conversation can confirm long-term memory writes".to_owned(),
        ));
    }
    Ok(())
}

pub fn import_text_knowledge_source(
    store: &KnowledgeStore,
    request: KnowledgeTextImportRequest,
) -> Result<KnowledgeImportResult, MemoryError> {
    let title = request.title.trim();
    if title.is_empty() {
        return Err(MemoryError::Validation(
            "knowledge source title is required".to_owned(),
        ));
    }
    if request.text.trim().is_empty() {
        return Err(MemoryError::Validation(
            "knowledge source text is required".to_owned(),
        ));
    }
    if request.allowed_agents.is_empty() {
        return Err(MemoryError::Validation(
            "knowledge source allowed_agents is required".to_owned(),
        ));
    }
    if request.purpose.is_empty() {
        return Err(MemoryError::Validation(
            "knowledge source purpose is required".to_owned(),
        ));
    }
    reject_secret_like_text(&request.text)?;

    let source_hash = hash_text(&request.text);
    let source_id = format!("knowledge-source-{}", &source_hash[..16]);
    let mut metadata = BTreeMap::new();
    metadata.insert("tags".to_owned(), json!(request.tags));
    metadata.insert("chunker".to_owned(), json!("heading_paragraph_v1"));
    let source = KnowledgeSource {
        source_id: source_id.clone(),
        kind: "manual_note".to_owned(),
        uri: format!("manual:{source_id}"),
        title: title.to_owned(),
        owner_scope: request.owner_scope,
        content_hash: format!("sha256:{source_hash}"),
        imported_at: now_rfc3339(),
        metadata,
    };
    let contexts = if request.allowed_contexts.is_empty() {
        contexts_for_agents(&request.allowed_agents, &request.purpose)
    } else {
        request.allowed_contexts
    };
    let mut chunks = Vec::new();
    for (index, (chunk_title, chunk_text)) in
        chunk_markdown(title, &request.text).into_iter().enumerate()
    {
        let chunk = KnowledgeChunk {
            chunk_id: format!("knowledge-chunk-{}-{}", &source_hash[..16], index + 1),
            source_id: source_id.clone(),
            title: chunk_title,
            text: chunk_text.clone(),
            summary: summarize(&chunk_text),
            tags: source_tags(&source),
            purpose: request.purpose.clone(),
            acl: MemoryAcl {
                allowed_agents: request.allowed_agents.clone(),
                allowed_contexts: contexts.clone(),
                sensitivity: request.sensitivity,
            },
            sensitivity: request.sensitivity,
            trust_level: MemoryTrustLevel::Source,
            content_hash: format!(
                "sha256:{}",
                hash_text(&format!("{source_hash}{chunk_text}{}", index + 1))
            ),
            embedding_id: None,
            token_estimate: token_estimate(&chunk_text),
        };
        validate_knowledge_chunk(&chunk)?;
        chunks.push(chunk);
    }

    let stored_source = store.append_source(&source)?;
    let mut stored_chunks = Vec::new();
    for chunk in &chunks {
        stored_chunks.push(store.append_chunk(chunk)?);
    }
    Ok(KnowledgeImportResult {
        source: stored_source,
        chunks: stored_chunks,
    })
}

pub fn retrieve_knowledge_hints(
    chunks: &[KnowledgeChunk],
    request: &KnowledgeRetrievalRequest,
) -> Result<Vec<KnowledgeHint>, MemoryError> {
    match request.backend {
        RetrievalBackendKind::Lexical => LexicalRetrievalBackend.retrieve(chunks, request),
        RetrievalBackendKind::DenseMock => {
            DeterministicDenseRetrievalBackend.retrieve(chunks, request)
        }
        RetrievalBackendKind::Hybrid => HybridRetrievalBackend.retrieve(chunks, request),
    }
}

impl RetrievalBackend for LexicalRetrievalBackend {
    fn backend(&self) -> RetrievalBackendKind {
        RetrievalBackendKind::Lexical
    }

    fn retrieve(
        &self,
        chunks: &[KnowledgeChunk],
        request: &KnowledgeRetrievalRequest,
    ) -> Result<Vec<KnowledgeHint>, MemoryError> {
        retrieve_ranked_hints(chunks, request, self.backend(), lexical_score_chunk)
    }
}

impl RetrievalBackend for DeterministicDenseRetrievalBackend {
    fn backend(&self) -> RetrievalBackendKind {
        RetrievalBackendKind::DenseMock
    }

    fn retrieve(
        &self,
        chunks: &[KnowledgeChunk],
        request: &KnowledgeRetrievalRequest,
    ) -> Result<Vec<KnowledgeHint>, MemoryError> {
        retrieve_ranked_hints(chunks, request, self.backend(), dense_score_chunk)
    }
}

impl RetrievalBackend for HybridRetrievalBackend {
    fn backend(&self) -> RetrievalBackendKind {
        RetrievalBackendKind::Hybrid
    }

    fn retrieve(
        &self,
        chunks: &[KnowledgeChunk],
        request: &KnowledgeRetrievalRequest,
    ) -> Result<Vec<KnowledgeHint>, MemoryError> {
        retrieve_ranked_hints(chunks, request, self.backend(), hybrid_score_chunk)
    }
}

fn retrieve_ranked_hints(
    chunks: &[KnowledgeChunk],
    request: &KnowledgeRetrievalRequest,
    backend: RetrievalBackendKind,
    scorer: fn(&KnowledgeChunk, &KnowledgeRetrievalRequest) -> f64,
) -> Result<Vec<KnowledgeHint>, MemoryError> {
    if request.query.trim().is_empty() {
        return Err(MemoryError::Validation(
            "knowledge retrieval query is required".to_owned(),
        ));
    }
    let policy = MemoryPolicy::for_role(request.role);
    if !policy.allowed_contexts.contains(&request.requested_context) {
        return Ok(Vec::new());
    }
    let max_results = request.max_results.unwrap_or(policy.max_records);
    let token_budget = request
        .token_budget
        .map(|budget| budget.min(policy.max_tokens))
        .unwrap_or(policy.max_tokens);
    let mut ranked = Vec::new();
    for chunk in chunks {
        if !chunk_allowed(chunk, request, &policy) {
            continue;
        }
        let score = scorer(chunk, request);
        if score <= 0.0 {
            continue;
        }
        let (content_preview, content_truncated) = if request.include_content {
            let (preview, truncated) = preview_text(&chunk.text, KNOWLEDGE_RESULT_PREVIEW_LIMIT);
            (Some(preview), truncated)
        } else {
            (None, false)
        };
        ranked.push(KnowledgeHint {
            id: chunk.chunk_id.clone(),
            source_id: chunk.source_id.clone(),
            title: chunk.title.clone(),
            summary: chunk.summary.clone(),
            tags: chunk.tags.clone(),
            purpose: chunk.purpose.clone(),
            evidence_kind: "knowledge_hint".to_owned(),
            requires_repo_verification: looks_code_like(&request.query)
                || looks_code_like(&chunk.text),
            trust_level: chunk.trust_level,
            sensitivity: chunk.sensitivity,
            content_hash: chunk.content_hash.clone(),
            token_estimate: chunk.token_estimate.max(token_estimate(&chunk.summary)),
            score,
            backend,
            content_preview,
            content_truncated,
        });
    }

    ranked.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.token_estimate.cmp(&right.token_estimate))
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut selected = Vec::new();
    let mut used_tokens = 0usize;
    for hint in ranked {
        if selected.len() >= max_results {
            break;
        }
        let cost = hint.token_estimate.max(1);
        if used_tokens + cost > token_budget {
            continue;
        }
        used_tokens += cost;
        selected.push(hint);
    }
    Ok(selected)
}

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
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
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    PolicyViolation(String),
    #[error("{0}")]
    Duplicate(String),
}

#[derive(Debug)]
struct MemoryPolicy {
    allowed_purposes: BTreeSet<MemoryPurpose>,
    allowed_contexts: BTreeSet<MemoryAllowedContext>,
    max_records: usize,
    max_tokens: usize,
}

impl MemoryPolicy {
    fn for_role(role: AgentMemoryRole) -> Self {
        match role {
            AgentMemoryRole::Conversation => Self {
                allowed_purposes: BTreeSet::from([
                    MemoryPurpose::CodingKnowledge,
                    MemoryPurpose::ProjectRules,
                    MemoryPurpose::ConversationContext,
                    MemoryPurpose::PersonaStyle,
                    MemoryPurpose::HistoricalEvidence,
                    MemoryPurpose::TaskCheckpoint,
                ]),
                allowed_contexts: BTreeSet::from([
                    MemoryAllowedContext::AssistantMessage,
                    MemoryAllowedContext::ConversationState,
                ]),
                max_records: 12,
                max_tokens: 4000,
            },
            AgentMemoryRole::Task => Self {
                allowed_purposes: BTreeSet::from([
                    MemoryPurpose::CodingKnowledge,
                    MemoryPurpose::ProjectRules,
                    MemoryPurpose::ExecutionContext,
                    MemoryPurpose::HistoricalEvidence,
                    MemoryPurpose::TaskCheckpoint,
                ]),
                allowed_contexts: BTreeSet::from([
                    MemoryAllowedContext::ExecutionPrompt,
                    MemoryAllowedContext::TaskContext,
                    MemoryAllowedContext::FinalReport,
                ]),
                max_records: 10,
                max_tokens: 3000,
            },
        }
    }
}

fn write_project_memory_file(path: &Path, file: &ProjectMemoryFile) -> Result<(), MemoryError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| MemoryError::Write {
            path: parent.display().to_string(),
            source,
        })?;
    }
    let text = serde_json::to_string_pretty(file).map_err(|source| MemoryError::Parse {
        path: path.display().to_string(),
        source,
    })?;
    fs::write(path, format!("{text}\n")).map_err(|source| MemoryError::Write {
        path: path.display().to_string(),
        source,
    })
}

fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<(), MemoryError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| MemoryError::Write {
            path: parent.display().to_string(),
            source,
        })?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| MemoryError::Write {
            path: path.display().to_string(),
            source,
        })?;
    let line = serde_json::to_string(value).map_err(|source| MemoryError::Parse {
        path: path.display().to_string(),
        source,
    })?;
    writeln!(file, "{line}").map_err(|source| MemoryError::Write {
        path: path.display().to_string(),
        source,
    })
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, MemoryError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).map_err(|source| MemoryError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        rows.push(
            serde_json::from_str(line).map_err(|source| MemoryError::Parse {
                path: path.display().to_string(),
                source,
            })?,
        );
    }
    Ok(rows)
}

fn chunk_allowed(
    chunk: &KnowledgeChunk,
    request: &KnowledgeRetrievalRequest,
    policy: &MemoryPolicy,
) -> bool {
    if chunk.sensitivity == MemorySensitivity::Secret
        || chunk.acl.sensitivity == MemorySensitivity::Secret
    {
        return false;
    }
    if request.role == AgentMemoryRole::Task
        && (chunk.sensitivity == MemorySensitivity::Private
            || chunk.acl.sensitivity == MemorySensitivity::Private)
    {
        return false;
    }
    if !scope_allows_chunk(request.scope.as_deref(), chunk) {
        return false;
    }
    if !chunk.acl.allowed_agents.contains(&request.role) {
        return false;
    }
    if !chunk
        .acl
        .allowed_contexts
        .contains(&request.requested_context)
    {
        return false;
    }
    chunk
        .purpose
        .iter()
        .all(|purpose| policy.allowed_purposes.contains(purpose))
}

fn lexical_score_chunk(chunk: &KnowledgeChunk, request: &KnowledgeRetrievalRequest) -> f64 {
    let mut searchable = String::new();
    searchable.push_str(&chunk.title);
    searchable.push(' ');
    searchable.push_str(&chunk.summary);
    searchable.push(' ');
    searchable.push_str(&chunk.text);
    searchable.push(' ');
    searchable.push_str(&chunk.tags.join(" "));
    let mut score = term_overlap_score(&request.query, &searchable);
    score += tag_overlap_score(&request.tags, &chunk.tags);
    score += match chunk.trust_level {
        MemoryTrustLevel::UserConfirmed => 1.0,
        MemoryTrustLevel::SystemRecorded => 0.7,
        MemoryTrustLevel::Source => 0.5,
        MemoryTrustLevel::ModelInferred => 0.1,
    };
    score -= (chunk.token_estimate as f64 / 4000.0).min(1.0);
    round_score(score)
}

fn dense_score_chunk(chunk: &KnowledgeChunk, request: &KnowledgeRetrievalRequest) -> f64 {
    let query_vector = deterministic_embedding(&request.query);
    let mut searchable = String::new();
    searchable.push_str(&chunk.title);
    searchable.push(' ');
    searchable.push_str(&chunk.summary);
    searchable.push(' ');
    searchable.push_str(&chunk.text);
    searchable.push(' ');
    searchable.push_str(&chunk.tags.join(" "));
    let chunk_vector = deterministic_embedding(&searchable);
    let semantic_score = cosine_similarity(&query_vector, &chunk_vector);
    if semantic_score <= 0.0 {
        return 0.0;
    }
    let mut score = semantic_score * 4.0;
    score += tag_overlap_score(&request.tags, &chunk.tags);
    score += match chunk.trust_level {
        MemoryTrustLevel::UserConfirmed => 0.7,
        MemoryTrustLevel::SystemRecorded => 0.5,
        MemoryTrustLevel::Source => 0.3,
        MemoryTrustLevel::ModelInferred => 0.05,
    };
    score -= (chunk.token_estimate as f64 / 6000.0).min(0.7);
    round_score(score)
}

fn hybrid_score_chunk(chunk: &KnowledgeChunk, request: &KnowledgeRetrievalRequest) -> f64 {
    let lexical = lexical_score_chunk(chunk, request);
    let dense = dense_score_chunk(chunk, request);
    if lexical <= 0.0 && dense <= 0.0 {
        return 0.0;
    }
    round_score((lexical * 0.55) + (dense * 0.45))
}

fn scope_allows_chunk(scope: Option<&str>, chunk: &KnowledgeChunk) -> bool {
    match scope.unwrap_or("all") {
        "" | "all" | "*" => true,
        "project" => matches!(
            chunk.sensitivity,
            MemorySensitivity::Public | MemorySensitivity::Project
        ),
        "private" => chunk.sensitivity == MemorySensitivity::Private,
        "public" => chunk.sensitivity == MemorySensitivity::Public,
        _ => false,
    }
}

fn term_overlap_score(query: &str, text: &str) -> f64 {
    let query_terms = terms(query).into_iter().collect::<BTreeSet<_>>();
    if query_terms.is_empty() {
        return 0.0;
    }
    let text_terms = terms(text).into_iter().collect::<BTreeSet<_>>();
    let overlap = query_terms.intersection(&text_terms).count() as f64;
    if overlap == 0.0 {
        return 0.0;
    }
    overlap + overlap / (query_terms.len() as f64).sqrt().max(1.0)
}

fn tag_overlap_score(request_tags: &[String], chunk_tags: &[String]) -> f64 {
    let requested = request_tags
        .iter()
        .map(|tag| tag.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let available = chunk_tags
        .iter()
        .map(|tag| tag.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    requested.intersection(&available).count() as f64 * 2.0
}

fn deterministic_embedding(text: &str) -> [f64; 32] {
    let mut vector = [0.0; 32];
    for term in terms(text) {
        let digest = Sha256::digest(term.as_bytes());
        let index = digest[0] as usize % vector.len();
        let sign = if digest[1] % 2 == 0 { 1.0 } else { -1.0 };
        let weight = 1.0 + (digest[2] as f64 / 255.0);
        vector[index] += sign * weight;
    }
    normalize_vector(vector)
}

fn normalize_vector(mut vector: [f64; 32]) -> [f64; 32] {
    let norm = vector.iter().map(|value| value * value).sum::<f64>().sqrt();
    if norm <= f64::EPSILON {
        return vector;
    }
    for value in &mut vector {
        *value /= norm;
    }
    vector
}

fn cosine_similarity(left: &[f64; 32], right: &[f64; 32]) -> f64 {
    round_score(
        left.iter()
            .zip(right.iter())
            .map(|(left, right)| left * right)
            .sum::<f64>(),
    )
}

fn validate_memory_record_safety(record: &MemoryRecord) -> Result<(), MemoryError> {
    if record.id.trim().is_empty() {
        return Err(MemoryError::Validation(
            "memory record id is required".to_owned(),
        ));
    }
    if record.key.trim().is_empty() {
        return Err(MemoryError::Validation(
            "memory record key is required".to_owned(),
        ));
    }
    if record.content.trim().is_empty() {
        return Err(MemoryError::Validation(
            "memory record content is required".to_owned(),
        ));
    }
    reject_secret_like_text(&record.content)
}

fn validate_knowledge_chunk(chunk: &KnowledgeChunk) -> Result<(), MemoryError> {
    if chunk.chunk_id.trim().is_empty() {
        return Err(MemoryError::Validation(
            "knowledge chunk id is required".to_owned(),
        ));
    }
    if chunk.source_id.trim().is_empty() {
        return Err(MemoryError::Validation(
            "knowledge chunk source_id is required".to_owned(),
        ));
    }
    if chunk.acl.allowed_agents.is_empty() {
        return Err(MemoryError::Validation(
            "knowledge chunk allowed_agents is required".to_owned(),
        ));
    }
    if chunk.acl.allowed_contexts.is_empty() {
        return Err(MemoryError::Validation(
            "knowledge chunk allowed_contexts is required".to_owned(),
        ));
    }
    if chunk.sensitivity == MemorySensitivity::Secret && !chunk.acl.allowed_agents.is_empty() {
        return Err(MemoryError::Validation(
            "secret memory must not be retrievable by agents".to_owned(),
        ));
    }
    if chunk.purpose.contains(&MemoryPurpose::PersonaStyle)
        && (chunk.acl.allowed_agents != [AgentMemoryRole::Conversation]
            || chunk.acl.allowed_contexts != [MemoryAllowedContext::AssistantMessage])
    {
        return Err(MemoryError::Validation(
            "persona_style memory is only allowed for conversation assistant_message".to_owned(),
        ));
    }
    if chunk.acl.allowed_agents.contains(&AgentMemoryRole::Task)
        && chunk.purpose.contains(&MemoryPurpose::PersonaStyle)
    {
        return Err(MemoryError::Validation(
            "task cannot receive persona_style memory".to_owned(),
        ));
    }
    reject_secret_like_text(&chunk.text)
}

fn reject_secret_like_text(value: &str) -> Result<(), MemoryError> {
    let lower = value.to_ascii_lowercase();
    for marker in SECRET_MARKERS {
        if lower.contains(marker) {
            return Err(MemoryError::Validation(format!(
                "memory content contains secret-like marker: {marker}"
            )));
        }
    }
    Ok(())
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

fn chunk_markdown(default_title: &str, text: &str) -> Vec<(String, String)> {
    let sections = heading_sections(default_title, text);
    let sections = if sections.is_empty() {
        paragraph_chunks(text)
            .into_iter()
            .map(|chunk| (default_title.to_owned(), chunk))
            .collect()
    } else {
        sections
    };
    let mut chunks = Vec::new();
    for (title, section) in sections {
        if section.chars().count() <= KNOWLEDGE_CHUNK_CHAR_LIMIT {
            chunks.push((title, section.trim().to_owned()));
            continue;
        }
        for (index, chunk) in paragraph_chunks(&section).into_iter().enumerate() {
            chunks.push((format!("{title} ({})", index + 1), chunk));
        }
    }
    chunks
        .into_iter()
        .filter(|(_, chunk)| !chunk.trim().is_empty())
        .collect()
}

fn heading_sections(default_title: &str, text: &str) -> Vec<(String, String)> {
    let mut headings = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(title) = markdown_heading_title(trimmed) {
            headings.push((offset, trimmed.to_owned(), title));
        }
        offset += line.len();
    }
    if headings.is_empty() {
        return Vec::new();
    }

    let mut sections = Vec::new();
    let first_start = headings[0].0;
    let prefix = text[..first_start].trim();
    if !prefix.is_empty() {
        sections.push((default_title.to_owned(), prefix.to_owned()));
    }
    for (index, (start, heading_line, title)) in headings.iter().enumerate() {
        let body_start = start + heading_line.len();
        let end = headings
            .get(index + 1)
            .map(|(next_start, _, _)| *next_start)
            .unwrap_or(text.len());
        let body = text[body_start..end].trim();
        let section = if body.is_empty() {
            heading_line.clone()
        } else {
            format!("{heading_line}\n\n{body}")
        };
        sections.push((title.clone(), section));
    }
    sections
}

fn markdown_heading_title(line: &str) -> Option<String> {
    let hash_count = line.chars().take_while(|ch| *ch == '#').count();
    if !(1..=6).contains(&hash_count) {
        return None;
    }
    if !line
        .chars()
        .nth(hash_count)
        .is_some_and(char::is_whitespace)
    {
        return None;
    }
    let title = line[hash_count..].trim();
    (!title.is_empty()).then(|| title.to_owned())
}

fn paragraph_chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for paragraph in split_paragraphs(text) {
        let candidate = if current.is_empty() {
            paragraph.clone()
        } else {
            format!("{current}\n\n{paragraph}")
        };
        if candidate.chars().count() <= KNOWLEDGE_CHUNK_CHAR_LIMIT {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        if paragraph.chars().count() <= KNOWLEDGE_CHUNK_CHAR_LIMIT {
            current = paragraph;
        } else {
            chunks.extend(split_long_text(&paragraph, KNOWLEDGE_CHUNK_CHAR_LIMIT));
            current = String::new();
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn split_paragraphs(text: &str) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut current = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                paragraphs.push(current.join("\n").trim().to_owned());
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join("\n").trim().to_owned());
    }
    paragraphs
}

fn split_long_text(text: &str, max_chars: usize) -> Vec<String> {
    let chars = text.chars().collect::<Vec<_>>();
    chars
        .chunks(max_chars)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect()
}

fn contexts_for_agents(
    agents: &[AgentMemoryRole],
    purpose: &[MemoryPurpose],
) -> Vec<MemoryAllowedContext> {
    if purpose.contains(&MemoryPurpose::PersonaStyle) {
        return vec![MemoryAllowedContext::AssistantMessage];
    }
    let mut contexts = Vec::new();
    if agents.contains(&AgentMemoryRole::Conversation) {
        contexts.extend([
            MemoryAllowedContext::AssistantMessage,
            MemoryAllowedContext::ConversationState,
        ]);
    }
    if agents.contains(&AgentMemoryRole::Task) {
        contexts.extend([
            MemoryAllowedContext::ExecutionPrompt,
            MemoryAllowedContext::TaskContext,
            MemoryAllowedContext::FinalReport,
        ]);
    }
    let mut seen = BTreeSet::new();
    contexts
        .into_iter()
        .filter(|context| seen.insert(*context))
        .collect()
}

fn summarize(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= 240 {
        return compact;
    }
    let mut summary = compact.chars().take(237).collect::<String>();
    summary = summary.trim_end().to_owned();
    summary.push_str("...");
    summary
}

fn source_tags(source: &KnowledgeSource) -> Vec<String> {
    source
        .metadata
        .get("tags")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn terms(value: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());
        } else if current.len() > 1 {
            terms.push(std::mem::take(&mut current));
        } else {
            current.clear();
        }
    }
    if current.len() > 1 {
        terms.push(current);
    }
    terms
}

fn looks_code_like(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("```")
        || lower.contains(".rs")
        || lower.contains(".py")
        || lower.contains(".ts")
        || lower.contains(".tsx")
        || lower.contains("src/")
        || lower.contains("crates/")
        || lower.contains("fn ")
        || lower.contains("class ")
        || lower.contains("import ")
        || lower.contains("def ")
}

fn token_estimate(text: &str) -> usize {
    (text.chars().count() / 4).max(1)
}

fn preview_text(text: &str, limit: usize) -> (String, bool) {
    let mut chars = text.chars();
    let preview: String = chars.by_ref().take(limit).collect();
    let truncated = chars.next().is_some();
    (preview, truncated)
}

fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

fn round_score(score: f64) -> f64 {
    (score * 1_000_000.0).round() / 1_000_000.0
}

fn is_long_term_scope(scope: MemoryScope) -> bool {
    matches!(
        scope,
        MemoryScope::User
            | MemoryScope::Project
            | MemoryScope::Agent
            | MemoryScope::RepoFacts
            | MemoryScope::KnowledgeHints
            | MemoryScope::ExternalDocs
    )
}

fn default_memory_trust_level() -> String {
    "local".to_owned()
}

fn default_project_sensitivity() -> MemorySensitivity {
    MemorySensitivity::Project
}

fn default_source_trust_level() -> MemoryTrustLevel {
    MemoryTrustLevel::Source
}

fn default_owner_scope() -> String {
    "project".to_owned()
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
                  "evidence_refs": [{"kind": "doc", "reference": "docs/ARCHITECTURE.md"}],
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

    #[test]
    fn only_conversation_can_confirm_long_term_memory_write() {
        let record = fixture_record("Rust owns the control plane.");

        let error = ensure_memory_write_allowed(AgentMemoryRole::Task, &record).unwrap_err();

        assert!(error
            .to_string()
            .contains("only conversation can confirm long-term memory writes"));
        let supervisor_error =
            ensure_memory_write_allowed(AgentMemoryRole::Task, &record).unwrap_err();
        assert!(supervisor_error
            .to_string()
            .contains("only conversation can confirm long-term memory writes"));
        assert!(ensure_memory_write_allowed(AgentMemoryRole::Conversation, &record).is_ok());
    }

    #[test]
    fn confirmed_project_memory_write_persists_record() {
        let path = temp_path("project-memory-write.json");
        let file = append_project_memory_record(&path, fixture_record("Keep Rust API v3 primary."))
            .unwrap();

        assert_eq!(file.records.len(), 1);
        let loaded = load_project_memory_file(&path).unwrap();
        assert_eq!(loaded.records[0].key, "architecture");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn import_text_chunks_markdown_and_preserves_acl_metadata() {
        let root = temp_path("knowledge-store");
        let store = KnowledgeStore::new(&root);
        let result = import_text_knowledge_source(
            &store,
            KnowledgeTextImportRequest {
                title: "Project notes".to_owned(),
                text: "# Architecture\n\nRust owns the control plane.\n\n## Tasks\n\nEach task runs through one agent.".to_owned(),
                owner_scope: "project".to_owned(),
                tags: vec!["rust".to_owned()],
                allowed_agents: vec![
                    AgentMemoryRole::Conversation,
                    AgentMemoryRole::Task,
                ],
                purpose: vec![MemoryPurpose::ProjectRules, MemoryPurpose::ConversationContext],
                allowed_contexts: Vec::new(),
                sensitivity: MemorySensitivity::Project,
            },
        )
        .unwrap();

        assert_eq!(result.source.kind, "manual_note");
        assert_eq!(result.chunks.len(), 2);
        assert_eq!(result.chunks[0].title, "Architecture");
        assert!(result.chunks[0]
            .acl
            .allowed_contexts
            .contains(&MemoryAllowedContext::TaskContext));
        assert_eq!(store.list_sources().unwrap().len(), 1);
        assert_eq!(store.list_chunks(None).unwrap().len(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn knowledge_import_rejects_secret_like_text() {
        let root = temp_path("knowledge-secret-store");
        let store = KnowledgeStore::new(&root);
        let error = import_text_knowledge_source(
            &store,
            KnowledgeTextImportRequest {
                title: "Secret".to_owned(),
                text: "api_key should never be stored".to_owned(),
                owner_scope: "project".to_owned(),
                tags: Vec::new(),
                allowed_agents: vec![AgentMemoryRole::Conversation],
                purpose: vec![MemoryPurpose::ConversationContext],
                allowed_contexts: Vec::new(),
                sensitivity: MemorySensitivity::Project,
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("secret-like marker"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lexical_retrieval_applies_acl_and_returns_hints_only() {
        let root = temp_path("knowledge-retrieve-store");
        let store = KnowledgeStore::new(&root);
        let result = import_text_knowledge_source(
            &store,
            KnowledgeTextImportRequest {
                title: "Rust migration".to_owned(),
                text: "Rust API v3 owns workflow evidence. src/lib.rs must be verified.".to_owned(),
                owner_scope: "project".to_owned(),
                tags: vec!["rust".to_owned()],
                allowed_agents: vec![AgentMemoryRole::Task],
                purpose: vec![MemoryPurpose::ProjectRules],
                allowed_contexts: vec![MemoryAllowedContext::TaskContext],
                sensitivity: MemorySensitivity::Project,
            },
        )
        .unwrap();

        let allowed = retrieve_knowledge_hints(
            &result.chunks,
            &KnowledgeRetrievalRequest {
                role: AgentMemoryRole::Task,
                query: "workflow evidence".to_owned(),
                requested_context: MemoryAllowedContext::TaskContext,
                backend: RetrievalBackendKind::Lexical,
                scope: Some("project".to_owned()),
                tags: vec!["rust".to_owned()],
                token_budget: Some(1000),
                max_results: Some(5),
                include_content: false,
            },
        )
        .unwrap();
        assert_eq!(allowed.len(), 1);
        assert_eq!(allowed[0].evidence_kind, "knowledge_hint");
        assert!(allowed[0].requires_repo_verification);
        assert!(allowed[0].content_preview.is_none());

        let denied = retrieve_knowledge_hints(
            &result.chunks,
            &KnowledgeRetrievalRequest {
                role: AgentMemoryRole::Task,
                query: "workflow evidence".to_owned(),
                requested_context: MemoryAllowedContext::ExecutionPrompt,
                backend: RetrievalBackendKind::Lexical,
                scope: Some("project".to_owned()),
                tags: Vec::new(),
                token_budget: None,
                max_results: None,
                include_content: true,
            },
        )
        .unwrap();
        assert!(denied.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dense_and_hybrid_retrieval_are_deterministic_and_opt_in() {
        let root = temp_path("knowledge-dense-store");
        let store = KnowledgeStore::new(&root);
        let result = import_text_knowledge_source(
            &store,
            KnowledgeTextImportRequest {
                title: "Runtime notes".to_owned(),
                text: "# Rust Runtime\n\nRust native backend owns task execution and run events.\n\n# Recipe\n\nTomato sauce simmers with basil and pasta.".to_owned(),
                owner_scope: "project".to_owned(),
                tags: vec!["rust".to_owned(), "runtime".to_owned()],
                allowed_agents: vec![AgentMemoryRole::Task],
                purpose: vec![MemoryPurpose::ProjectRules],
                allowed_contexts: vec![MemoryAllowedContext::TaskContext],
                sensitivity: MemorySensitivity::Project,
            },
        )
        .unwrap();

        let index = DeterministicDenseRetrievalBackend
            .index(&result.source, &result.chunks)
            .unwrap();
        assert_eq!(index.backend, RetrievalBackendKind::DenseMock);
        assert_eq!(index.chunk_count, result.chunks.len());

        let default_backend = RetrievalBackendKind::default();
        assert_eq!(default_backend, RetrievalBackendKind::Lexical);
        let lexical = retrieve_knowledge_hints(
            &result.chunks,
            &retrieval_request(
                AgentMemoryRole::Task,
                MemoryAllowedContext::TaskContext,
                "native backend run events",
                default_backend,
            ),
        )
        .unwrap();
        let dense = retrieve_knowledge_hints(
            &result.chunks,
            &retrieval_request(
                AgentMemoryRole::Task,
                MemoryAllowedContext::TaskContext,
                "native backend run events",
                RetrievalBackendKind::DenseMock,
            ),
        )
        .unwrap();
        let hybrid = retrieve_knowledge_hints(
            &result.chunks,
            &retrieval_request(
                AgentMemoryRole::Task,
                MemoryAllowedContext::TaskContext,
                "native backend run events",
                RetrievalBackendKind::Hybrid,
            ),
        )
        .unwrap();

        assert_eq!(lexical[0].backend, RetrievalBackendKind::Lexical);
        assert_eq!(dense[0].backend, RetrievalBackendKind::DenseMock);
        assert_eq!(hybrid[0].backend, RetrievalBackendKind::Hybrid);
        assert_eq!(dense[0].title, "Rust Runtime");
        assert_eq!(hybrid[0].title, "Rust Runtime");
        let dense_again = retrieve_knowledge_hints(
            &result.chunks,
            &retrieval_request(
                AgentMemoryRole::Task,
                MemoryAllowedContext::TaskContext,
                "native backend run events",
                RetrievalBackendKind::DenseMock,
            ),
        )
        .unwrap();
        assert_eq!(dense[0].id, dense_again[0].id);
        assert_eq!(dense[0].score, dense_again[0].score);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dense_retrieval_keeps_acl_scope_and_secret_filters() {
        let private_chunk = KnowledgeChunk {
            chunk_id: "private-1".to_owned(),
            source_id: "manual-private".to_owned(),
            title: "Private executor note".to_owned(),
            text: "Executor should not see private workflow notes.".to_owned(),
            summary: "Private workflow notes.".to_owned(),
            tags: vec!["private".to_owned()],
            purpose: vec![MemoryPurpose::ExecutionContext],
            acl: MemoryAcl {
                allowed_agents: vec![AgentMemoryRole::Task],
                allowed_contexts: vec![MemoryAllowedContext::ExecutionPrompt],
                sensitivity: MemorySensitivity::Private,
            },
            sensitivity: MemorySensitivity::Private,
            trust_level: MemoryTrustLevel::UserConfirmed,
            content_hash: "sha256:private".to_owned(),
            embedding_id: None,
            token_estimate: 8,
        };
        let secret_chunk = KnowledgeChunk {
            chunk_id: "secret-1".to_owned(),
            source_id: "manual-secret".to_owned(),
            title: "Secret note".to_owned(),
            text: "api_key should not appear in retrieval output.".to_owned(),
            summary: "Secret note.".to_owned(),
            tags: vec!["secret".to_owned()],
            purpose: vec![MemoryPurpose::ProjectRules],
            acl: MemoryAcl {
                allowed_agents: vec![AgentMemoryRole::Task],
                allowed_contexts: vec![MemoryAllowedContext::TaskContext],
                sensitivity: MemorySensitivity::Secret,
            },
            sensitivity: MemorySensitivity::Secret,
            trust_level: MemoryTrustLevel::UserConfirmed,
            content_hash: "sha256:secret".to_owned(),
            embedding_id: None,
            token_estimate: 8,
        };
        let chunks = vec![private_chunk, secret_chunk];

        let executor_dense = retrieve_knowledge_hints(
            &chunks,
            &KnowledgeRetrievalRequest {
                role: AgentMemoryRole::Task,
                query: "private workflow notes".to_owned(),
                requested_context: MemoryAllowedContext::ExecutionPrompt,
                backend: RetrievalBackendKind::DenseMock,
                scope: Some("private".to_owned()),
                tags: Vec::new(),
                token_budget: None,
                max_results: Some(5),
                include_content: true,
            },
        )
        .unwrap();
        let supervisor_hybrid = retrieve_knowledge_hints(
            &chunks,
            &KnowledgeRetrievalRequest {
                role: AgentMemoryRole::Task,
                query: "api_key retrieval output".to_owned(),
                requested_context: MemoryAllowedContext::TaskContext,
                backend: RetrievalBackendKind::Hybrid,
                scope: Some("all".to_owned()),
                tags: Vec::new(),
                token_budget: None,
                max_results: Some(5),
                include_content: true,
            },
        )
        .unwrap();

        assert!(executor_dense.is_empty());
        assert!(supervisor_hybrid.is_empty());
        assert!(!serde_json::to_string(&supervisor_hybrid)
            .unwrap()
            .contains("api_key"));
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
                reference: "docs/ARCHITECTURE.md".to_owned(),
            }],
            source_ref: Some("memory://project/architecture".to_owned()),
            trust_level: "local".to_owned(),
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        test_tmp_root().join(format!("coder-memory-{}-{}-{name}", std::process::id(), id))
    }

    fn test_tmp_root() -> PathBuf {
        std::env::var_os("CODER_TEST_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }

    fn retrieval_request(
        role: AgentMemoryRole,
        requested_context: MemoryAllowedContext,
        query: &str,
        backend: RetrievalBackendKind,
    ) -> KnowledgeRetrievalRequest {
        KnowledgeRetrievalRequest {
            role,
            query: query.to_owned(),
            requested_context,
            backend,
            scope: Some("project".to_owned()),
            tags: Vec::new(),
            token_budget: Some(1000),
            max_results: Some(5),
            include_content: false,
        }
    }
}
