use async_trait::async_trait;
use coder_core::RunId;
use coder_tools::canonical_builtin_tool_name;
use coder_workflow::{
    ModelToolExecutionError, ModelToolExecutionRequest, ModelToolExecutionResult,
    ModelToolExecutor, ModelToolResultBlock, TurnContext,
};
use serde_json::Value;
use std::time::Duration;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use crate::model_tool_async_attachments::{
    drain_async_hook_response_attachments, drain_async_rewake_notification_attachments,
    drain_idle_queue_async_rewake_notification_attachments,
};
use crate::model_tool_dispatch::ModelMcpToolRoute;
use crate::model_tool_execute_pipeline::execute_model_tool_response_with_turn_context_and_route;
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
    server_model_tool_executor_with_mcp(state, BTreeMap::new())
}

pub(crate) fn server_model_tool_executor_with_mcp(
    state: ApiState,
    mcp_routes: BTreeMap<String, ModelMcpToolRoute>,
) -> Arc<dyn ModelToolExecutor> {
    Arc::new(ServerModelToolExecutor {
        state,
        mcp_routes,
        content_replacement_state: Arc::new(
            Mutex::new(ModelToolContentReplacementState::default()),
        ),
    })
}

struct ServerModelToolExecutor {
    state: ApiState,
    mcp_routes: BTreeMap<String, ModelMcpToolRoute>,
    content_replacement_state: Arc<Mutex<ModelToolContentReplacementState>>,
}

#[async_trait]
impl ModelToolExecutor for ServerModelToolExecutor {
    async fn execute_model_tool(
        &self,
        mut request: ModelToolExecutionRequest,
    ) -> Result<ModelToolExecutionResult, ModelToolExecutionError> {
        if !turn_context_allows_tool(&request.turn_context, &request.tool_name) {
            return Err(ModelToolExecutionError::new(format!(
                "tool '{}' is not selected for this turn",
                request.tool_name
            )));
        }
        let mcp_route = self.mcp_routes.get(&request.tool_name).cloned();
        if mcp_route.is_none() {
            apply_turn_context_to_tool_input(
                &mut request.input,
                &request.turn_context,
                &request.tool_name,
            );
        }
        let run_id = request
            .turn_context
            .run_id
            .as_deref()
            .map(|run_id| RunId::from_string(run_id.to_owned()))
            .or_else(|| run_id_from_model_tool_input(&request.input));
        if let Some(run_id) = run_id {
            if let Ok(mut state) = self.content_replacement_state.lock() {
                state.record_tool_run_id(request.tool_use_id.clone(), run_id);
            }
        }
        let turn_context = request.turn_context;
        let response = execute_model_tool_response_with_turn_context_and_route(
            self.state.clone(),
            ModelToolExecuteRequest {
                tool_use_id: request.tool_use_id.clone(),
                tool_name: request.tool_name.clone(),
                run_id: turn_context.run_id.clone(),
                harness_id: turn_context.harness_id.clone(),
                agent_id: turn_context.agent_id.clone(),
                current_model: turn_context.current_model.clone(),
                current_effort: turn_context.current_effort.clone(),
                skill_context_modifiers: turn_context.skill_context_modifiers.clone(),
                input: request.input,
            },
            turn_context,
            mcp_route,
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
        host_context: &TurnContext,
        results: &[ModelToolResultBlock],
    ) -> Vec<Value> {
        let run_ids = model_tool_turn_attachment_run_ids(
            host_context.run_id.as_deref(),
            results,
            &self.content_replacement_state,
        );
        let async_rewake_requested = model_tool_results_request_async_rewake(results);
        let drain_later_notifications =
            host_context.drain_later_notifications || async_rewake_requested;
        let mut all_attachments = skill_context_modifier_attachments(host_context, results);
        let mut async_rewake_delivered = false;
        for run_id in &run_ids {
            all_attachments.extend(drain_async_hook_response_attachments(
                &self.state.store,
                run_id,
            ));
            let attachments = drain_async_rewake_notification_attachments(
                &self.state.store,
                run_id,
                drain_later_notifications,
                host_context.agent_id.as_deref(),
            );
            async_rewake_delivered |= !attachments.is_empty();
            all_attachments.extend(attachments);
            if let Some(mut compaction_attachment) = maybe_auto_compact_run_transcript_attachment(
                self.state.clone(),
                run_id,
                results,
                host_context.agent_id.as_deref(),
            )
            .await
            {
                let cleanup = clear_content_replacement_state_for_run(
                    &self.content_replacement_state,
                    run_id,
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
        if async_rewake_requested && !async_rewake_delivered {
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(25)).await;
                let mut attachments = Vec::new();
                for run_id in &run_ids {
                    attachments.extend(drain_async_rewake_notification_attachments(
                        &self.state.store,
                        run_id,
                        true,
                        host_context.agent_id.as_deref(),
                    ));
                }
                if !attachments.is_empty() {
                    all_attachments.extend(attachments);
                    break;
                }
            }
        }
        all_attachments
    }

    async fn drain_idle_model_tool_attachments(&self, run_id: &RunId) -> Vec<Value> {
        drain_idle_queue_async_rewake_notification_attachments(&self.state.store, run_id)
    }
}

fn turn_context_allows_tool(turn_context: &TurnContext, tool_name: &str) -> bool {
    if turn_context.selected_tools.is_empty() {
        return true;
    }
    let canonical_tool_name = canonical_builtin_tool_name(tool_name).unwrap_or(tool_name);
    turn_context
        .selected_tools
        .iter()
        .any(|tool| tool == canonical_tool_name)
}

fn apply_turn_context_to_tool_input(
    input: &mut Value,
    turn_context: &TurnContext,
    tool_name: &str,
) {
    if !input.is_object() {
        *input = serde_json::json!({});
    }
    let Some(input) = input.as_object_mut() else {
        return;
    };
    for (key, value) in [
        ("run_id", turn_context.run_id.as_ref()),
        ("repo_root", turn_context.repo_root.as_ref()),
        ("harness_id", turn_context.harness_id.as_ref()),
        ("agent_id", turn_context.agent_id.as_ref()),
    ] {
        if let Some(value) = value {
            input.insert(key.to_owned(), Value::String(value.clone()));
        }
    }
    input.insert(
        "approved".to_owned(),
        Value::Bool(turn_context.host_approved),
    );
    if canonical_builtin_tool_name(tool_name) == Some("command_run") {
        input.insert("sandbox".to_owned(), Value::Bool(false));
    }
}

fn model_tool_results_request_async_rewake(results: &[ModelToolResultBlock]) -> bool {
    results.iter().any(|result| {
        result.phases.iter().any(|phase| {
            phase
                .get("hook_results")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|hook_result| {
                    hook_result
                        .get("async_rewake")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn turn_context_overrides_model_supplied_identity_scope_and_approval() {
        let mut input = json!({
            "run_id": "model-run",
            "repo_root": "C:/outside",
            "harness_id": "model-harness",
            "agent_id": "model-agent",
            "approved": false
        });
        apply_turn_context_to_tool_input(
            &mut input,
            &TurnContext {
                run_id: Some("host-run".to_owned()),
                repo_root: Some("F:/repo".to_owned()),
                harness_id: Some("host-harness".to_owned()),
                agent_id: Some("host-agent".to_owned()),
                host_approved: true,
                ..TurnContext::default()
            },
            "command_run",
        );

        assert_eq!(input["run_id"], "host-run");
        assert_eq!(input["repo_root"], "F:/repo");
        assert_eq!(input["harness_id"], "host-harness");
        assert_eq!(input["agent_id"], "host-agent");
        assert_eq!(input["approved"], true);
        assert_eq!(input["sandbox"], false);
    }

    #[test]
    fn turn_context_rejects_model_supplied_approval_without_host_authorization() {
        let mut input = json!({"approved": true, "sandbox": true});
        apply_turn_context_to_tool_input(&mut input, &TurnContext::default(), "command_run");
        assert_eq!(input["approved"], false);
        assert_eq!(input["sandbox"], false);
    }

    #[test]
    fn turn_context_tool_snapshot_is_enforced_with_alias_normalization() {
        let turn_context = TurnContext {
            selected_tools: vec!["repo_read_file".to_owned()],
            ..TurnContext::default()
        };
        assert!(turn_context_allows_tool(&turn_context, "read_file"));
        assert!(!turn_context_allows_tool(&turn_context, "write_text_file"));
    }

    #[tokio::test]
    async fn mcp_route_and_approval_are_owned_by_the_host_snapshot() {
        let root = std::env::temp_dir().join(format!("coder-mcp-route-{}", uuid::Uuid::new_v4()));
        let state = ApiState::new(coder_store::RunStore::new(&root));
        let provider_name = "mcp__trusted__lookup".to_owned();
        let executor = server_model_tool_executor_with_mcp(
            state,
            BTreeMap::from([(
                provider_name.clone(),
                ModelMcpToolRoute {
                    server_id: "trusted".to_owned(),
                    tool_name: "lookup".to_owned(),
                },
            )]),
        );
        let request = |authorized| ModelToolExecutionRequest {
            tool_use_id: format!("mcp-{authorized}"),
            tool_name: provider_name.clone(),
            input: json!({
                "server_id": "model-controlled",
                "tool_name": "delete_everything",
                "approved": true
            }),
            turn_context: TurnContext {
                selected_tools: vec![provider_name.clone()],
                host_approved: authorized,
                ..TurnContext::default()
            },
        };

        let blocked = executor.execute_model_tool(request(false)).await.unwrap();
        assert_eq!(blocked.status, "blocked");
        assert!(blocked.is_error);
        assert_eq!(blocked.payload["approval_key"], "mcp:trusted:lookup");

        let approved = executor.execute_model_tool(request(true)).await.unwrap();
        assert_eq!(approved.status, "failed");
        assert!(approved.is_error);
        assert_eq!(approved.payload["approval_key"], "mcp:trusted:lookup");
        assert_eq!(approved.payload["output"]["server_id"], "trusted");
        assert_eq!(approved.payload["output"]["tool_name"], "lookup");
        let _ = std::fs::remove_dir_all(root);
    }
}
