use std::path::PathBuf;

use coder_core::RunId;
use coder_harness::HarnessRunEventRef;
use coder_store::{RepoEvidenceKind, RepoEvidenceRef};
use coder_tools::{
    apply_patch_text, edit_text_file_batch, write_text_file, FileEditBatchRequest,
    FileEditReplacement, FileWriteEvidence, FileWriteRequest, PatchApplyEvidence,
    PatchApplyTextRequest, MODEL_MAX_FILE_EDITS,
};
use coder_workflow::TurnContext;
use serde_json::{json, Value};

use crate::model_tool_hook_runtime::append_model_tool_event_with_refs_checked;
use crate::model_tool_input::model_tool_string;
use crate::model_tool_permissions::model_tool_context_run_id;
use crate::model_tool_run_context::latest_run_context;
use crate::{ApiError, ApiState};

const MODEL_TOOL_MAX_FILE_BYTES: usize = 512 * 1024;

pub(crate) fn execute_write_text_file(
    state: &ApiState,
    input: &Value,
    host_context: &TurnContext,
) -> Result<Value, ApiError> {
    let context = file_operation_context(state, input, host_context)?;
    let path = required_string(input, "path")?;
    let content = required_content(input, "content")?;
    if content.is_empty() {
        return Err(ApiError::bad_request("content is empty"));
    }
    let evidence = write_text_file(
        &context.repo_root,
        FileWriteRequest {
            path: PathBuf::from(path),
            content,
            max_bytes: MODEL_TOOL_MAX_FILE_BYTES,
            source: "model_tool_runtime".to_owned(),
        },
    )
    .map_err(|error| ApiError::bad_request(error.to_string()))?;
    persist_file_change(
        state,
        &context.run_id,
        "write_text_file",
        "full_write",
        evidence,
    )
}

pub(crate) fn execute_edit_text_file(
    state: &ApiState,
    input: &Value,
    host_context: &TurnContext,
) -> Result<Value, ApiError> {
    let context = file_operation_context(state, input, host_context)?;
    let path = required_string(input, "path")?;
    let edits = file_edits(input)?;
    let operation = if edits.len() > 1 {
        "exact_string_edit_batch"
    } else {
        "exact_string_edit"
    };
    let evidence = edit_text_file_batch(
        &context.repo_root,
        FileEditBatchRequest {
            path: PathBuf::from(path),
            edits,
            max_bytes: MODEL_TOOL_MAX_FILE_BYTES,
            source: "model_tool_runtime".to_owned(),
        },
    )
    .map_err(|error| ApiError::bad_request(error.to_string()))?;
    persist_file_change(
        state,
        &context.run_id,
        "edit_text_file",
        operation,
        evidence,
    )
}

pub(crate) fn execute_apply_patch(
    state: &ApiState,
    input: &Value,
    host_context: &TurnContext,
) -> Result<Value, ApiError> {
    let context = file_operation_context(state, input, host_context)?;
    let patch = required_content(input, "patch")?;
    let evidence = apply_patch_text(
        &context.repo_root,
        PatchApplyTextRequest {
            patch,
            max_patch_bytes: input
                .get("max_patch_bytes")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(coder_tools::DEFAULT_MAX_PATCH_BYTES),
            source: "model".to_owned(),
            approved: input
                .get("approved")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
    )
    .map_err(|error| ApiError::bad_request(error.to_string()))?;
    persist_patch_change(state, &context.run_id, evidence)
}

pub(crate) fn execute_finish(input: &Value) -> Result<Value, ApiError> {
    let status = required_string(input, "status")?;
    if !matches!(status.as_str(), "completed" | "blocked") {
        return Err(ApiError::bad_request(
            "finish status must be completed or blocked",
        ));
    }
    let summary = required_string(input, "summary")?;
    Ok(json!({
        "status": status,
        "tool": "finish",
        "summary": summary,
        "checks": string_array(input, "checks"),
        "blockers": string_array(input, "blockers")
    }))
}

struct FileOperationContext {
    run_id: RunId,
    repo_root: String,
}

fn file_operation_context(
    state: &ApiState,
    input: &Value,
    host_context: &TurnContext,
) -> Result<FileOperationContext, ApiError> {
    let run_id = model_tool_context_run_id(input, host_context)
        .ok_or_else(|| ApiError::bad_request("file tool input requires run_id"))?;
    let repo_root = model_tool_string(input, &["repo_root", "repoRoot"])
        .or_else(|| latest_run_context(&state.store, &run_id).and_then(|ctx| ctx.repo_root))
        .ok_or_else(|| ApiError::bad_request("file tool input requires repo_root"))?;
    Ok(FileOperationContext {
        run_id: RunId::from_string(run_id),
        repo_root,
    })
}

fn file_edits(input: &Value) -> Result<Vec<FileEditReplacement>, ApiError> {
    if let Some(items) = input.get("edits") {
        let items = items
            .as_array()
            .ok_or_else(|| ApiError::bad_request("edits must be an array"))?;
        if items.is_empty() {
            return Err(ApiError::bad_request(
                "edits must contain at least one edit",
            ));
        }
        if items.len() > MODEL_MAX_FILE_EDITS {
            return Err(ApiError::bad_request(format!(
                "edits exceeded the maximum of {MODEL_MAX_FILE_EDITS} items"
            )));
        }
        return items.iter().map(file_edit).collect();
    }
    Ok(vec![file_edit(input)?])
}

fn file_edit(input: &Value) -> Result<FileEditReplacement, ApiError> {
    Ok(FileEditReplacement {
        old_string: required_content(input, "old_string")?,
        new_string: required_content(input, "new_string")?,
        replace_all: input
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn persist_file_change(
    state: &ApiState,
    run_id: &RunId,
    tool_name: &'static str,
    operation: &'static str,
    evidence: FileWriteEvidence,
) -> Result<Value, ApiError> {
    let evidence_ref = write_file_evidence(state, run_id, &evidence)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let reference = HarnessRunEventRef {
        label: "repo_evidence".to_owned(),
        uri: format!("repo-evidence://{}", evidence_ref.ref_id),
    };
    append_model_tool_event_with_refs_checked(
        &state.store,
        run_id,
        "file.written",
        json!({
            "backend": "native-rust",
            "implementation": "shared-model-tool-runtime",
            "execution_mode": "tool_loop",
            "tool_name": tool_name,
            "operation": operation,
            "path": &evidence.path,
            "status": &evidence.status,
            "created": evidence.created,
            "bytes_written": evidence.bytes_written,
            "evidence_ref": &evidence_ref.ref_id
        }),
        std::slice::from_ref(&reference),
    )
    .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(json!({
        "status": "completed",
        "tool": tool_name,
        "operation": operation,
        "changed_file": {
            "path": &evidence.path,
            "status": if evidence.created { "added" } else { "modified" },
            "bytes_written": evidence.bytes_written
        },
        "result": evidence,
        "evidence_ref": evidence_ref
    }))
}

fn persist_patch_change(
    state: &ApiState,
    run_id: &RunId,
    evidence: PatchApplyEvidence,
) -> Result<Value, ApiError> {
    let changed_files = evidence
        .preview
        .files
        .iter()
        .filter_map(|file| file.new_path.as_ref().or(file.old_path.as_ref()))
        .cloned()
        .collect::<Vec<_>>();
    let evidence_ref = state
        .store
        .write_repo_evidence(
            run_id,
            RepoEvidenceKind::RepoDiff,
            evidence.repo_root.clone(),
            changed_files.clone(),
            format!(
                "Inline patch {}: {} file(s).",
                evidence.status, evidence.preview.file_count
            ),
            json!({
                "evidence_kind": &evidence.evidence_kind,
                "operation": "apply_patch",
                "result": &evidence
            }),
        )
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let reference = HarnessRunEventRef {
        label: "patch_evidence".to_owned(),
        uri: format!("repo-evidence://{}", evidence_ref.ref_id),
    };
    let event_kind = if evidence.requires_approval {
        "approval.requested"
    } else if evidence.applied {
        "patch.applied"
    } else {
        "patch.failed"
    };
    append_model_tool_event_with_refs_checked(
        &state.store,
        run_id,
        event_kind,
        json!({
            "backend": "native-rust",
            "implementation": "shared-model-tool-runtime",
            "execution_mode": "tool_loop",
            "tool_name": "apply_patch",
            "operation": "atomic_multi_file_patch",
            "approval_type": if evidence.requires_approval { "patch_apply" } else { "" },
            "approval_key": &evidence.approval_key,
            "status": &evidence.status,
            "applied": evidence.applied,
            "requires_approval": evidence.requires_approval,
            "file_count": evidence.preview.file_count,
            "files": &evidence.preview.files,
            "changed_files": &changed_files,
            "evidence_ref": &evidence_ref.ref_id
        }),
        std::slice::from_ref(&reference),
    )
    .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(json!({
        "status": if evidence.requires_approval { "blocked" } else { "completed" },
        "tool": "apply_patch",
        "operation": "atomic_multi_file_patch",
        "changed_files": changed_files,
        "result": evidence,
        "evidence_ref": evidence_ref
    }))
}

fn write_file_evidence(
    state: &ApiState,
    run_id: &RunId,
    evidence: &FileWriteEvidence,
) -> Result<RepoEvidenceRef, coder_store::StoreError> {
    state.store.write_repo_evidence(
        run_id,
        RepoEvidenceKind::RepoDiff,
        evidence.repo_root.clone(),
        vec![evidence.path.clone()],
        format!("Changed file '{}'.", evidence.path),
        json!({
            "evidence_kind": &evidence.evidence_kind,
            "operation": &evidence.evidence_kind,
            "files": [{
                "path": &evidence.path,
                "status": if evidence.created { "added" } else { "modified" }
            }],
            "result": evidence
        }),
    )
}

fn required_string(input: &Value, key: &str) -> Result<String, ApiError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ApiError::bad_request(format!("{key} is required")))
}

fn required_content(input: &Value, key: &str) -> Result<String, ApiError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ApiError::bad_request(format!("{key} is required")))
}

fn string_array(input: &Value, key: &str) -> Vec<String> {
    input
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_edits_accept_batch_and_legacy_shapes() {
        let batch = file_edits(&json!({
            "edits": [
                {"old_string": "a", "new_string": "b"},
                {"old_string": "c", "new_string": "d", "replace_all": true}
            ]
        }))
        .unwrap();
        assert_eq!(batch.len(), 2);
        assert!(batch[1].replace_all);

        let legacy = file_edits(&json!({
            "old_string": "a",
            "new_string": "b",
            "replace_all": false
        }))
        .unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].old_string, "a");
    }

    #[test]
    fn finish_requires_canonical_status() {
        assert_eq!(
            execute_finish(&json!({"status": "completed", "summary": "done"})).unwrap()["status"],
            "completed"
        );
        assert!(execute_finish(&json!({"status": "done", "summary": "done"})).is_err());
    }
}
