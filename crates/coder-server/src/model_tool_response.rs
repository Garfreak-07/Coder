use coder_harness::HarnessRunEventRef;
use coder_workflow::{ModelToolExecutionResult, ModelToolResultBlock};
use serde_json::{json, Value};

use crate::{ApiError, ModelToolExecuteResponse};

pub(crate) fn model_tool_success_response(
    tool_use_id: String,
    tool_name: String,
    payload: Value,
) -> ModelToolExecuteResponse {
    let status = model_tool_status(&payload);
    let successful_cancellation = matches!(
        tool_name.as_str(),
        "cancel_command_background" | "cancel_subagent_background"
    ) && matches!(status.as_str(), "cancelled" | "canceled");
    let successful_terminal = tool_name == "finish" && status == "blocked";
    let is_error = !successful_cancellation
        && !successful_terminal
        && matches!(
            status.as_str(),
            "blocked" | "failed" | "cancelled" | "canceled"
        );
    let (content, content_truncated) = model_tool_content(&payload, is_error);
    let refs = model_tool_refs(&payload);
    ModelToolExecuteResponse {
        contract: "coder.model_tool_result.v1",
        source: "coder-server",
        result_type: "tool_result",
        tool_use_id,
        tool_name,
        status,
        is_error,
        content,
        content_truncated,
        payload,
        refs,
        phases: Vec::new(),
    }
}

pub(crate) fn model_tool_error_response(
    tool_use_id: String,
    tool_name: String,
    error: ApiError,
) -> ModelToolExecuteResponse {
    let message = error.message;
    let payload = json!({
        "status": "failed",
        "error": message
    });
    let content = format!("<tool_use_error>Error: {message}</tool_use_error>");
    ModelToolExecuteResponse {
        contract: "coder.model_tool_result.v1",
        source: "coder-server",
        result_type: "tool_result",
        tool_use_id,
        tool_name,
        status: "failed".to_owned(),
        is_error: true,
        content,
        content_truncated: false,
        payload,
        refs: Vec::new(),
        phases: Vec::new(),
    }
}

pub(crate) fn model_tool_hook_blocked_response(
    tool_use_id: String,
    tool_name: String,
    blocking_error: String,
) -> ModelToolExecuteResponse {
    let payload = json!({
        "status": "blocked",
        "error": blocking_error,
        "blocked_by": "pre_tool_use_hook"
    });
    let content = format!(
        "<tool_use_error>Tool use blocked by PreToolUse hook: {blocking_error}</tool_use_error>"
    );
    ModelToolExecuteResponse {
        contract: "coder.model_tool_result.v1",
        source: "coder-server",
        result_type: "tool_result",
        tool_use_id,
        tool_name,
        status: "blocked".to_owned(),
        is_error: true,
        content,
        content_truncated: false,
        payload,
        refs: Vec::new(),
        phases: Vec::new(),
    }
}

pub(crate) fn model_tool_permission_blocked_response(
    tool_use_id: String,
    tool_name: String,
    permission_phase_payload: Value,
) -> ModelToolExecuteResponse {
    let policy_decision_status = permission_phase_payload
        .get("policy_decision_status")
        .and_then(Value::as_str)
        .unwrap_or("unresolved_permission")
        .to_owned();
    let required_permission = permission_phase_payload
        .get("required_permission")
        .cloned()
        .unwrap_or(Value::Null);
    let message = match policy_decision_status.as_str() {
        "requires_confirmation" => "Tool use requires confirmation before execution".to_owned(),
        "denied_by_policy" => "Tool use denied by the active permission policy".to_owned(),
        "unresolved_permission" => {
            "Tool use permission could not be resolved before execution".to_owned()
        }
        "unknown_policy_behavior" => {
            "Tool use permission policy has an unknown behavior".to_owned()
        }
        "denied_by_agent_tool_allowlist" => {
            "Agent tool use denied by the active allowed agent type list".to_owned()
        }
        "denied_by_agent_type_rule" => {
            "Agent tool use denied by a persisted Agent(type) permission rule".to_owned()
        }
        other => format!("Tool use blocked by permission decision: {other}"),
    };
    let payload = json!({
        "status": "blocked",
        "error": message.clone(),
        "blocked_by": "permission_decision",
        "policy_decision_status": policy_decision_status,
        "required_permission": required_permission,
        "approved_supplied": permission_phase_payload["approved_supplied"].clone(),
        "permission_result": permission_phase_payload["permission_result"].clone(),
        "permission_policy_source": permission_phase_payload["permission_policy_source"].clone(),
        "permission_policy": permission_phase_payload["permission_policy"].clone(),
        "permission_decision": permission_phase_payload
    });
    let content = format!("<tool_use_error>{message}</tool_use_error>");
    ModelToolExecuteResponse {
        contract: "coder.model_tool_result.v1",
        source: "coder-server",
        result_type: "tool_result",
        tool_use_id,
        tool_name,
        status: "blocked".to_owned(),
        is_error: true,
        content,
        content_truncated: false,
        payload,
        refs: Vec::new(),
        phases: Vec::new(),
    }
}

fn model_tool_status(payload: &Value) -> String {
    payload
        .get("status")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("result")
                .and_then(|result| result.get("status"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            payload
                .get("background_task")
                .and_then(|task| task.get("status"))
                .and_then(Value::as_str)
        })
        .unwrap_or("completed")
        .to_owned()
}

fn model_tool_content(payload: &Value, is_error: bool) -> (String, bool) {
    let model_payload = concise_subagent_payload(payload).unwrap_or_else(|| payload.clone());
    let content =
        serde_json::to_string_pretty(&model_payload).unwrap_or_else(|_| model_payload.to_string());
    if is_error {
        (format!("<tool_use_error>{content}</tool_use_error>"), false)
    } else {
        (content, false)
    }
}

fn concise_subagent_payload(payload: &Value) -> Option<Value> {
    if payload.get("agent_id").is_none()
        || payload.get("metadata_ref").is_none()
        || payload.get("transcript_ref").is_none()
    {
        return None;
    }
    let mut concise = serde_json::Map::new();
    for key in [
        "status",
        "task_id",
        "run_id",
        "agent_id",
        "metadata_ref",
        "transcript_ref",
        "report",
        "event_count",
        "events_truncated",
        "error",
        "background_task",
        "retrieval_status",
        "block",
        "timeout_ms",
    ] {
        if let Some(value) = payload.get(key) {
            concise.insert(key.to_owned(), value.clone());
        }
    }
    Some(Value::Object(concise))
}

pub(crate) fn apply_model_tool_post_hook_updated_output(
    response: &mut ModelToolExecuteResponse,
    updated_output: Value,
) {
    let original_payload = std::mem::take(&mut response.payload);
    let original_refs = response.refs.clone();
    let content = model_tool_updated_output_content(&updated_output);
    response.payload = json!({
        "status": response.status,
        "hook_updated_output": true,
        "hook_event": "PostToolUse",
        "output": updated_output,
        "original_payload": original_payload,
        "original_refs": original_refs
    });
    response.content = content;
    response.content_truncated = false;
}

fn model_tool_updated_output_content(output: &Value) -> String {
    output.as_str().map(str::to_owned).unwrap_or_else(|| {
        serde_json::to_string_pretty(output).unwrap_or_else(|_| output.to_string())
    })
}

fn model_tool_refs(payload: &Value) -> Vec<HarnessRunEventRef> {
    let mut refs = Vec::new();
    collect_evidence_ref(payload, &mut refs);
    collect_string_ref(payload, "metadata_ref", "subagent_metadata", &mut refs);
    collect_string_ref(payload, "transcript_ref", "subagent_transcript", &mut refs);
    if let Some(task) = payload.get("background_task") {
        collect_evidence_ref(task, &mut refs);
        collect_string_ref(task, "metadata_ref", "subagent_metadata", &mut refs);
        collect_string_ref(task, "transcript_ref", "subagent_transcript", &mut refs);
    }
    refs.sort_by(|left, right| {
        (left.label.as_str(), left.uri.as_str()).cmp(&(right.label.as_str(), right.uri.as_str()))
    });
    refs.dedup_by(|left, right| left.label == right.label && left.uri == right.uri);
    refs
}

fn collect_evidence_ref(payload: &Value, refs: &mut Vec<HarnessRunEventRef>) {
    let Some(ref_id) = payload
        .get("evidence_ref")
        .and_then(|value| value.get("ref_id"))
        .and_then(Value::as_str)
    else {
        return;
    };
    refs.push(HarnessRunEventRef {
        label: "repo_evidence".to_owned(),
        uri: format!("repo-evidence://{ref_id}"),
    });
}

fn collect_string_ref(payload: &Value, key: &str, label: &str, refs: &mut Vec<HarnessRunEventRef>) {
    let Some(uri) = payload.get(key).and_then(Value::as_str) else {
        return;
    };
    refs.push(HarnessRunEventRef {
        label: label.to_owned(),
        uri: uri.to_owned(),
    });
}

pub(crate) fn model_tool_execution_result_from_response(
    response: ModelToolExecuteResponse,
) -> ModelToolExecutionResult {
    ModelToolExecutionResult {
        tool_use_id: response.tool_use_id,
        tool_name: response.tool_name,
        status: response.status,
        is_error: response.is_error,
        content: response.content,
        content_truncated: response.content_truncated,
        payload: response.payload,
        refs: response.refs,
        cancels_siblings: false,
        phases: response.phases,
    }
}

pub(crate) fn model_tool_execute_response_from_result_block(
    result: ModelToolResultBlock,
) -> ModelToolExecuteResponse {
    ModelToolExecuteResponse {
        contract: result.contract,
        source: result.source,
        result_type: result.result_type,
        tool_use_id: result.tool_use_id,
        tool_name: result.tool_name,
        status: result.status,
        is_error: result.is_error,
        content: result.content,
        content_truncated: result.content_truncated,
        payload: result.payload,
        refs: result.refs,
        phases: result.phases,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_cancellation_status_is_a_successful_tool_result() {
        for tool_name in ["cancel_command_background", "cancel_subagent_background"] {
            let response = model_tool_success_response(
                "call-cancel".to_owned(),
                tool_name.to_owned(),
                json!({"status": "cancelled", "cancelled": true}),
            );

            assert_eq!(response.status, "cancelled");
            assert!(!response.is_error);
            assert!(!response.content.contains("<tool_use_error>"));
        }
    }

    #[test]
    fn subagent_model_content_uses_summary_and_durable_refs() {
        let payload = json!({
            "status": "completed",
            "run_id": "run-1",
            "agent_id": "agent-1",
            "metadata_ref": "subagent://meta",
            "transcript_ref": "subagent://transcript",
            "report": {"summary": "review complete"},
            "event_count": 1000,
            "events_truncated": false,
            "events": [{"kind": "large-event", "payload": "x".repeat(10_000)}]
        });

        let response =
            model_tool_success_response("call-1".to_owned(), "agent_subagent".to_owned(), payload);

        assert!(response.content.contains("review complete"));
        assert!(response.content.contains("subagent://transcript"));
        assert!(!response.content.contains("large-event"));
        assert_eq!(response.payload["events"][0]["kind"], "large-event");
    }
}
