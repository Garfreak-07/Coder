use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn from_string(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for RunId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowId(String);

impl WorkflowId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for WorkflowId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Completed,
    Blocked,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRequest {
    pub repo_root: String,
    pub task: String,
    pub workflow_id: WorkflowId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub run_id: RunId,
    pub workflow_id: WorkflowId,
    pub status: RunStatus,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl RunState {
    pub fn new(run_id: RunId, workflow_id: WorkflowId) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            run_id,
            workflow_id,
            status: RunStatus::Queued,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Completed,
    Blocked,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub kind: String,
    pub reference: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalReport {
    pub status: ReportStatus,
    pub summary: String,
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub patch_refs: Vec<String>,
    #[serde(default)]
    pub artifact_refs: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<EvidenceRef>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub next_steps: Vec<String>,
}

impl FinalReport {
    pub fn with_status(status: ReportStatus, summary: impl Into<String>) -> Self {
        Self {
            status,
            summary: summary.into(),
            changed_files: Vec::new(),
            checks: Vec::new(),
            patch_refs: Vec::new(),
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            blockers: Vec::new(),
            next_steps: Vec::new(),
        }
    }

    pub fn completed(summary: impl Into<String>) -> Self {
        Self::with_status(ReportStatus::Completed, summary)
    }

    pub fn blocked(summary: impl Into<String>, blocker: impl Into<String>) -> Self {
        let mut report = Self::with_status(ReportStatus::Blocked, summary);
        report.blockers.push(blocker.into());
        report
    }

    pub fn failed(summary: impl Into<String>, blocker: impl Into<String>) -> Self {
        let mut report = Self::with_status(ReportStatus::Failed, summary);
        report.blockers.push(blocker.into());
        report
    }

    pub fn with_check(mut self, check: impl Into<String>) -> Self {
        self.checks.push(check.into());
        self
    }

    pub fn with_evidence(mut self, kind: impl Into<String>, reference: impl Into<String>) -> Self {
        self.evidence_refs.push(EvidenceRef {
            kind: kind.into(),
            reference: reference.into(),
        });
        self
    }

    pub fn refresh_evidence_summary(&mut self, requested: Option<&str>, completed: &[String]) {
        let requested = requested
            .map(|value| compact_summary_text(value, 320))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "No explicit user request was recorded.".to_owned());
        let done = compact_list_or_missing(
            completed,
            "No completed work item was recorded in evidence.",
            4,
        );
        let changed_files =
            compact_list_or_missing(&self.changed_files, "No changed files were recorded.", 6);
        let checks =
            compact_list_or_missing(&self.checks, "No verification or check was recorded.", 6);
        let evidence = evidence_summary(&self.evidence_refs);
        let risks = compact_list_or_missing(
            &self.blockers,
            "No remaining blocker or risk was recorded.",
            5,
        );
        let next_steps = compact_list_or_missing(&self.next_steps, "No next step was recorded.", 5);

        self.summary = format!(
            "Status: {status}\nRequested: {requested}\nDone: {done}\nChanged files: {changed_files}\nVerification: {checks}\nEvidence: {evidence}\nRemaining risks: {risks}\nNext steps: {next_steps}",
            status = report_status_label(self.status)
        );
    }
}

fn report_status_label(status: ReportStatus) -> &'static str {
    match status {
        ReportStatus::Completed => "completed",
        ReportStatus::Blocked => "blocked",
        ReportStatus::Failed => "failed",
        ReportStatus::Cancelled => "cancelled",
    }
}

fn compact_list_or_missing(items: &[String], missing: &str, limit: usize) -> String {
    if items.is_empty() {
        return missing.to_owned();
    }
    let mut values = items
        .iter()
        .take(limit)
        .map(|item| compact_summary_text(item, 180))
        .collect::<Vec<_>>();
    if items.len() > limit {
        values.push(format!("+{} more", items.len() - limit));
    }
    values.join("; ")
}

fn compact_summary_text(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    let mut output = trimmed.chars().take(max_chars).collect::<String>();
    if trimmed.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn evidence_summary(evidence_refs: &[EvidenceRef]) -> String {
    if evidence_refs.is_empty() {
        return "No evidence refs were recorded.".to_owned();
    }
    let mut kinds = evidence_refs
        .iter()
        .map(|reference| reference.kind.as_str())
        .collect::<Vec<_>>();
    kinds.sort_unstable();
    kinds.dedup();
    format!(
        "{} evidence ref(s) recorded: {}.",
        evidence_refs.len(),
        kinds.join(", ")
    )
}

#[derive(Debug, Error)]
pub enum CoderError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("workflow not found: {0}")]
    WorkflowNotFound(String),
    #[error("runtime error: {0}")]
    Runtime(String),
}
