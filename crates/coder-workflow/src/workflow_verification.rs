use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use coder_config::{HarnessSpec, WorkflowNodeSpec};
use coder_core::{FinalReport, ReportStatus, RunId};
use coder_harness::{HarnessRunEvent, HarnessRunResult};
use serde_json::{json, Value};

pub(crate) struct VerificationEventContext<'a> {
    pub(crate) run_id: &'a RunId,
    pub(crate) workflow_id: &'a str,
    pub(crate) round: u32,
    pub(crate) node: &'a WorkflowNodeSpec,
    pub(crate) backend: &'a str,
    pub(crate) harness: &'a HarnessSpec,
}

#[derive(Debug, Clone, Copy)]
struct VerificationEvidenceSummary {
    report_refs: usize,
    patch_refs: usize,
    artifact_refs: usize,
    event_refs: usize,
}

impl VerificationEvidenceSummary {
    fn total(self) -> usize {
        self.report_refs + self.patch_refs + self.artifact_refs + self.event_refs
    }
}

pub(crate) fn enforce_harness_verification(
    result: &mut HarnessRunResult,
    context: VerificationEventContext<'_>,
) {
    if !context.harness.verification.require_evidence
        || !completion_status_requires_evidence(&result.status)
    {
        return;
    }

    result.events.push(verification_event(
        "verification.started",
        &context,
        "started",
        None,
        None,
    ));

    let evidence = verification_evidence_summary(result);
    if evidence.total() > 0 {
        if let Some(report) = result.report.as_mut() {
            report.checks.push(format!(
                "verification: required evidence present (report_refs={}, patch_refs={}, artifact_refs={}, event_refs={})",
                evidence.report_refs, evidence.patch_refs, evidence.artifact_refs, evidence.event_refs
            ));
        }
        result.events.push(verification_event(
            "verification.completed",
            &context,
            "completed",
            Some(evidence),
            None,
        ));
        return;
    }

    let reason =
        "verification requires evidence refs before completion, but the backend returned none";
    result.status = "blocked".to_owned();
    let report = result.report.get_or_insert_with(|| {
        FinalReport::blocked("Harness verification blocked completion.", reason)
    });
    report.status = ReportStatus::Blocked;
    if !report.blockers.iter().any(|blocker| blocker == reason) {
        report.blockers.push(reason.to_owned());
    }
    report
        .checks
        .push("verification: missing required evidence".to_owned());
    report.next_steps.push(
        "Capture repo, command, patch, browser, or artifact evidence before reporting completion."
            .to_owned(),
    );
    result.events.push(verification_event(
        "verification.failed",
        &context,
        "failed",
        Some(evidence),
        Some(reason),
    ));
}

fn completion_status_requires_evidence(status: &str) -> bool {
    matches!(status, "completed" | "finish")
}

fn verification_evidence_summary(result: &HarnessRunResult) -> VerificationEvidenceSummary {
    let (report_refs, patch_refs, artifact_refs) = result
        .report
        .as_ref()
        .map(|report| {
            (
                report.evidence_refs.len(),
                report.patch_refs.len(),
                report.artifact_refs.len(),
            )
        })
        .unwrap_or((0, 0, 0));
    let event_refs = result.events.iter().map(|event| event.refs.len()).sum();
    VerificationEvidenceSummary {
        report_refs,
        patch_refs,
        artifact_refs,
        event_refs,
    }
}

fn verification_event(
    kind: &str,
    context: &VerificationEventContext<'_>,
    status: &str,
    evidence: Option<VerificationEvidenceSummary>,
    reason: Option<&str>,
) -> HarnessRunEvent {
    let mut payload = json!({
        "run_id": context.run_id.as_str(),
        "workflow_id": context.workflow_id,
        "round": context.round,
        "node_id": context.node.id,
        "agent_id": context.node.agent,
        "harness_id": context.node.harness,
        "backend": context.backend,
        "status": status,
        "require_evidence": true
    });
    if let Some(evidence) = evidence {
        payload["evidence"] = json!({
            "report_refs": evidence.report_refs,
            "patch_refs": evidence.patch_refs,
            "artifact_refs": evidence.artifact_refs,
            "event_refs": evidence.event_refs,
            "total_refs": evidence.total()
        });
    }
    if let Some(reason) = reason {
        payload["reason"] = json!(reason);
    }
    HarnessRunEvent::new(kind, payload)
}

pub(crate) fn collect_harness_event_change_metadata(
    event: &HarnessRunEvent,
    repo_root: &Path,
    changed_files: &mut BTreeSet<String>,
) {
    if !harness_event_may_contain_changes(event) {
        return;
    }
    for key in ["files", "changed_files", "touched_files"] {
        collect_changed_file_values(event.payload.get(key), repo_root, changed_files);
    }
    for key in [
        "file",
        "path",
        "filename",
        "filepath",
        "new_path",
        "old_path",
        "target_file",
    ] {
        if let Some(path) = event.payload.get(key).and_then(Value::as_str) {
            push_changed_file_path(changed_files, repo_root, path);
        }
    }
}

fn harness_event_may_contain_changes(event: &HarnessRunEvent) -> bool {
    event.kind == "patch.applied"
        || event.kind.contains("patch")
        || event.payload.get("files").is_some()
        || event.payload.get("changed_files").is_some()
        || event.payload.get("touched_files").is_some()
}

fn collect_changed_file_values(
    value: Option<&Value>,
    repo_root: &Path,
    changed_files: &mut BTreeSet<String>,
) {
    let Some(value) = value else {
        return;
    };
    match value {
        Value::Array(items) => {
            for item in items {
                collect_changed_file_values(Some(item), repo_root, changed_files);
            }
        }
        Value::Object(_) => {
            for key in [
                "path",
                "file",
                "filename",
                "filepath",
                "new_path",
                "old_path",
                "target_file",
            ] {
                if let Some(path) = value.get(key).and_then(Value::as_str) {
                    push_changed_file_path(changed_files, repo_root, path);
                    break;
                }
            }
        }
        Value::String(path) => push_changed_file_path(changed_files, repo_root, path),
        _ => {}
    }
}

fn push_changed_file_path(changed_files: &mut BTreeSet<String>, repo_root: &Path, path: &str) {
    let Some(path) = normalize_changed_file_path(repo_root, path) else {
        return;
    };
    changed_files.insert(path);
}

fn normalize_changed_file_path(repo_root: &Path, path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() || path == "/dev/null" {
        return None;
    }
    let candidate = PathBuf::from(path);
    let normalized = path.replace('\\', "/");
    let relative = if candidate.is_absolute() {
        let repo = normalize_path_for_compare(repo_root);
        let file = normalize_path_for_compare(&candidate);
        if file == repo {
            return None;
        }
        let prefix = format!("{repo}/");
        file.strip_prefix(&prefix)?.to_owned()
    } else {
        normalized
            .trim_start_matches("./")
            .trim_start_matches('/')
            .to_owned()
    };
    if relative.is_empty()
        || relative == "."
        || relative.starts_with("../")
        || relative.contains(":/")
    {
        return None;
    }
    Some(relative)
}

fn normalize_path_for_compare(path: &Path) -> String {
    path.components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_owned()
}
