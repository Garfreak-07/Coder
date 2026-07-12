use axum::{extract::State, Json};
use coder_core::RunId;
use coder_events::redact_payload;
use coder_store::{
    redact_repo_evidence_payload, RepoEvidenceKind, RepoEvidenceRef, RunStore, StoreError,
};
use coder_tools::{
    apply_patch_file, builtin_tool, find_files, git_diff, git_status, preview_command,
    preview_patch_file, read_file, read_file_range, search_text, CommandPreview,
    CommandRunEvidence, PatchApplyEvidence, PatchApplyRequest as ToolPatchApplyRequest,
    PatchPreviewEvidence, RepoToolConfig,
};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, time::Duration};

use crate::background_commands::{background_command_status, start_background_command_task};
use crate::{
    ApiError, ApiState, CommandBackgroundStartRequest, CommandRunResponse, CommandRunToolRequest,
    GitDiffRequest, GitDiffResponse, GitStatusRequest, GitStatusResponse, PatchApplyResponse,
    PatchApplyToolRequest, PatchPreviewRequest, RepoFindFilesRequest, RepoFindFilesResponse,
    RepoReadFileRangeRequest, RepoReadFileRangeResponse, RepoReadFileRequest, RepoReadFileResponse,
    RepoSearchTextRequest, RepoSearchTextResponse,
};

pub(crate) fn ensure_tool_boundary(tool_name: &str) -> Result<(), ApiError> {
    builtin_tool(tool_name)
        .ok_or_else(|| ApiError::forbidden(format!("tool '{tool_name}' is not registered")))?;
    Ok(())
}

pub(crate) async fn preview_command_endpoint(
    Json(request): Json<crate::CommandPreviewRequest>,
) -> Result<Json<CommandPreview>, ApiError> {
    ensure_tool_boundary("run_command_sandbox")?;
    let preview = preview_command(
        &request.repo_root,
        request.cwd.unwrap_or_else(|| ".".to_owned()),
        request.argv,
        request.source.as_deref().unwrap_or("model"),
        request.sandbox.unwrap_or(false),
    )?;
    Ok(Json(preview))
}

pub(crate) async fn run_command_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<CommandRunToolRequest>,
) -> Result<Json<CommandRunResponse>, ApiError> {
    let CommandRunToolRequest {
        repo_root,
        cwd,
        argv,
        timeout_seconds,
        foreground_timeout_seconds,
        background_on_timeout,
        max_output_bytes,
        interactive,
        source,
        sandbox,
        approved,
        run_id,
    } = request;
    ensure_tool_boundary("run_command_sandbox")?;
    let cwd = cwd.unwrap_or_else(|| ".".to_owned());
    let source = source.unwrap_or_else(|| "model".to_owned());
    let sandbox = sandbox.unwrap_or(false);
    let approved = approved.unwrap_or(false);
    let max_output_bytes =
        max_output_bytes.unwrap_or(coder_tools::DEFAULT_MAX_COMMAND_OUTPUT_BYTES);

    ensure_tool_boundary("command_background")?;
    let allow_background = background_on_timeout.unwrap_or(false);
    let process_timeout_seconds = if allow_background {
        timeout_seconds
    } else {
        Some(timeout_seconds.unwrap_or(coder_tools::DEFAULT_COMMAND_TIMEOUT_SECONDS))
    };
    let background_task = start_background_command_task(
        &state,
        CommandBackgroundStartRequest {
            repo_root: repo_root.clone(),
            cwd: Some(cwd.clone()),
            argv: argv.clone(),
            timeout_seconds: process_timeout_seconds,
            max_output_bytes: Some(max_output_bytes),
            interactive,
            source: Some(source.clone()),
            sandbox: Some(sandbox),
            approved: Some(approved),
            run_id,
        },
    )?;
    let foreground_deadline = allow_background.then(|| {
        let foreground_timeout_seconds = foreground_timeout_seconds
            .unwrap_or(coder_tools::DEFAULT_COMMAND_TIMEOUT_SECONDS)
            .clamp(1, coder_tools::MAX_COMMAND_TIMEOUT_SECONDS);
        tokio::time::Instant::now() + Duration::from_secs(foreground_timeout_seconds)
    });

    loop {
        let status = background_command_status(&state, &background_task.task_id)?;
        if status.status != "running" {
            if let Some(result) = status.result {
                return Ok(Json(CommandRunResponse {
                    evidence_ref: status.evidence_ref,
                    result,
                    background_task: None,
                }));
            }
            return Err(ApiError::internal(format!(
                "command process {} reached terminal status '{}' without a result",
                background_task.task_id, status.status
            )));
        }
        if foreground_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            let status = background_command_status(&state, &background_task.task_id)?;
            if status.status != "running" {
                if let Some(result) = status.result {
                    return Ok(Json(CommandRunResponse {
                        evidence_ref: status.evidence_ref,
                        result,
                        background_task: None,
                    }));
                }
            }
            let preview = preview_command(&repo_root, &cwd, argv, &source, sandbox)?;
            let output = if status.output_preview.trim().is_empty() {
                format!(
                    "Command is still running in background task {}. Read {} for output or delete {} to cancel.",
                    background_task.task_id, background_task.output_url, background_task.cancel_url
                )
            } else {
                format!(
                    "Command is still running in background task {}. Read {} for output or delete {} to cancel.\n\nRecent output:\n{}",
                    background_task.task_id,
                    background_task.output_url,
                    background_task.cancel_url,
                    status.output_preview
                )
            };
            return Ok(Json(CommandRunResponse {
                evidence_ref: None,
                result: CommandRunEvidence {
                    repo_root: preview.repo_root,
                    cwd: preview.cwd,
                    argv: preview.argv,
                    command: preview.command,
                    status: "backgrounded".to_owned(),
                    passed: false,
                    blocked: false,
                    requires_approval: false,
                    approval_key: preview.approval_key,
                    returncode: None,
                    output,
                    output_truncated: status.output_truncated,
                    timed_out: false,
                    policy: preview.policy,
                    evidence_kind: "command_evidence".to_owned(),
                },
                background_task: Some(background_task),
            }));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub(crate) async fn repo_find_files_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<RepoFindFilesRequest>,
) -> Result<Json<RepoFindFilesResponse>, ApiError> {
    ensure_tool_boundary("search_files")?;
    let files = find_files(
        &request.repo_root,
        request.query.as_deref(),
        &request.extensions.unwrap_or_default(),
        request
            .max_results
            .unwrap_or(coder_tools::DEFAULT_MAX_FILE_RESULTS),
    )?;
    let evidence_ref = write_tool_evidence(
        &state.store,
        request.run_id.as_deref(),
        RepoEvidenceKind::RepoFileList,
        &request.repo_root,
        format!("Found {} file(s).", files.len()),
        json!({
            "evidence_kind": "repo_evidence",
            "operation": "find_files",
            "files": serde_json::to_value(&files).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    Ok(Json(RepoFindFilesResponse {
        evidence_ref,
        files,
    }))
}

pub(crate) async fn repo_search_text_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<RepoSearchTextRequest>,
) -> Result<Json<RepoSearchTextResponse>, ApiError> {
    ensure_tool_boundary("search_files")?;
    let matches = search_text(
        &request.repo_root,
        &request.query,
        &RepoToolConfig {
            max_file_bytes: request
                .max_file_bytes
                .unwrap_or(coder_tools::DEFAULT_MAX_FILE_BYTES),
            max_search_matches: request
                .max_matches
                .unwrap_or(coder_tools::DEFAULT_MAX_SEARCH_MATCHES),
        },
    )?;
    let evidence_ref = write_tool_evidence(
        &state.store,
        request.run_id.as_deref(),
        RepoEvidenceKind::RepoTextSearch,
        &request.repo_root,
        format!("Found {} text match(es).", matches.len()),
        json!({
            "evidence_kind": "repo_evidence",
            "operation": "search_text",
            "query": request.query,
            "matches": serde_json::to_value(&matches).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    Ok(Json(RepoSearchTextResponse {
        evidence_ref,
        matches,
    }))
}

pub(crate) async fn repo_read_file_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<RepoReadFileRequest>,
) -> Result<Json<RepoReadFileResponse>, ApiError> {
    ensure_tool_boundary("read_file")?;
    let file = read_file(
        &request.repo_root,
        PathBuf::from(&request.path),
        &RepoToolConfig {
            max_file_bytes: request
                .max_file_bytes
                .unwrap_or(coder_tools::DEFAULT_MAX_FILE_BYTES),
            max_search_matches: coder_tools::DEFAULT_MAX_SEARCH_MATCHES,
        },
    )?;
    let evidence_ref = write_tool_evidence(
        &state.store,
        request.run_id.as_deref(),
        RepoEvidenceKind::RepoRead,
        &request.repo_root,
        format!("Read file '{}'.", file.path),
        json!({
            "evidence_kind": "repo_evidence",
            "operation": "read_file",
            "file": serde_json::to_value(&file).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    Ok(Json(RepoReadFileResponse { evidence_ref, file }))
}

pub(crate) async fn repo_read_file_range_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<RepoReadFileRangeRequest>,
) -> Result<Json<RepoReadFileRangeResponse>, ApiError> {
    ensure_tool_boundary("read_file")?;
    let snippet = read_file_range(
        &request.repo_root,
        PathBuf::from(&request.path),
        request.start_line.unwrap_or(1),
        request.max_lines.unwrap_or(120),
        request.max_chars.unwrap_or(16_000),
    )?;
    let evidence_ref = write_tool_evidence(
        &state.store,
        request.run_id.as_deref(),
        RepoEvidenceKind::RepoRead,
        &request.repo_root,
        format!("Read file range '{}'.", snippet.path),
        json!({
            "evidence_kind": "repo_evidence",
            "operation": "read_file_range",
            "snippet": serde_json::to_value(&snippet).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    Ok(Json(RepoReadFileRangeResponse {
        evidence_ref,
        snippet,
    }))
}

pub(crate) async fn git_status_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<GitStatusRequest>,
) -> Result<Json<GitStatusResponse>, ApiError> {
    ensure_tool_boundary("inspect_git_diff")?;
    let status = git_status(&request.repo_root)?;
    let evidence_ref = write_tool_evidence(
        &state.store,
        request.run_id.as_deref(),
        RepoEvidenceKind::RepoDiff,
        &request.repo_root,
        "Captured git status.",
        json!({
            "evidence_kind": "repo_evidence",
            "operation": "git_status",
            "status": serde_json::to_value(&status).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    Ok(Json(GitStatusResponse {
        evidence_ref,
        status,
    }))
}

pub(crate) async fn git_diff_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<GitDiffRequest>,
) -> Result<Json<GitDiffResponse>, ApiError> {
    ensure_tool_boundary("inspect_git_diff")?;
    let diff = git_diff(
        &request.repo_root,
        request
            .max_output_bytes
            .unwrap_or(coder_tools::DEFAULT_MAX_GIT_OUTPUT_BYTES),
    )?;
    let evidence_ref = write_tool_evidence(
        &state.store,
        request.run_id.as_deref(),
        RepoEvidenceKind::RepoDiff,
        &request.repo_root,
        "Captured git diff.",
        json!({
            "evidence_kind": "repo_evidence",
            "operation": "git_diff",
            "diff": serde_json::to_value(&diff).map_err(|error| ApiError::internal(error.to_string()))?
        }),
    )?;
    Ok(Json(GitDiffResponse { evidence_ref, diff }))
}

pub(crate) async fn preview_patch_endpoint(
    Json(request): Json<PatchPreviewRequest>,
) -> Result<Json<PatchPreviewEvidence>, ApiError> {
    ensure_tool_boundary("propose_patch")?;
    let preview = preview_patch_file(
        &request.repo_root,
        PathBuf::from(&request.patch_file),
        request
            .max_patch_bytes
            .unwrap_or(coder_tools::DEFAULT_MAX_PATCH_BYTES),
    )?;
    Ok(Json(preview))
}

pub(crate) async fn apply_patch_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<PatchApplyToolRequest>,
) -> Result<Json<PatchApplyResponse>, ApiError> {
    ensure_tool_boundary("apply_patch_sandbox")?;
    let run_id = request
        .run_id
        .as_deref()
        .map(RunId::from_string)
        .ok_or_else(|| ApiError::bad_request("run_id is required for patch apply"))?;
    let result = apply_patch_file(
        &request.repo_root,
        ToolPatchApplyRequest {
            patch_file: PathBuf::from(&request.patch_file),
            max_patch_bytes: request
                .max_patch_bytes
                .unwrap_or(coder_tools::DEFAULT_MAX_PATCH_BYTES),
            source: request.source.unwrap_or_else(|| "model".to_owned()),
            approved: request.approved.unwrap_or(false),
        },
    )?;
    let result_json =
        serde_json::to_value(&result).map_err(|error| ApiError::internal(error.to_string()))?;
    let evidence_ref = state.store.write_repo_evidence(
        &run_id,
        RepoEvidenceKind::RepoDiff,
        result.repo_root.clone(),
        Vec::new(),
        format!(
            "Patch apply {}: {} file(s).",
            result.status, result.preview.file_count
        ),
        json!({
            "evidence_kind": "patch_apply",
            "operation": "patch_apply",
            "result": result_json,
        }),
    )?;
    record_patch_apply_event(&state.store, &run_id, &result, &evidence_ref)?;
    Ok(Json(PatchApplyResponse {
        run_id: run_id.to_string(),
        evidence_ref,
        result,
    }))
}

pub(crate) fn write_tool_evidence(
    store: &RunStore,
    run_id: Option<&str>,
    kind: RepoEvidenceKind,
    repo_root: &str,
    summary: impl Into<String>,
    payload: Value,
) -> Result<Option<RepoEvidenceRef>, ApiError> {
    let Some(run_id) = run_id else {
        return Ok(None);
    };
    let repo_root = fs::canonicalize(repo_root)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| repo_root.to_owned());
    let reference = store.write_repo_evidence(
        &RunId::from_string(run_id.to_owned()),
        kind,
        repo_root,
        Vec::new(),
        summary,
        redact_repo_evidence_payload(redact_payload(payload)),
    )?;
    Ok(Some(reference))
}

pub(crate) fn record_command_events(
    store: &RunStore,
    run_id: &RunId,
    output: &CommandRunEvidence,
    evidence_ref: &RepoEvidenceRef,
) -> Result<(), StoreError> {
    let mut sequence = store.event_count(run_id)? as u64 + 1;
    let evidence_uri = format!("repo-evidence://{}", evidence_ref.ref_id);
    if output.blocked && output.requires_approval {
        store.append_event(
            run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                sequence,
                "approval.requested",
                json!({
                    "approval_type": "command",
                    "approval_key": &output.approval_key,
                    "command": &output.command,
                    "cwd": &output.cwd,
                    "policy": &output.policy,
                    "evidence_ref": &evidence_ref.ref_id,
                }),
            )
            .with_ref("command_evidence", evidence_uri),
        )?;
        return Ok(());
    }

    store.append_event(
        run_id,
        &coder_events::CoderEvent::new(
            run_id.clone(),
            sequence,
            "command.started",
            json!({
                "command": &output.command,
                "argv": &output.argv,
                "cwd": &output.cwd,
                "approval_key": &output.approval_key,
                "policy": &output.policy,
                "evidence_ref": &evidence_ref.ref_id,
            }),
        )
        .with_ref("command_evidence", evidence_uri.clone()),
    )?;
    sequence += 1;
    let kind = match output.status.as_str() {
        "completed" => "command.completed",
        _ => "command.failed",
    };
    store.append_event(
        run_id,
        &coder_events::CoderEvent::new(
            run_id.clone(),
            sequence,
            kind,
            json!({
                "command": &output.command,
                "cwd": &output.cwd,
                "status": &output.status,
                "passed": output.passed,
                "returncode": output.returncode,
                "timed_out": output.timed_out,
                "output_preview": &output.output,
                "output_truncated": output.output_truncated,
                "evidence_ref": &evidence_ref.ref_id,
            }),
        )
        .with_ref("command_evidence", evidence_uri),
    )?;
    Ok(())
}

fn record_patch_apply_event(
    store: &RunStore,
    run_id: &RunId,
    output: &PatchApplyEvidence,
    evidence_ref: &RepoEvidenceRef,
) -> Result<(), StoreError> {
    let sequence = store.event_count(run_id)? as u64 + 1;
    let evidence_uri = format!("repo-evidence://{}", evidence_ref.ref_id);
    if output.requires_approval {
        store.append_event(
            run_id,
            &coder_events::CoderEvent::new(
                run_id.clone(),
                sequence,
                "approval.requested",
                json!({
                    "approval_type": "patch_apply",
                    "approval_key": &output.approval_key,
                    "patch_file": &output.patch_file,
                    "reason": &output.reason,
                    "files": &output.preview.files,
                    "evidence_ref": &evidence_ref.ref_id,
                }),
            )
            .with_ref("patch_evidence", evidence_uri),
        )?;
        return Ok(());
    }

    let kind = if output.applied {
        "patch.applied"
    } else {
        "patch.failed"
    };
    store.append_event(
        run_id,
        &coder_events::CoderEvent::new(
            run_id.clone(),
            sequence,
            kind,
            json!({
                "status": &output.status,
                "patch_file": &output.patch_file,
                "applied": output.applied,
                "reason": &output.reason,
                "approval_key": &output.approval_key,
                "file_count": output.preview.file_count,
                "files": &output.preview.files,
                "evidence_ref": &evidence_ref.ref_id,
            }),
        )
        .with_ref("patch_evidence", evidence_uri),
    )?;
    Ok(())
}
