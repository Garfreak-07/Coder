use axum::{
    extract::{Path, State},
    Json,
};
use coder_core::RunId;
use coder_harness::{
    mcp_approval_key, validate_mcp_manifest, McpManifestValidation, McpToolCallRequest,
    McpToolCallResult,
};
use coder_store::{RunStore, StoreError};
use serde_json::{json, Value};

use crate::{
    api_types::{
        McpManifestValidationRequest, McpServerListResponse, McpServerRegistrationRequest,
        McpServerRegistrationResponse, McpServerRemoveResponse, McpToolListResponse,
    },
    stored_run_exists, ApiError, ApiState,
};

const MCP_OUTPUT_INLINE_LIMIT: usize = 1024;

pub(crate) async fn validate_mcp(
    Json(request): Json<McpManifestValidationRequest>,
) -> Json<McpManifestValidation> {
    Json(validate_mcp_manifest(&request.manifest))
}

pub(crate) async fn register_mcp_server(
    State(state): State<ApiState>,
    Json(request): Json<McpServerRegistrationRequest>,
) -> Result<Json<McpServerRegistrationResponse>, ApiError> {
    let validation = validate_mcp_manifest(&request.manifest);
    if !validation.ok {
        return Err(ApiError::bad_request(validation.errors.join("; ")));
    }
    let manifest = validation
        .manifest
        .ok_or_else(|| ApiError::bad_request("MCP manifest was not parsed"))?;
    let server_id = manifest.server_id.clone();
    let server = state
        .mcp_runtime
        .register(manifest)
        .await
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let tools = state
        .mcp_runtime
        .list_tools()
        .await
        .into_iter()
        .filter(|tool| tool.server_id == server_id)
        .collect();
    Ok(Json(McpServerRegistrationResponse { server, tools }))
}

pub(crate) async fn remove_mcp_server(
    State(state): State<ApiState>,
    Path(server_id): Path<String>,
) -> Json<McpServerRemoveResponse> {
    let removed = state.mcp_runtime.remove(&server_id).await;
    Json(McpServerRemoveResponse { server_id, removed })
}

pub(crate) async fn list_mcp_servers(State(state): State<ApiState>) -> Json<McpServerListResponse> {
    Json(McpServerListResponse {
        servers: state.mcp_runtime.list_servers().await,
    })
}

pub(crate) async fn list_mcp_tools(State(state): State<ApiState>) -> Json<McpToolListResponse> {
    Json(McpToolListResponse {
        tools: state.mcp_runtime.list_tools().await,
    })
}

pub(crate) async fn invoke_mcp_tool(
    State(state): State<ApiState>,
    Json(request): Json<McpToolCallRequest>,
) -> Result<Json<McpToolCallResult>, ApiError> {
    Ok(Json(invoke_mcp_tool_request(&state, request).await?))
}

pub(crate) async fn invoke_mcp_tool_request(
    state: &ApiState,
    request: McpToolCallRequest,
) -> Result<McpToolCallResult, ApiError> {
    let registered_server = state
        .mcp_runtime
        .list_servers()
        .await
        .into_iter()
        .find(|server| server.server_id == request.server_id);
    let discovered = state
        .mcp_runtime
        .find_tool(&request.server_id, &request.tool_name)
        .await;
    if let Some(run_id) = &request.run_id {
        if !stored_run_exists(&state.store, run_id)? {
            return Err(ApiError::not_found(format!(
                "run '{}' was not found",
                run_id.as_str()
            )));
        }
        if let Some(server) = &registered_server {
            append_mcp_event(
                &state.store,
                run_id,
                "mcp.server.registered",
                json!({
                    "server_id": server.server_id.as_str(),
                    "name": server.name.as_str(),
                    "enabled": server.enabled,
                    "requires_approval": server.requires_approval
                }),
                None,
            )?;
        }
        append_mcp_event(
            &state.store,
            run_id,
            "mcp.tool.discovered",
            json!({
                "server_id": request.server_id.as_str(),
                "tool_name": request.tool_name.as_str(),
                "discovered": discovered.is_some(),
                "enabled": discovered.is_some(),
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

    let approval_key = mcp_approval_key(&request.server_id, &request.tool_name);
    let mut result = if !request.approved {
        blocked_mcp_result(approval_key, "MCP tool calls require explicit approval.")
    } else if discovered.is_none() {
        failed_mcp_result(
            approval_key,
            json!({
                "error": "MCP tool was not discovered",
                "server_id": request.server_id,
                "tool_name": request.tool_name
            }),
        )
    } else {
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
        match state
            .mcp_runtime
            .call_tool(&request.server_id, &request.tool_name, request.args.clone())
            .await
        {
            Ok(output) if output.is_error => failed_mcp_result(approval_key, output.output),
            Ok(output) => completed_mcp_result(approval_key, output.output),
            Err(error) => failed_mcp_result(
                approval_key,
                json!({
                    "error": error.to_string(),
                    "server_id": request.server_id,
                    "tool_name": request.tool_name
                }),
            ),
        }
    };

    if result.status == "failed" {
        let persisted_output = coder_events::redact_payload(result.output.clone());
        attach_mcp_evidence(&state.store, &mut result, &persisted_output)?;
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
                "output": coder_events::redact_payload(result.output.clone())
            }),
            result.evidence_ref.as_deref(),
        )?;
    }

    Ok(result)
}

fn completed_mcp_result(approval_key: String, output: Value) -> McpToolCallResult {
    McpToolCallResult {
        status: "completed".to_owned(),
        requires_approval: false,
        approval_key,
        output,
        evidence_ref: None,
    }
}

fn blocked_mcp_result(approval_key: String, reason: &str) -> McpToolCallResult {
    McpToolCallResult {
        status: "blocked".to_owned(),
        requires_approval: true,
        approval_key,
        output: json!({"reason": reason}),
        evidence_ref: None,
    }
}

fn failed_mcp_result(approval_key: String, output: Value) -> McpToolCallResult {
    McpToolCallResult {
        status: "failed".to_owned(),
        requires_approval: false,
        approval_key,
        output,
        evidence_ref: None,
    }
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

fn attach_mcp_evidence(
    store: &RunStore,
    result: &mut McpToolCallResult,
    persisted_output: &Value,
) -> Result<(), StoreError> {
    if result.evidence_ref.is_some() {
        return Ok(());
    }
    let output = serde_json::to_string(persisted_output).unwrap_or_else(|_| "{}".to_owned());
    let evidence_ref = store.write_blob(output.as_bytes())?;
    result.evidence_ref = Some(evidence_ref);
    Ok(())
}

fn externalize_large_mcp_output(
    store: &RunStore,
    result: &mut McpToolCallResult,
) -> Result<(), StoreError> {
    let persisted_output = coder_events::redact_payload(result.output.clone());
    let output = serde_json::to_string(&persisted_output).unwrap_or_else(|_| "{}".to_owned());
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
