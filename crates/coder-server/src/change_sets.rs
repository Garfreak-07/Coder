use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::Path as FsPath,
    process::{Command, Stdio},
};

use axum::{
    extract::{Path, State},
    Json,
};
use coder_core::{FinalReport, RunId};
use coder_store::{RunStore, StoreError};
use coder_tools::{git_diff, GitDiffEvidence};
use serde_json::{json, Value};

use crate::api_types::{
    ChangeSet, ChangeSetActionResponse, ChangeSetDiffResponse, ChangeSetStatus, ChangedFileSummary,
    CommandCheckSummary, RunChangeSetListResponse,
};
use crate::{now_timestamp_string, payload_string, public_preview, stored_run_exists, ApiError};

pub(crate) async fn list_run_changes(
    State(state): State<crate::ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunChangeSetListResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    if !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    let change_set = current_change_set(&state.store, &run_id)?;
    Ok(Json(RunChangeSetListResponse {
        run_id: run_id.to_string(),
        changes: change_set.into_iter().collect(),
    }))
}

pub(crate) async fn get_change_diff(
    State(state): State<crate::ApiState>,
    Path((run_id, change_set_id)): Path<(String, String)>,
) -> Result<Json<ChangeSetDiffResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let change_set = read_change_set(&state.store, &run_id, &change_set_id)?;
    Ok(Json(ChangeSetDiffResponse {
        run_id: run_id.to_string(),
        change_set_id,
        diff: change_set.after_diff,
        truncated: change_set.diff_truncated,
    }))
}

pub(crate) async fn accept_change_set(
    State(state): State<crate::ApiState>,
    Path((run_id, change_set_id)): Path<(String, String)>,
) -> Result<Json<ChangeSetActionResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let mut change_set = read_change_set(&state.store, &run_id, &change_set_id)?;
    change_set.status = ChangeSetStatus::Accepted;
    write_change_set(&state.store, &run_id, &change_set)?;
    append_change_set_event(
        &state.store,
        &run_id,
        "changeset.accepted",
        &change_set.change_set_id,
        json!({"changed_files": &change_set.changed_files}),
    )?;
    Ok(Json(ChangeSetActionResponse {
        run_id: run_id.to_string(),
        change_set,
        status: "accepted".to_owned(),
        message: "Change set accepted.".to_owned(),
    }))
}

pub(crate) async fn undo_change_set(
    State(state): State<crate::ApiState>,
    Path((run_id, change_set_id)): Path<(String, String)>,
) -> Result<Json<ChangeSetActionResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let mut change_set = read_change_set(&state.store, &run_id, &change_set_id)?;
    append_change_set_event(
        &state.store,
        &run_id,
        "changeset.undo.started",
        &change_set.change_set_id,
        json!({}),
    )?;
    let current_diff = git_review_diff_including_untracked(
        &change_set.repo_root,
        usize::MAX,
        change_set.base_git_head.as_deref(),
    )?
    .preview;
    if current_diff != change_set.after_diff {
        let conflict_reason = undo_conflict_summary(&change_set.after_diff, &current_diff);
        change_set.status = ChangeSetStatus::FailedToUndo;
        change_set.undo_conflict = Some(conflict_reason.clone());
        write_change_set(&state.store, &run_id, &change_set)?;
        append_change_set_event(
            &state.store,
            &run_id,
            "changeset.undo.failed",
            &change_set.change_set_id,
            json!({"reason": conflict_reason}),
        )?;
        return Err(ApiError::conflict(conflict_reason));
    }
    apply_reverse_diff(&change_set.repo_root, &change_set.after_diff)?;
    change_set.status = ChangeSetStatus::Undone;
    write_change_set(&state.store, &run_id, &change_set)?;
    append_change_set_event(
        &state.store,
        &run_id,
        "changeset.undo.completed",
        &change_set.change_set_id,
        json!({"changed_files": &change_set.changed_files}),
    )?;
    Ok(Json(ChangeSetActionResponse {
        run_id: run_id.to_string(),
        change_set,
        status: "undone".to_owned(),
        message: "Change set undone with reverse patch.".to_owned(),
    }))
}

fn git_review_diff_including_untracked(
    repo_root: &str,
    max_output_bytes: usize,
    base_git_head: Option<&str>,
) -> Result<GitDiffEvidence, ApiError> {
    let mut diff = GitDiffEvidence {
        repo_root: fs::canonicalize(repo_root)
            .map_err(|error| {
                ApiError::bad_request(format!("repo root '{repo_root}' is invalid: {error}"))
            })?
            .display()
            .to_string(),
        preview: String::new(),
        truncated: false,
        evidence_kind: "repo_evidence".to_owned(),
    };
    if let Some(base_git_head) = base_git_head.filter(|value| !value.trim().is_empty()) {
        let committed = git_diff_from_base(repo_root, base_git_head, max_output_bytes)?;
        append_diff_preview(
            &mut diff,
            committed.preview,
            committed.truncated,
            max_output_bytes,
        );
    }
    let remaining = max_output_bytes.saturating_sub(diff.preview.len());
    if remaining > 0 {
        let working_tree = git_diff(repo_root, remaining)?;
        append_diff_preview(
            &mut diff,
            working_tree.preview,
            working_tree.truncated,
            max_output_bytes,
        );
    } else {
        diff.truncated = true;
    }
    let untracked = git_untracked_files(repo_root)?;
    if untracked.is_empty() {
        return Ok(diff);
    }

    let root = fs::canonicalize(repo_root).map_err(|error| {
        ApiError::bad_request(format!("repo root '{repo_root}' is invalid: {error}"))
    })?;
    let mut preview = diff.preview;
    let mut truncated = diff.truncated;
    for relative_path in untracked {
        if preview.len() >= max_output_bytes {
            truncated = true;
            break;
        }
        let addition =
            synthetic_new_file_diff(&root, &relative_path, max_output_bytes - preview.len())?;
        if addition.truncated {
            truncated = true;
        }
        if !preview.trim().is_empty() && !preview.ends_with('\n') {
            preview.push('\n');
        }
        preview.push_str(&addition.diff);
    }
    diff.preview = preview;
    diff.truncated = truncated;
    Ok(diff)
}

fn git_diff_from_base(
    repo_root: &str,
    base_git_head: &str,
    max_output_bytes: usize,
) -> Result<GitDiffEvidence, ApiError> {
    let output = Command::new("git")
        .args([
            "-C",
            repo_root,
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            &format!("{base_git_head}..HEAD"),
            "--",
        ])
        .output()
        .map_err(|error| ApiError::internal(format!("git diff from base failed: {error}")))?;
    if !output.status.success() {
        return Err(ApiError::internal(format!(
            "git diff from base failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let mut preview = String::from_utf8_lossy(&output.stdout).into_owned();
    let truncated = preview.len() > max_output_bytes;
    if truncated {
        preview.truncate(max_output_bytes);
    }
    Ok(GitDiffEvidence {
        repo_root: repo_root.to_owned(),
        preview,
        truncated,
        evidence_kind: "repo_evidence".to_owned(),
    })
}

fn append_diff_preview(
    diff: &mut GitDiffEvidence,
    addition: String,
    addition_truncated: bool,
    max_output_bytes: usize,
) {
    if addition.trim().is_empty() {
        diff.truncated |= addition_truncated;
        return;
    }
    if !diff.preview.trim().is_empty() && !diff.preview.ends_with('\n') {
        diff.preview.push('\n');
    }
    let remaining = max_output_bytes.saturating_sub(diff.preview.len());
    if addition.len() > remaining {
        diff.preview.push_str(&public_preview(&addition, remaining));
        diff.truncated = true;
    } else {
        diff.preview.push_str(&addition);
        diff.truncated |= addition_truncated;
    }
}

#[derive(Debug)]
struct SyntheticDiff {
    diff: String,
    truncated: bool,
}

fn git_untracked_files(repo_root: &str) -> Result<Vec<String>, ApiError> {
    let output = Command::new("git")
        .args([
            "-C",
            repo_root,
            "ls-files",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .map_err(|error| ApiError::internal(format!("git ls-files failed: {error}")))?;
    if !output.status.success() {
        return Err(ApiError::internal(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.replace('\\', "/"))
        .collect())
}

fn synthetic_new_file_diff(
    root: &FsPath,
    relative_path: &str,
    max_bytes: usize,
) -> Result<SyntheticDiff, ApiError> {
    let normalized_path = relative_path.replace('\\', "/");
    let full_path = root.join(&normalized_path);
    let canonical = fs::canonicalize(&full_path).map_err(|error| {
        ApiError::internal(format!(
            "untracked file '{}' could not be read: {error}",
            normalized_path
        ))
    })?;
    if !canonical.starts_with(root) {
        return Err(ApiError::internal(format!(
            "untracked file '{}' escapes repo root",
            normalized_path
        )));
    }
    let bytes = fs::read(&canonical).map_err(|error| {
        ApiError::internal(format!(
            "untracked file '{}' could not be read: {error}",
            normalized_path
        ))
    })?;
    if bytes.contains(&0) {
        let diff = format!(
            "diff --git a/{0} b/{0}\nnew file mode 100644\nindex 0000000..0000000\nBinary files /dev/null and b/{0} differ\n",
            normalized_path
        );
        return Ok(SyntheticDiff {
            truncated: diff.len() > max_bytes,
            diff: public_preview(&diff, max_bytes),
        });
    }
    let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
    let lines = text.lines().collect::<Vec<_>>();
    let mut diff = format!(
        "diff --git a/{0} b/{0}\nnew file mode 100644\nindex 0000000..0000000\n--- /dev/null\n+++ b/{0}\n@@ -0,0 +1,{1} @@\n",
        normalized_path,
        lines.len()
    );
    let mut truncated = false;
    for line in lines {
        let next = format!("+{line}\n");
        if diff.len() + next.len() > max_bytes {
            truncated = true;
            break;
        }
        diff.push_str(&next);
    }
    Ok(SyntheticDiff { diff, truncated })
}

fn build_current_change_set(
    store: &RunStore,
    run_id: &RunId,
) -> Result<Option<ChangeSet>, ApiError> {
    let events = store.read_events(run_id)?;
    let report = store.read_report(run_id)?.unwrap_or_else(|| {
        store
            .build_evidence_report(run_id)
            .unwrap_or_else(|_| FinalReport::completed("No report available."))
    });
    let Some(repo_root) = repo_root_from_events(&events) else {
        return Ok(None);
    };
    let base_git_head = run_started_payload_string(&events, "git_head");
    let diff =
        git_review_diff_including_untracked(&repo_root, 1024 * 1024, base_git_head.as_deref())?;
    if diff.preview.trim().is_empty() {
        return Ok(None);
    }
    let change_set_id = "changeset-current".to_owned();
    let changed_files = if !report.changed_files.is_empty() {
        report
            .changed_files
            .iter()
            .map(|path| ChangedFileSummary {
                path: path.clone(),
                change_type: "modified".to_owned(),
                additions: None,
                deletions: None,
            })
            .collect()
    } else {
        changed_files_from_diff(&diff.preview)
    };
    let command_checks = report
        .checks
        .iter()
        .filter(|check| !check.starts_with("plan_context:") && !check.starts_with("acceptance:"))
        .map(|check| CommandCheckSummary {
            command: check.clone(),
            status: if check.contains("failed") {
                "failed".to_owned()
            } else {
                "completed".to_owned()
            },
            exit_code: None,
        })
        .collect();
    let before_checkpoint_ref = store
        .list_checkpoints(run_id)?
        .into_iter()
        .find(|checkpoint| checkpoint.name == "before-run.json")
        .map(|checkpoint| checkpoint.checkpoint_ref);
    let after_diff_ref = format!(
        "artifact://runs/{}/artifacts/{}.json",
        run_id.as_str(),
        change_set_id
    );
    let change_set = ChangeSet {
        change_set_id,
        run_id: run_id.to_string(),
        repo_root,
        status: ChangeSetStatus::PendingReview,
        created_at: now_timestamp_string(),
        base_git_head,
        before_checkpoint_ref,
        after_diff_ref: Some(after_diff_ref.clone()),
        reverse_patch_ref: Some(format!("{after_diff_ref}#reverse-git-apply")),
        changed_files,
        command_checks,
        evidence_refs: report.evidence_refs,
        after_diff: diff.preview,
        diff_truncated: diff.truncated,
        undo_conflict: None,
    };
    write_change_set(store, run_id, &change_set)?;
    Ok(Some(change_set))
}

fn current_change_set(store: &RunStore, run_id: &RunId) -> Result<Option<ChangeSet>, ApiError> {
    let Some(stored) = read_stored_change_set(store, run_id, "changeset-current")? else {
        return build_current_change_set(store, run_id);
    };
    let current_diff = git_review_diff_including_untracked(
        &stored.repo_root,
        1024 * 1024,
        stored.base_git_head.as_deref(),
    )?
    .preview;
    if current_diff.trim().is_empty() {
        return Ok(None);
    }
    if current_diff == stored.after_diff || stored.status != ChangeSetStatus::PendingReview {
        return Ok(Some(stored));
    }
    build_current_change_set(store, run_id)
}

fn undo_conflict_summary(recorded_diff: &str, current_diff: &str) -> String {
    let recorded_files = diff_file_set(recorded_diff);
    let current_files = diff_file_set(current_diff);
    let added = current_files
        .difference(&recorded_files)
        .cloned()
        .collect::<Vec<_>>();
    let removed = recorded_files
        .difference(&current_files)
        .cloned()
        .collect::<Vec<_>>();
    let common = recorded_files
        .intersection(&current_files)
        .cloned()
        .collect::<Vec<_>>();
    let mut details = Vec::new();
    if !added.is_empty() {
        details.push(format!(
            "new current diff file(s): {}",
            format_file_set(&added)
        ));
    }
    if !removed.is_empty() {
        details.push(format!(
            "recorded diff file(s) no longer present: {}",
            format_file_set(&removed)
        ));
    }
    if details.is_empty() {
        if !common.is_empty() {
            details.push(format!(
                "diff content changed for: {}",
                format_file_set(&common)
            ));
        } else if current_diff.trim().is_empty() {
            details.push("current diff is empty".to_owned());
        } else {
            details.push("current diff changed shape".to_owned());
        }
    }
    format!(
        "Undo refused because current working-tree diff differs from the recorded review diff; {}.",
        details.join("; ")
    )
}

fn diff_file_set(diff: &str) -> BTreeSet<String> {
    changed_files_from_diff(diff)
        .into_iter()
        .map(|file| file.path)
        .collect()
}

fn format_file_set(files: &[String]) -> String {
    let mut preview = files.iter().take(6).cloned().collect::<Vec<_>>();
    if files.len() > 6 {
        preview.push(format!("+{} more", files.len() - 6));
    }
    preview.join(", ")
}

fn read_stored_change_set(
    store: &RunStore,
    run_id: &RunId,
    change_set_id: &str,
) -> Result<Option<ChangeSet>, ApiError> {
    match store.read_artifact_json(run_id, &change_set_artifact_name(change_set_id)) {
        Ok(value) => Ok(Some(serde_json::from_value(value).map_err(|error| {
            ApiError::internal(format!("stored change set is invalid: {error}"))
        })?)),
        Err(StoreError::ArtifactNotFound { .. }) => Ok(None),
        Err(error) => Err(ApiError::from(error)),
    }
}

fn read_change_set(
    store: &RunStore,
    run_id: &RunId,
    change_set_id: &str,
) -> Result<ChangeSet, ApiError> {
    read_stored_change_set(store, run_id, change_set_id)?.map_or_else(
        || {
            build_current_change_set(store, run_id)?
                .filter(|change_set| change_set.change_set_id == change_set_id)
                .ok_or_else(|| {
                    ApiError::not_found(format!("change set '{change_set_id}' was not found"))
                })
        },
        Ok,
    )
}

fn write_change_set(
    store: &RunStore,
    run_id: &RunId,
    change_set: &ChangeSet,
) -> Result<String, ApiError> {
    Ok(store.write_artifact(
        run_id,
        &change_set_artifact_name(&change_set.change_set_id),
        change_set,
    )?)
}

fn append_change_set_event(
    store: &RunStore,
    run_id: &RunId,
    kind: &str,
    change_set_id: &str,
    mut payload: Value,
) -> Result<(), ApiError> {
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "change_set_id".to_owned(),
            Value::String(change_set_id.to_owned()),
        );
    }
    let sequence = store.event_count(run_id)? as u64 + 1;
    store.append_event(
        run_id,
        &coder_events::CoderEvent::new(run_id.clone(), sequence, kind, payload),
    )?;
    Ok(())
}

fn apply_reverse_diff(repo_root: &str, diff: &str) -> Result<(), ApiError> {
    if diff.trim().is_empty() {
        return Ok(());
    }
    let root = fs::canonicalize(repo_root).map_err(|error| {
        ApiError::bad_request(format!("repo root '{repo_root}' is invalid: {error}"))
    })?;
    if !root.is_dir() {
        return Err(ApiError::bad_request(format!(
            "repo root '{}' is not a directory",
            root.display()
        )));
    }
    run_git_apply_reverse(&root, diff, true)?;
    run_git_apply_reverse(&root, diff, false)
}

fn run_git_apply_reverse(root: &FsPath, diff: &str, check: bool) -> Result<(), ApiError> {
    let mut command = Command::new("git");
    command.arg("apply").arg("-R");
    if check {
        command.arg("--check");
    }
    let mut child = command
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| ApiError::internal(format!("failed to run git apply: {error}")))?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| ApiError::internal("failed to open git apply stdin"))?
        .write_all(diff.as_bytes())
        .map_err(|error| ApiError::internal(format!("failed to write reverse patch: {error}")))?;
    let output = child
        .wait_with_output()
        .map_err(|error| ApiError::internal(format!("failed to wait for git apply: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(ApiError::conflict(format!(
        "reverse patch did not apply cleanly: {}",
        public_preview(&stderr, 1000)
    )))
}

fn change_set_artifact_name(change_set_id: &str) -> String {
    format!("{change_set_id}.json")
}

fn repo_root_from_events(events: &[coder_events::CoderEvent]) -> Option<String> {
    events
        .iter()
        .find(|event| event.kind == "run.started")
        .and_then(|event| payload_string(&event.payload, "repo_root"))
}

fn run_started_payload_string(events: &[coder_events::CoderEvent], key: &str) -> Option<String> {
    events
        .iter()
        .find(|event| event.kind == "run.started")
        .and_then(|event| payload_string(&event.payload, key))
}

fn changed_files_from_diff(diff: &str) -> Vec<ChangedFileSummary> {
    let mut files = Vec::new();
    for line in diff.lines() {
        let path = line.strip_prefix("+++ b/").or_else(|| {
            line.strip_prefix("diff --git ")
                .and_then(|rest| rest.split_whitespace().nth(1))
                .and_then(|path| path.strip_prefix("b/"))
        });
        let Some(path) = path else { continue };
        if path == "/dev/null" {
            continue;
        }
        files.push(ChangedFileSummary {
            path: path.to_owned(),
            change_type: "modified".to_owned(),
            additions: None,
            deletions: None,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files.dedup_by(|left, right| left.path == right.path);
    files
}
