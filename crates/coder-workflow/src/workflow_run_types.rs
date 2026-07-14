use std::{path::PathBuf, process::Command};

use coder_core::{FinalReport, RunId, RunStatus};
use coder_events::CoderEvent;
use serde_json::Value;
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowRunControl {
    Running,
    Paused,
    Cancelled,
}

pub fn replay_run_status(events: &[CoderEvent]) -> Option<RunStatus> {
    events
        .iter()
        .rev()
        .find_map(|event| match event.kind.as_str() {
            "run.completed" => Some(RunStatus::Completed),
            "run.blocked" => Some(RunStatus::Blocked),
            "run.failed" => Some(RunStatus::Failed),
            "run.cancelled" => Some(RunStatus::Cancelled),
            _ => None,
        })
}

pub(crate) fn git_head(repo_root: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", repo_root, "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let head = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if head.is_empty() {
        None
    } else {
        Some(head)
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowRunOptions {
    pub run_id: Option<RunId>,
    pub workflow_id: String,
    pub task: String,
    pub repo_root: PathBuf,
    pub dry_run: bool,
    pub task_context: Option<Value>,
    pub control: Option<watch::Receiver<WorkflowRunControl>>,
}

impl WorkflowRunOptions {
    pub fn new(workflow_id: impl Into<String>, task: impl Into<String>) -> Self {
        Self {
            run_id: None,
            workflow_id: workflow_id.into(),
            task: task.into(),
            repo_root: PathBuf::from("."),
            dry_run: false,
            task_context: None,
            control: None,
        }
    }
}

#[derive(Debug)]
pub struct WorkflowRunOutput {
    pub run_id: RunId,
    pub report: FinalReport,
    pub report_ref: String,
}
