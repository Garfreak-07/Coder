use coder_config::HookEvent;
use coder_workflow::TurnContext;
use serde_json::{json, Value};
use std::time::Instant;

use crate::model_tool_dispatch::{execute_model_tool_request_with_route, ModelMcpToolRoute};
use crate::model_tool_hook_phase::{execute_model_tool_hook_phase, ModelToolHookInvocation};
use crate::model_tool_input::{
    apply_model_tool_defaults, apply_model_tool_policy_approval_defaults,
    apply_model_tool_request_context, canonical_model_tool_name,
};
use crate::model_tool_permissions::{
    model_tool_permission_allows_execution, model_tool_permission_phase_payload,
    model_tool_permission_phase_status,
};
use crate::model_tool_phase::ModelToolPhaseRecorder;
use crate::model_tool_response::{
    apply_model_tool_post_hook_updated_output, model_tool_error_response,
    model_tool_hook_blocked_response, model_tool_permission_blocked_response,
    model_tool_success_response,
};
use crate::model_tool_result_storage::maybe_persist_large_model_tool_result;
use crate::run_transcript_compaction::post_compact_restore_candidate_payload;
use crate::{ApiError, ApiState, ModelToolExecuteRequest, ModelToolExecuteResponse};

pub(crate) async fn execute_model_tool_response(
    state: ApiState,
    mut request: ModelToolExecuteRequest,
) -> ModelToolExecuteResponse {
    let turn_context = apply_model_tool_request_context(&mut request);
    execute_model_tool_response_with_turn_context(state, request, turn_context).await
}

pub(crate) async fn execute_model_tool_response_with_turn_context(
    state: ApiState,
    request: ModelToolExecuteRequest,
    turn_context: TurnContext,
) -> ModelToolExecuteResponse {
    execute_model_tool_response_with_turn_context_and_route(state, request, turn_context, None)
        .await
}

pub(crate) async fn execute_model_tool_response_with_turn_context_and_route(
    state: ApiState,
    mut request: ModelToolExecuteRequest,
    turn_context: TurnContext,
    mcp_route: Option<ModelMcpToolRoute>,
) -> ModelToolExecuteResponse {
    let store = state.store.clone();
    let tool_use_id = request.tool_use_id.clone();
    let tool_name = request.tool_name.clone();
    let canonical_tool_name = canonical_model_tool_name(&request.tool_name);
    let mut recorder = ModelToolPhaseRecorder::new(
        &state.store,
        &tool_use_id,
        &tool_name,
        canonical_tool_name,
        &request.input,
    );
    let applied_defaults = apply_model_tool_defaults(&mut request);
    let mut tool_input_for_hooks = request.input.clone();

    recorder.record_phase_started("pre_tool_use_hooks");
    let pre_hook_started = Instant::now();
    let pre_hook_phase = execute_model_tool_hook_phase(
        &state,
        ModelToolHookInvocation {
            event: HookEvent::PreToolUse,
            canonical_tool_name,
            requested_tool_name: &tool_name,
            tool_use_id: &tool_use_id,
            tool_input: &tool_input_for_hooks,
            tool_response: None,
            tool_error: None,
            host_context: &turn_context,
        },
    )
    .await;
    recorder.record_phase(
        "pre_tool_use_hooks",
        pre_hook_phase.status,
        pre_hook_started.elapsed().as_millis() as u64,
        pre_hook_phase.payload.clone(),
    );
    if let Some(blocking_error) = pre_hook_phase.blocking_error.clone() {
        recorder.record_phase(
            "permission_decision",
            "skipped_pre_tool_use_hook_blocked",
            0,
            json!({
                "reason": "pre_tool_use_hook_blocked",
                "blocking_error": blocking_error
            }),
        );
        recorder.record_phase(
            "tool_execution",
            "blocked",
            0,
            json!({
                "is_error": true,
                "content_truncated": false,
                "blocked_by": "pre_tool_use_hook"
            }),
        );
        recorder.record_phase(
            "post_tool_use_hooks",
            "skipped_pre_tool_use_hook_blocked",
            0,
            json!({
                "reason": "pre_tool_use_hook_blocked"
            }),
        );
        let phases = recorder.into_phases();
        let mut response = model_tool_hook_blocked_response(tool_use_id, tool_name, blocking_error);
        if let Value::Object(payload) = &mut response.payload {
            payload.insert("model_tool_phases".to_owned(), Value::Array(phases.clone()));
        }
        response.phases = phases;
        return response;
    }
    if let Some(updated_input) = pre_hook_phase.updated_input.clone() {
        request.input = updated_input;
        tool_input_for_hooks = request.input.clone();
    }
    let permission_phase_payload = model_tool_permission_phase_payload(
        &state,
        canonical_tool_name,
        &tool_use_id,
        &request.input,
        &turn_context,
    );
    recorder.set_required_permission_override(
        permission_phase_payload
            .get("required_permission")
            .and_then(Value::as_str),
    );
    recorder.record_phase_started("permission_decision");
    let permission_started = Instant::now();
    let permission_decision_status = permission_phase_payload
        .get("policy_decision_status")
        .and_then(Value::as_str)
        .unwrap_or("unresolved_permission")
        .to_owned();
    let permission_allows_execution =
        model_tool_permission_allows_execution(&permission_decision_status);
    recorder.record_phase(
        "permission_decision",
        model_tool_permission_phase_status(&permission_decision_status),
        permission_started.elapsed().as_millis() as u64,
        permission_phase_payload.clone(),
    );
    if !permission_allows_execution {
        recorder.record_phase(
            "tool_execution",
            "blocked",
            0,
            json!({
                "is_error": true,
                "content_truncated": false,
                "blocked_by": "permission_decision",
                "policy_decision_status": permission_decision_status.clone(),
                "required_permission": permission_phase_payload["required_permission"].clone()
            }),
        );
        recorder.record_phase(
            "post_tool_use_hooks",
            "skipped_permission_decision_blocked",
            0,
            json!({
                "reason": "permission_decision_blocked",
                "policy_decision_status": permission_decision_status.clone()
            }),
        );
        let phases = recorder.into_phases();
        let mut response = model_tool_permission_blocked_response(
            tool_use_id,
            tool_name,
            permission_phase_payload,
        );
        if let Value::Object(payload) = &mut response.payload {
            payload.insert("model_tool_phases".to_owned(), Value::Array(phases.clone()));
        }
        response.phases = phases;
        return response;
    }
    let policy_approval_defaults = apply_model_tool_policy_approval_defaults(
        canonical_tool_name,
        &permission_decision_status,
        &mut request.input,
    );

    let started = Instant::now();
    let executed_tool_input = request.input.clone();
    recorder.record_phase_started("tool_execution");
    let response = match execute_model_tool_request_with_route(
        state.clone(),
        request,
        &turn_context,
        mcp_route.as_ref(),
    )
    .await
    {
        Ok(payload) => model_tool_success_response(tool_use_id.clone(), tool_name.clone(), payload),
        Err(error) => model_tool_error_response(tool_use_id.clone(), tool_name.clone(), error),
    };
    let mut response = response;
    if let Err(error) = maybe_persist_large_model_tool_result(&store, &mut response) {
        response = model_tool_error_response(
            response.tool_use_id,
            response.tool_name,
            ApiError::internal(error.to_string()),
        );
    }
    let mut tool_execution_payload = json!({
        "is_error": response.is_error,
        "content_truncated": response.content_truncated,
        "applied_defaults": applied_defaults,
        "policy_approval_defaults": policy_approval_defaults
    });
    if let Some(candidate) = post_compact_restore_candidate_payload(
        canonical_tool_name,
        &tool_use_id,
        &tool_name,
        &executed_tool_input,
        &response,
        turn_context.agent_id.as_deref(),
        turn_context.harness_id.as_deref(),
    ) {
        if let Some(object) = tool_execution_payload.as_object_mut() {
            object.insert("post_compact_restore_candidate".to_owned(), candidate);
        }
    }
    recorder.record_phase(
        "tool_execution",
        &response.status,
        started.elapsed().as_millis() as u64,
        tool_execution_payload,
    );
    recorder.record_phase_started("post_tool_use_hooks");
    let post_hook_started = Instant::now();
    let post_hook_phase = execute_model_tool_hook_phase(
        &state,
        ModelToolHookInvocation {
            event: if response.is_error {
                HookEvent::PostToolUseFailure
            } else {
                HookEvent::PostToolUse
            },
            canonical_tool_name,
            requested_tool_name: &response.tool_name,
            tool_use_id: &tool_use_id,
            tool_input: &tool_input_for_hooks,
            tool_response: if response.is_error {
                None
            } else {
                Some(&response.payload)
            },
            tool_error: if response.is_error {
                Some(response.content.as_str())
            } else {
                None
            },
            host_context: &turn_context,
        },
    )
    .await;
    recorder.record_phase(
        "post_tool_use_hooks",
        post_hook_phase.status,
        post_hook_started.elapsed().as_millis() as u64,
        post_hook_phase.payload.clone(),
    );
    if let Some(updated_output) = post_hook_phase.updated_tool_output.clone() {
        apply_model_tool_post_hook_updated_output(&mut response, updated_output);
        if let Err(error) = maybe_persist_large_model_tool_result(&store, &mut response) {
            response = model_tool_error_response(
                response.tool_use_id,
                response.tool_name,
                ApiError::internal(error.to_string()),
            );
        }
    }
    let phases = recorder.into_phases();
    if let Value::Object(payload) = &mut response.payload {
        payload.insert("model_tool_phases".to_owned(), Value::Array(phases.clone()));
    }
    response.phases = phases;
    response
}
