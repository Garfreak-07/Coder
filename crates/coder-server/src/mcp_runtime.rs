use axum::{extract::State, Json};
use coder_core::RunId;
use coder_harness::{
    find_mock_mcp_tool, invoke_mock_mcp_tool, mock_mcp_servers, mock_mcp_tools,
    validate_mcp_manifest, McpManifestValidation, McpToolCallRequest, McpToolCallResult,
};
use coder_store::{RunStore, StoreError};
use serde_json::{json, Value};

use crate::{
    api_types::{McpManifestValidationRequest, McpServerListResponse, McpToolListResponse},
    stored_run_exists, ApiError, ApiState,
};

const MCP_OUTPUT_INLINE_LIMIT: usize = 1024;

pub(crate) async fn validate_mcp(
    Json(request): Json<McpManifestValidationRequest>,
) -> Json<McpManifestValidation> {
    Json(validate_mcp_manifest(&request.manifest))
}

pub(crate) async fn list_mcp_servers() -> Json<McpServerListResponse> {
    Json(McpServerListResponse {
        servers: mock_mcp_servers(),
    })
}

pub(crate) async fn list_mcp_tools() -> Json<McpToolListResponse> {
    Json(McpToolListResponse {
        tools: mock_mcp_tools(),
    })
}

pub(crate) async fn invoke_mcp_tool(
    State(state): State<ApiState>,
    Json(request): Json<McpToolCallRequest>,
) -> Result<Json<McpToolCallResult>, ApiError> {
    if let Some(run_id) = &request.run_id {
        if !stored_run_exists(&state.store, run_id)? {
            return Err(ApiError::not_found(format!(
                "run '{}' was not found",
                run_id.as_str()
            )));
        }
        append_mcp_event(
            &state.store,
            run_id,
            "mcp.server.registered",
            json!({
                "server_id": request.server_id.as_str(),
                "enabled": false,
                "requires_approval": true
            }),
            None,
        )?;
        let discovered = find_mock_mcp_tool(&request.server_id, &request.tool_name);
        append_mcp_event(
            &state.store,
            run_id,
            "mcp.tool.discovered",
            json!({
                "server_id": request.server_id.as_str(),
                "tool_name": request.tool_name.as_str(),
                "discovered": discovered.is_some(),
                "enabled": false,
                "requires_approval": true,
                "risk": discovered.as_ref().map(|tool| tool.risk),
                "side_effect": discovered.as_ref().map(|tool| tool.side_effect)
            }),
            None,
        )?;
        append_mcp_event(
            &state.store,
            run_id,
            "mcp.approval.requested",
            json!({
                "server_id": request.server_id.as_str(),
                "tool_name": request.tool_name.as_str(),
                "approved": request.approved,
                "args_keys": json_object_keys(&request.args)
            }),
            None,
        )?;
    }

    let approved = request.approved;
    let mut result = if approved {
        if let Some(run_id) = &request.run_id {
            append_mcp_event(
                &state.store,
                run_id,
                "mcp.tool.started",
                json!({
                    "server_id": request.server_id.as_str(),
                    "tool_name": request.tool_name.as_str()
                }),
                None,
            )?;
        }
        invoke_mock_mcp_tool(&request)
    } else {
        invoke_mock_mcp_tool(&request)
    };

    if result.status == "failed" {
        attach_mcp_evidence(&state.store, &mut result)?;
    }
    externalize_large_mcp_output(&state.store, &mut result)?;

    if let Some(run_id) = &request.run_id {
        let event_kind = match result.status.as_str() {
            "completed" => "mcp.tool.completed",
            "failed" => "mcp.tool.failed",
            "blocked" => "mcp.tool.blocked",
            _ => "mcp.tool.failed",
        };
        append_mcp_event(
            &state.store,
            run_id,
            event_kind,
            json!({
                "server_id": request.server_id.as_str(),
                "tool_name": request.tool_name.as_str(),
                "status": result.status.as_str(),
                "requires_approval": result.requires_approval,
                "approval_key": result.approval_key.as_str(),
                "evidence_ref": result.evidence_ref.as_deref(),
                "output": &result.output
            }),
            result.evidence_ref.as_deref(),
        )?;
    }

    Ok(Json(result))
}

fn append_mcp_event(
    store: &RunStore,
    run_id: &RunId,
    kind: &str,
    payload: Value,
    evidence_ref: Option<&str>,
) -> Result<(), StoreError> {
    let sequence = store.event_count(run_id)? as u64 + 1;
    let mut event = coder_events::CoderEvent::new(run_id.clone(), sequence, kind, payload);
    if let Some(reference) = evidence_ref {
        event = event.with_ref("mcp_evidence", reference);
    }
    store.append_event(run_id, &event)
}

fn attach_mcp_evidence(store: &RunStore, result: &mut McpToolCallResult) -> Result<(), StoreError> {
    if result.evidence_ref.is_some() {
        return Ok(());
    }
    let output = serde_json::to_string(&result.output).unwrap_or_else(|_| "{}".to_owned());
    let evidence_ref = store.write_blob(output.as_bytes())?;
    result.evidence_ref = Some(evidence_ref);
    Ok(())
}

fn externalize_large_mcp_output(
    store: &RunStore,
    result: &mut McpToolCallResult,
) -> Result<(), StoreError> {
    let output = serde_json::to_string(&result.output).unwrap_or_else(|_| "{}".to_owned());
    if output.len() <= MCP_OUTPUT_INLINE_LIMIT {
        return Ok(());
    }
    let large_ref = store.write_large_text_ref_with_limit(&output, MCP_OUTPUT_INLINE_LIMIT)?;
    result.evidence_ref = Some(large_ref.blob_ref.clone());
    result.output = json!({
        "preview": large_ref.preview,
        "truncated": large_ref.truncated,
        "blob_ref": large_ref.blob_ref
    });
    Ok(())
}

fn json_object_keys(value: &Value) -> Vec<String> {
    match value {
        Value::Object(object) => object.keys().cloned().collect(),
        _ => Vec::new(),
    }
}
