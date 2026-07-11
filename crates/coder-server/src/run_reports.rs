use axum::{
    extract::{Path, State},
    Json,
};
use coder_core::RunId;
use serde_json::{json, Value};

use crate::api_types::{
    RunReportResponse, RunVerificationEvidenceRequest, RunVerificationEvidenceResponse,
};
use crate::{stored_run_exists, ApiError, ApiState};

pub(crate) async fn preview_run_report(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunReportResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let report = state.store.build_evidence_report(&run_id)?;
    Ok(Json(RunReportResponse {
        run_id: run_id.to_string(),
        report_ref: None,
        report,
    }))
}

pub(crate) async fn write_run_report(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunReportResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    let report = state.store.build_evidence_report(&run_id)?;
    let report_ref = state.store.write_report(&run_id, &report)?;
    Ok(Json(RunReportResponse {
        run_id: run_id.to_string(),
        report_ref: Some(report_ref),
        report,
    }))
}

pub(crate) async fn record_run_verification_evidence(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    Json(request): Json<RunVerificationEvidenceRequest>,
) -> Result<Json<RunVerificationEvidenceResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    if !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    let status = normalize_verification_status(&request.status)?;
    let source = if request.source.trim().is_empty() {
        "external_verification".to_owned()
    } else {
        request.source.trim().to_owned()
    };
    let summary = if request.summary.trim().is_empty() {
        format!("{source} {status}")
    } else {
        request.summary.trim().to_owned()
    };
    let evidence_payload = json!({
        "source": source,
        "status": status,
        "summary": summary,
        "reason": request.reason,
        "remaining_work": request.remaining_work,
        "evidence": request.evidence
    });
    let evidence_text = serde_json::to_string_pretty(&evidence_payload)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let evidence_ref = state.store.write_large_text_ref(&evidence_text)?;
    let mut event_payload = json!({
        "status": status,
        "summary": summary,
        "source": source,
        "evidence": {
            "preview": evidence_ref.preview,
            "truncated": evidence_ref.truncated,
            "blob_ref": evidence_ref.blob_ref
        }
    });
    if let Some(reason) = evidence_payload.get("reason").and_then(Value::as_str) {
        event_payload["reason"] = json!(reason);
    }
    if let Some(remaining_work) = evidence_payload.get("remaining_work") {
        event_payload["remaining_work"] = remaining_work.clone();
    }
    let kind = if status == "completed" {
        "verification.completed"
    } else {
        "verification.failed"
    };
    let sequence = state.store.event_count(&run_id)? as u64 + 1;
    let event = coder_events::CoderEvent::new(run_id.clone(), sequence, kind, event_payload)
        .with_ref("verification_evidence", evidence_ref.blob_ref.clone());
    state.store.append_event(&run_id, &event)?;
    let report = state.store.build_evidence_report(&run_id)?;
    Ok(Json(RunVerificationEvidenceResponse {
        run_id: run_id.to_string(),
        status: status.to_owned(),
        event_count: sequence as usize,
        evidence_ref: evidence_ref.blob_ref,
        report,
    }))
}

fn normalize_verification_status(status: &str) -> Result<&'static str, ApiError> {
    match status.trim().to_ascii_lowercase().as_str() {
        "ok" | "pass" | "passed" | "success" | "succeeded" | "complete" | "completed" => {
            Ok("completed")
        }
        "fail" | "failed" | "failure" | "error" | "errored" => Ok("failed"),
        "blocked" => Ok("blocked"),
        _ => Err(ApiError::bad_request(
            "verification status must be completed, failed, blocked, ok, or passed",
        )),
    }
}
