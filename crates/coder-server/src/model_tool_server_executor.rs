use async_trait::async_trait;
use coder_core::RunId;
use coder_workflow::{
    ModelToolExecutionError, ModelToolExecutionRequest, ModelToolExecutionResult,
    ModelToolExecutor, ModelToolHostContext, ModelToolResultBlock,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};

use crate::model_tool_async_attachments::{
    drain_async_hook_response_attachments, drain_async_rewake_notification_attachments,
    drain_idle_queue_async_rewake_notification_attachments,
    drain_planner_user_guidance_attachments,
};
use crate::model_tool_execute_pipeline::execute_model_tool_response;
use crate::model_tool_input::run_id_from_model_tool_input;
use crate::model_tool_response::model_tool_execution_result_from_response;
use crate::model_tool_result_storage::{
    clear_content_replacement_state_for_run, enforce_aggregate_model_tool_result_budget,
    model_tool_turn_attachment_run_ids, ModelToolContentReplacementState,
};
use crate::model_tool_skill_context::skill_context_modifier_attachments;
use crate::run_transcript_compaction::maybe_auto_compact_run_transcript_attachment;
use crate::{ApiState, ModelToolExecuteRequest};

pub(crate) fn server_model_tool_executor(state: ApiState) -> Arc<dyn ModelToolExecutor> {
    Arc::new(ServerModelToolExecutor {
        state,
        content_replacement_state: Arc::new(
            Mutex::new(ModelToolContentReplacementState::default()),
        ),
    })
}

struct ServerModelToolExecutor {
    state: ApiState,
    content_replacement_state: Arc<Mutex<ModelToolContentReplacementState>>,
}

#[async_trait]
impl ModelToolExecutor for ServerModelToolExecutor {
    async fn execute_model_tool(
        &self,
        request: ModelToolExecutionRequest,
    ) -> Result<ModelToolExecutionResult, ModelToolExecutionError> {
        let run_id = request
            .host_context
            .run_id
            .as_deref()
            .map(|run_id| RunId::from_string(run_id.to_owned()))
            .or_else(|| run_id_from_model_tool_input(&request.input));
        if let Some(run_id) = run_id {
            if let Ok(mut state) = self.content_replacement_state.lock() {
                state.record_tool_run_id(request.tool_use_id.clone(), run_id);
            }
        }
        let host_context = request.host_context;
        let response = execute_model_tool_response(
            self.state.clone(),
            ModelToolExecuteRequest {
                tool_use_id: request.tool_use_id.clone(),
                tool_name: request.tool_name.clone(),
                run_id: host_context.run_id,
                harness_id: host_context.harness_id,
                agent_id: host_context.agent_id,
                current_model: host_context.current_model,
                current_effort: host_context.current_effort,
                skill_context_modifiers: host_context.skill_context_modifiers,
                input: request.input,
            },
        )
        .await;
        Ok(model_tool_execution_result_from_response(response))
    }

    async fn finalize_model_tool_turn_results(
        &self,
        results: Vec<ModelToolResultBlock>,
    ) -> Vec<ModelToolResultBlock> {
        enforce_aggregate_model_tool_result_budget(
            &self.state.store,
            &self.content_replacement_state,
            results,
        )
    }

    async fn drain_model_tool_turn_attachments(
        &self,
        host_context: &ModelToolHostContext,
        results: &[ModelToolResultBlock],
    ) -> Vec<Value> {
        let run_ids = model_tool_turn_attachment_run_ids(
            host_context.run_id.as_deref(),
            results,
            &self.content_replacement_state,
        );
        let mut all_attachments = skill_context_modifier_attachments(host_context, results);
        for run_id in run_ids {
            all_attachments.extend(drain_planner_user_guidance_attachments(
                &self.state,
                &run_id,
            ));
            all_attachments.extend(drain_async_hook_response_attachments(
                &self.state.store,
                &run_id,
            ));
            all_attachments.extend(drain_async_rewake_notification_attachments(
                &self.state.store,
                &run_id,
                host_context.drain_later_notifications,
                host_context.agent_id.as_deref(),
            ));
            if let Some(mut compaction_attachment) = maybe_auto_compact_run_transcript_attachment(
                self.state.clone(),
                &run_id,
                results,
                host_context.agent_id.as_deref(),
            )
            .await
            {
                let cleanup = clear_content_replacement_state_for_run(
                    &self.content_replacement_state,
                    &run_id,
                );
                if let Some(object) = compaction_attachment.as_object_mut() {
                    object.insert(
                        "post_compact_content_replacement_cleanup".to_owned(),
                        cleanup,
                    );
                }
                all_attachments.push(compaction_attachment);
            }
        }
        all_attachments
    }

    async fn drain_idle_model_tool_attachments(&self, run_id: &RunId) -> Vec<Value> {
        drain_idle_queue_async_rewake_notification_attachments(&self.state.store, run_id)
    }
}
