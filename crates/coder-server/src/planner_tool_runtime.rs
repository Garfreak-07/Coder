use coder_config::resolve_agent_tools;
use coder_tools::{builtin_tool, builtin_tools, canonical_builtin_tool_name, ToolPermission};
use coder_workflow::{
    execute_model_tool_turn, ModelToolLoopOptions, ModelToolResultBlock, ModelToolUseBlock,
    TurnContext,
};
use serde_json::{json, Value};

use crate::api_types::{PlannerConversationRequest, PlannerRuntimeContext};
use crate::model_tool_server_executor::server_model_tool_executor;
use crate::ApiState;

pub(crate) const PLANNER_MAX_TOOL_TURNS: usize = 2;
pub(crate) const PLANNER_MAX_TOOL_CALLS: usize = 4;
pub(crate) const PLANNER_TOOL_RESULT_MAX_BYTES: usize = 6_000;
pub(crate) const PLANNER_TOOL_RESULTS_TOTAL_MAX_BYTES: usize = 12_000;

pub(crate) fn planner_selected_tools(runtime: &PlannerRuntimeContext) -> Vec<String> {
    resolve_agent_tools(&runtime.agent, &runtime.harness)
        .selected_tools
        .into_iter()
        .filter_map(|name| {
            let canonical = canonical_builtin_tool_name(&name)?;
            builtin_tool(canonical)
                .filter(|tool| tool.permission == ToolPermission::ReadFiles)
                .map(|tool| tool.name.to_owned())
        })
        .collect()
}

pub(crate) fn planner_model_tools_schema(runtime: &PlannerRuntimeContext) -> Value {
    let selected = planner_selected_tools(runtime);
    Value::Array(
        builtin_tools()
            .iter()
            .copied()
            .filter(|tool| selected.iter().any(|name| name == tool.name))
            .filter_map(|tool| tool.model_spec().map(bound_planner_tool_schema))
            .collect(),
    )
}

pub(crate) async fn execute_planner_tool_turn(
    state: ApiState,
    request: &PlannerConversationRequest,
    mut tool_uses: Vec<ModelToolUseBlock>,
) -> Vec<ModelToolResultBlock> {
    let selected_tools = planner_selected_tools(&request.runtime);
    if selected_tools.is_empty() || request.repo_root.is_none() {
        return tool_uses
            .into_iter()
            .map(|tool_use| planner_tool_error_result(tool_use, "repository tools are unavailable"))
            .collect();
    }
    for tool_use in &mut tool_uses {
        bound_planner_tool_input(tool_use);
    }
    let turn_context = TurnContext {
        repo_root: request.repo_root.clone(),
        harness_id: Some(request.runtime.harness_id.clone()),
        agent_id: Some(request.runtime.agent_id.clone()),
        agent_role: Some(request.runtime.agent.role.clone()),
        current_model: Some(request.runtime.model.model.clone()),
        model_capabilities: Some(request.runtime.model.resolved_capabilities()),
        current_effort: request
            .runtime
            .agent
            .runtime
            .effort
            .as_ref()
            .map(|effort| json!(effort)),
        selected_tools,
        permission_policy: Some(request.runtime.harness.permissions.clone()),
        start_work_authorized: false,
        ..TurnContext::default()
    };
    let options = if request
        .runtime
        .model
        .resolved_capabilities()
        .supports_parallel_tool_calls
    {
        ModelToolLoopOptions::default()
    } else {
        ModelToolLoopOptions::with_max_tool_use_concurrency(1)
    };
    execute_model_tool_turn(
        tool_uses,
        server_model_tool_executor(state),
        options.with_turn_context(turn_context),
    )
    .await
    .results
}

fn bound_planner_tool_schema(mut schema: Value) -> Value {
    let Some(name) = schema.pointer("/function/name").and_then(Value::as_str) else {
        return schema;
    };
    let bounds = match name {
        "repo_find_files" => vec![("max_results", 100)],
        "repo_search_text" => vec![("max_matches", 20)],
        "repo_read_file_range" => vec![("max_lines", 80), ("max_chars", 6_000)],
        _ => Vec::new(),
    };
    for (field, maximum) in bounds {
        if let Some(property) = schema
            .pointer_mut(&format!("/function/parameters/properties/{field}"))
            .and_then(Value::as_object_mut)
        {
            property.insert("maximum".to_owned(), json!(maximum));
        }
    }
    schema
}

fn bound_planner_tool_input(tool_use: &mut ModelToolUseBlock) {
    if !tool_use.input.is_object() {
        tool_use.input = json!({});
    }
    let Some(input) = tool_use.input.as_object_mut() else {
        return;
    };
    let bounds = match canonical_builtin_tool_name(&tool_use.name) {
        Some("repo_find_files") => vec![("max_results", 100)],
        Some("repo_search_text") => vec![("max_matches", 20)],
        Some("repo_read_file_range") => vec![("max_lines", 80), ("max_chars", 6_000)],
        _ => Vec::new(),
    };
    for (field, maximum) in bounds {
        let value = input
            .get(field)
            .and_then(Value::as_u64)
            .unwrap_or(maximum)
            .min(maximum);
        input.insert(field.to_owned(), json!(value));
    }
}

pub(crate) fn planner_tool_error_result(
    tool_use: ModelToolUseBlock,
    message: &str,
) -> ModelToolResultBlock {
    ModelToolResultBlock {
        contract: coder_workflow::MODEL_TOOL_RESULT_CONTRACT,
        source: "coder-workflow",
        result_type: "tool_result",
        tool_use_id: tool_use.id,
        tool_name: tool_use.name,
        status: "failed".to_owned(),
        is_error: true,
        content: json!({"error": message}).to_string(),
        content_truncated: false,
        payload: json!({"status": "failed", "error": message}),
        refs: Vec::new(),
        phases: Vec::new(),
    }
}

pub(crate) fn planner_tool_result_messages(
    results: Vec<ModelToolResultBlock>,
    remaining_total_bytes: &mut usize,
) -> Vec<Value> {
    results
        .into_iter()
        .map(|result| {
            let limit = PLANNER_TOOL_RESULT_MAX_BYTES.min(*remaining_total_bytes);
            const TRUNCATED_NOTICE: &str = "\n...[truncated by Planner observation budget]";
            let content_limit = limit.saturating_sub(TRUNCATED_NOTICE.len());
            let (mut content, truncated) = if result.content.len() <= limit {
                (result.content, false)
            } else {
                truncate_utf8_bytes(&result.content, content_limit)
            };
            let content = if truncated {
                content.push_str(&TRUNCATED_NOTICE[..TRUNCATED_NOTICE.len().min(limit)]);
                content
            } else {
                content
            };
            *remaining_total_bytes = remaining_total_bytes.saturating_sub(content.len());
            json!({
                "role": "tool",
                "tool_call_id": result.tool_use_id,
                "name": result.tool_name,
                "content": content
            })
        })
        .collect()
}

pub(crate) fn planner_tool_budget_exhausted(
    tool_turns: usize,
    tool_calls: usize,
    remaining_result_bytes: usize,
    tool_turn_limit: usize,
) -> bool {
    tool_turns >= tool_turn_limit
        || tool_calls >= PLANNER_MAX_TOOL_CALLS
        || remaining_result_bytes == 0
}

fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut boundary = max_bytes.min(value.len());
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (value[..boundary].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{default_project_config, planner_runtime::resolve_planner_runtime};

    #[test]
    fn planner_tool_snapshot_contains_only_configured_read_tools() {
        let config = default_project_config();
        let runtime = resolve_planner_runtime(&config, "planner-led", None).unwrap();
        let selected = planner_selected_tools(&runtime);

        assert_eq!(
            selected,
            vec![
                "repo_find_files",
                "repo_search_text",
                "repo_read_file_range",
                "git_status"
            ]
        );
        assert!(!selected.iter().any(|tool| tool == "command_run"));
        assert!(!selected.iter().any(|tool| tool == "write_text_file"));
    }

    #[test]
    fn planner_tool_result_budget_is_utf8_safe_and_aggregate_bounded() {
        let result = ModelToolResultBlock {
            contract: coder_workflow::MODEL_TOOL_RESULT_CONTRACT,
            source: "coder-workflow",
            result_type: "tool_result",
            tool_use_id: "call-1".to_owned(),
            tool_name: "repo_read_file_range".to_owned(),
            status: "completed".to_owned(),
            is_error: false,
            content: "\u{754c}".repeat(10_000),
            content_truncated: false,
            payload: Value::Null,
            refs: Vec::new(),
            phases: Vec::new(),
        };
        let mut remaining = PLANNER_TOOL_RESULT_MAX_BYTES;
        let messages = planner_tool_result_messages(vec![result], &mut remaining);
        let content = messages[0]["content"].as_str().unwrap();

        assert!(content.is_char_boundary(content.len()));
        assert!(content.contains("truncated by Planner observation budget"));
        assert!(content.len() <= PLANNER_TOOL_RESULT_MAX_BYTES);
        assert_eq!(remaining, PLANNER_TOOL_RESULT_MAX_BYTES - content.len());
    }

    #[test]
    fn planner_tool_budget_stops_on_each_independent_bound() {
        assert!(planner_tool_budget_exhausted(2, 1, 1, 2));
        assert!(planner_tool_budget_exhausted(
            1,
            PLANNER_MAX_TOOL_CALLS,
            1,
            4
        ));
        assert!(planner_tool_budget_exhausted(1, 1, 0, 4));
        assert!(!planner_tool_budget_exhausted(1, 3, 1, 2));
    }

    #[test]
    fn planner_read_inputs_and_schemas_share_small_context_bounds() {
        let config = default_project_config();
        let runtime = resolve_planner_runtime(&config, "planner-led", None).unwrap();
        let schema = planner_model_tools_schema(&runtime);
        let range = schema
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool.pointer("/function/name") == Some(&json!("repo_read_file_range")))
            .unwrap();
        assert_eq!(
            range.pointer("/function/parameters/properties/max_chars/maximum"),
            Some(&json!(6_000))
        );

        let mut tool_use = ModelToolUseBlock::new(
            "read-1",
            "repo_read_file_range",
            json!({"path": "README.md", "max_lines": 200, "max_chars": 100_000}),
        );
        bound_planner_tool_input(&mut tool_use);
        assert_eq!(tool_use.input["max_lines"], 80);
        assert_eq!(tool_use.input["max_chars"], 6_000);
    }
}
