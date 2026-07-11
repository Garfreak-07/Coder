use axum::{extract::State, Json};
use coder_workflow::{
    execute_model_tool_turn, ModelToolHostContext, ModelToolLoopOptions, MODEL_TOOL_RESULT_CONTRACT,
};

use crate::model_tool_execute_pipeline::execute_model_tool_response;
use crate::model_tool_input::model_tool_use_blocks;
use crate::model_tool_response::model_tool_execute_response_from_result_block;
use crate::model_tool_server_executor::server_model_tool_executor;
use crate::{
    ApiError, ApiState, ModelToolExecuteRequest, ModelToolExecuteResponse, ModelToolTurnRequest,
    ModelToolTurnResponse,
};

const MODEL_TOOL_EXECUTE_BRIDGE: &str = "/api/v3/tools/model/execute";

pub(crate) async fn execute_model_tool_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<ModelToolExecuteRequest>,
) -> Result<Json<ModelToolExecuteResponse>, ApiError> {
    let response = execute_model_tool_response(state, request).await;
    Ok(Json(response))
}

pub(crate) async fn execute_model_tool_turn_endpoint(
    State(state): State<ApiState>,
    Json(request): Json<ModelToolTurnRequest>,
) -> Result<Json<ModelToolTurnResponse>, ApiError> {
    let ModelToolTurnRequest {
        tool_uses,
        max_tool_use_concurrency,
        run_id,
        harness_id,
        agent_id,
        current_model,
        current_effort,
        skill_context_modifiers,
    } = request;
    let tool_uses = model_tool_use_blocks(tool_uses)?;
    let options = max_tool_use_concurrency
        .map(ModelToolLoopOptions::with_max_tool_use_concurrency)
        .unwrap_or_default()
        .with_host_context(ModelToolHostContext {
            run_id,
            harness_id,
            agent_id,
            current_model,
            current_effort,
            skill_context_modifiers,
            ..ModelToolHostContext::default()
        });
    let executor = server_model_tool_executor(state);
    let output = execute_model_tool_turn(tool_uses, executor, options).await;
    Ok(Json(ModelToolTurnResponse {
        contract: output.contract,
        source: "coder-server",
        result_contract: MODEL_TOOL_RESULT_CONTRACT,
        model_tool_result_bridge: MODEL_TOOL_EXECUTE_BRIDGE,
        results: output
            .results
            .into_iter()
            .map(model_tool_execute_response_from_result_block)
            .collect(),
        attachments: output.attachments,
        claude_sources: output.claude_sources,
    }))
}
