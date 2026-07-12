use coder_core::{FinalReport, ReportStatus, RunId, RunStatus};

pub(crate) struct WorkflowReportInput<'a> {
    pub(crate) run_id: &'a RunId,
    pub(crate) workflow_id: &'a str,
    pub(crate) request: &'a str,
    pub(crate) status: RunStatus,
    pub(crate) reason: Option<&'a str>,
    pub(crate) dispatched_nodes: usize,
    pub(crate) checks: Vec<String>,
    pub(crate) evidence_refs: Vec<coder_core::EvidenceRef>,
    pub(crate) blockers: Vec<String>,
    pub(crate) changed_files: Vec<String>,
    pub(crate) patch_refs: Vec<String>,
}

pub(crate) fn workflow_run_report(input: WorkflowReportInput<'_>) -> FinalReport {
    let report_status = match input.status {
        RunStatus::Completed => ReportStatus::Completed,
        RunStatus::Blocked => ReportStatus::Blocked,
        RunStatus::Failed | RunStatus::Queued | RunStatus::Running => ReportStatus::Failed,
        RunStatus::Cancelled => ReportStatus::Cancelled,
    };
    let mut report = FinalReport::with_status(
        report_status,
        format!(
            "Workflow '{workflow_id}' finished with status '{}' after dispatching {} node(s).",
            run_status_str(input.status),
            input.dispatched_nodes,
            workflow_id = input.workflow_id
        ),
    );
    report.checks = input.checks;
    report.blockers = input.blockers;
    report.changed_files = input.changed_files;
    report.patch_refs = input.patch_refs;
    if report.blockers.is_empty() {
        if let Some(reason) = input.reason {
            report.blockers.push(reason.to_owned());
        }
    }
    let mut evidence_refs = input.evidence_refs;
    evidence_refs.push(coder_core::EvidenceRef {
        kind: "event_log".to_owned(),
        reference: format!("eventlog://runs/{}/events.jsonl", input.run_id.as_str()),
    });
    evidence_refs.sort_by(|left, right| {
        (left.kind.as_str(), left.reference.as_str())
            .cmp(&(right.kind.as_str(), right.reference.as_str()))
    });
    evidence_refs
        .dedup_by(|left, right| left.kind == right.kind && left.reference == right.reference);
    report.evidence_refs = evidence_refs;
    let mut completed = report.checks.clone();
    if completed.is_empty() && input.dispatched_nodes > 0 {
        completed.push(format!("Dispatched {} node(s).", input.dispatched_nodes));
    }
    report.refresh_planner_style_summary(Some(input.request), &completed);
    report
}

pub(crate) fn run_status_str(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Queued => "queued",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Blocked => "blocked",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}
