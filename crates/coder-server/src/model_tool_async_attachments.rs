use std::collections::BTreeSet;

use coder_core::RunId;
use coder_events::CoderEvent;
use coder_store::RunStore;
use serde_json::{json, Value};

use crate::model_tool_command_hooks::{
    ASYNC_HOOK_RESPONSE_EVENT_KIND, ASYNC_REWAKE_NOTIFICATION_EVENT_KIND,
};
use crate::model_tool_hook_runtime::with_model_tool_event_lock;

const ASYNC_REWAKE_DELIVERED_EVENT_KIND: &str = "model_tool.async_rewake.delivered";
const ASYNC_HOOK_RESPONSE_DELIVERED_EVENT_KIND: &str = "model_tool.async_hook.response.delivered";
const MODEL_TOOL_TURN_ATTACHMENT_CONTRACT: &str = "coder.model_tool_turn_attachment.v1";
const ASYNC_REWAKE_DELIVERY_CONTRACT: &str = "coder.model_tool_async_rewake_delivery.v1";
const ASYNC_REWAKE_MODEL_TOOL_TURN_DELIVERY_CHANNEL: &str = "model_tool_turn_attachment";
const ASYNC_REWAKE_IDLE_QUEUE_DELIVERY_CHANNEL: &str = "idle_queue_processor";
const ASYNC_HOOK_RESPONSE_DELIVERY_CONTRACT: &str =
    "coder.model_tool_async_hook_response_delivery.v1";

pub(crate) fn drain_idle_queue_async_rewake_notification_attachments(
    store: &RunStore,
    run_id: &RunId,
) -> Vec<Value> {
    drain_async_rewake_notification_attachments_for_delivery(
        store,
        run_id,
        true,
        None,
        ASYNC_REWAKE_IDLE_QUEUE_DELIVERY_CHANNEL,
    )
}

pub(crate) fn drain_async_hook_response_attachments(
    store: &RunStore,
    run_id: &RunId,
) -> Vec<Value> {
    with_model_tool_event_lock(|| {
        let Ok(events) = store.read_events(run_id) else {
            return Vec::new();
        };
        let delivered_sequences = events
            .iter()
            .filter(|event| event.kind == ASYNC_HOOK_RESPONSE_DELIVERED_EVENT_KIND)
            .filter_map(|event| event.payload["response_sequence"].as_u64())
            .collect::<BTreeSet<_>>();
        let mut attachments = Vec::new();
        for event in events
            .iter()
            .filter(|event| event.kind == ASYNC_HOOK_RESPONSE_EVENT_KIND)
            .filter(|event| !delivered_sequences.contains(&event.sequence))
        {
            let attachment = async_hook_response_attachment(event);
            let Ok(sequence) = store.event_count(run_id).map(|count| count as u64 + 1) else {
                continue;
            };
            let delivery_event = CoderEvent::new(
                run_id.clone(),
                sequence,
                ASYNC_HOOK_RESPONSE_DELIVERED_EVENT_KIND,
                json!({
                    "contract": ASYNC_HOOK_RESPONSE_DELIVERY_CONTRACT,
                    "source": "coder-server",
                    "response_sequence": event.sequence,
                    "async_hook_id": event.payload["async_hook_id"].clone(),
                    "processId": event.payload["processId"].clone(),
                    "hookName": event.payload["hookName"].clone(),
                    "hookEvent": event.payload["hookEvent"].clone(),
                    "toolName": event.payload["toolName"].clone(),
                    "delivery_status": "delivered",
                    "delivery_channel": "model_tool_turn_attachment",
                    "attachment_contract": MODEL_TOOL_TURN_ATTACHMENT_CONTRACT
                }),
            );
            if store.append_event(run_id, &delivery_event).is_ok() {
                attachments.push(attachment);
            }
        }
        attachments
    })
    .unwrap_or_default()
}

fn async_hook_response_attachment(event: &CoderEvent) -> Value {
    let response = event
        .payload
        .get("response")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut attachment = json!({
        "contract": MODEL_TOOL_TURN_ATTACHMENT_CONTRACT,
        "source": "coder-server",
        "type": "async_hook_response",
        "processId": event.payload["processId"].clone(),
        "process_id": event.payload["process_id"].clone(),
        "async_hook_id": event.payload["async_hook_id"].clone(),
        "hookName": event.payload["hookName"].clone(),
        "hookEvent": event.payload["hookEvent"].clone(),
        "toolName": event.payload["toolName"].clone(),
        "response": response,
        "stdout": event.payload["stdout"].clone(),
        "stderr": event.payload["stderr"].clone(),
        "exitCode": event.payload["exitCode"].clone(),
        "response_sequence": event.sequence,
        "payload": {
            "hook_output_kind": event.payload["hook_output_kind"].clone(),
            "output_channel": event.payload["output_channel"].clone(),
            "tool_use_id": event.payload["tool_use_id"].clone()
        }
    });
    let response = attachment
        .get("response")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let model_content = async_hook_response_model_content(&response);
    if !model_content.is_empty() {
        if let Some(object) = attachment.as_object_mut() {
            object.insert("model_content".to_owned(), Value::Array(model_content));
        }
    }
    attachment
}

fn async_hook_response_model_content(response: &Value) -> Vec<Value> {
    let mut blocks = Vec::new();
    if let Some(system_message) = response.get("systemMessage") {
        push_wrapped_async_hook_content(&mut blocks, system_message);
    }
    if let Some(additional_context) = response
        .get("hookSpecificOutput")
        .and_then(|hook_specific| hook_specific.get("additionalContext"))
    {
        push_wrapped_async_hook_content(&mut blocks, additional_context);
    }
    blocks
}

fn push_wrapped_async_hook_content(blocks: &mut Vec<Value>, content: &Value) {
    match content {
        Value::String(text) if !text.trim().is_empty() => {
            blocks.push(json!({
                "type": "text",
                "text": wrap_in_system_reminder(text)
            }));
        }
        Value::Array(items) => {
            for item in items {
                if item.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        let mut item = item.clone();
                        if let Some(object) = item.as_object_mut() {
                            object.insert("text".to_owned(), json!(wrap_in_system_reminder(text)));
                        }
                        blocks.push(item);
                    }
                } else {
                    blocks.push(item.clone());
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn drain_async_rewake_notification_attachments(
    store: &RunStore,
    run_id: &RunId,
    drain_later_notifications: bool,
    current_agent_id: Option<&str>,
) -> Vec<Value> {
    drain_async_rewake_notification_attachments_for_delivery(
        store,
        run_id,
        drain_later_notifications,
        current_agent_id,
        ASYNC_REWAKE_MODEL_TOOL_TURN_DELIVERY_CHANNEL,
    )
}

fn drain_async_rewake_notification_attachments_for_delivery(
    store: &RunStore,
    run_id: &RunId,
    drain_later_notifications: bool,
    current_agent_id: Option<&str>,
    delivery_channel: &'static str,
) -> Vec<Value> {
    with_model_tool_event_lock(|| {
        let Ok(events) = store.read_events(run_id) else {
            return Vec::new();
        };
        let delivered_sequences = events
            .iter()
            .filter(|event| event.kind == ASYNC_REWAKE_DELIVERED_EVENT_KIND)
            .filter_map(|event| event.payload["notification_sequence"].as_u64())
            .collect::<BTreeSet<_>>();
        let mut attachments = Vec::new();
        for event in events
            .iter()
            .filter(|event| event.kind == ASYNC_REWAKE_NOTIFICATION_EVENT_KIND)
            .filter(|event| {
                queued_notification_priority_allows(&event.payload, drain_later_notifications)
            })
            .filter(|event| {
                queued_notification_agent_scope_allows(&event.payload, current_agent_id)
            })
            .filter(|event| !delivered_sequences.contains(&event.sequence))
        {
            let attachment = async_rewake_notification_attachment(event);
            let target_agent_id = queued_notification_agent_id(&event.payload);
            let Ok(sequence) = store.event_count(run_id).map(|count| count as u64 + 1) else {
                continue;
            };
            let delivery_event = CoderEvent::new(
                run_id.clone(),
                sequence,
                ASYNC_REWAKE_DELIVERED_EVENT_KIND,
                json!({
                    "contract": ASYNC_REWAKE_DELIVERY_CONTRACT,
                    "source": "coder-server",
                    "notification_sequence": event.sequence,
                    "async_hook_id": event.payload["async_hook_id"].clone(),
                    "tool_use_id": event.payload["tool_use_id"].clone(),
                    "tool_name": event.payload["tool_name"].clone(),
                    "agent_id": target_agent_id,
                    "agentId": target_agent_id,
                    "drain_agent_id": current_agent_id,
                    "drainAgentId": current_agent_id,
                    "mode": "task-notification",
                    "priority": "later",
                    "delivery_status": "delivered",
                    "delivery_channel": delivery_channel,
                    "drain_later_notifications": drain_later_notifications,
                    "attachment_contract": MODEL_TOOL_TURN_ATTACHMENT_CONTRACT
                }),
            );
            if store.append_event(run_id, &delivery_event).is_ok() {
                attachments.push(attachment);
            }
        }
        attachments
    })
    .unwrap_or_default()
}

fn queued_notification_priority_allows(payload: &Value, drain_later_notifications: bool) -> bool {
    match payload
        .get("priority")
        .and_then(Value::as_str)
        .unwrap_or("next")
    {
        "now" | "next" => true,
        "later" => drain_later_notifications,
        _ => drain_later_notifications,
    }
}

fn queued_notification_agent_scope_allows(payload: &Value, current_agent_id: Option<&str>) -> bool {
    let current_agent_id = current_agent_id
        .map(str::trim)
        .filter(|agent_id| !agent_id.is_empty());
    let target_agent_id = queued_notification_agent_id(payload);
    match current_agent_id {
        Some(agent_id) => {
            payload.get("mode").and_then(Value::as_str) == Some("task-notification")
                && target_agent_id == Some(agent_id)
        }
        None => target_agent_id.is_none(),
    }
}

fn queued_notification_agent_id(payload: &Value) -> Option<&str> {
    payload
        .get("agent_id")
        .or_else(|| payload.get("agentId"))
        .or_else(|| payload.get("target_agent_id"))
        .or_else(|| payload.get("targetAgentId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|agent_id| !agent_id.is_empty())
}

fn async_rewake_notification_attachment(event: &CoderEvent) -> Value {
    let message = event
        .payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Async hook requested attention.");
    let prompt =
        wrap_in_system_reminder(&format!("A background agent completed a task:\n{message}"));
    json!({
        "contract": MODEL_TOOL_TURN_ATTACHMENT_CONTRACT,
        "source": "coder-server",
        "type": "queued_command",
        "prompt": prompt,
        "source_uuid": event.payload["async_hook_id"].clone(),
        "commandMode": "task-notification",
        "agent_id": event.payload["agent_id"].clone(),
        "agentId": event.payload["agentId"].clone(),
        "origin": {
            "kind": "task-notification"
        },
        "isMeta": true,
        "notification_sequence": event.sequence,
        "model_content": {
            "type": "text",
            "text": prompt
        },
        "payload": {
            "async_hook_id": event.payload["async_hook_id"].clone(),
            "hook_event": event.payload["hook_event"].clone(),
            "tool_name": event.payload["tool_name"].clone(),
            "tool_use_id": event.payload["tool_use_id"].clone(),
            "agent_id": event.payload["agent_id"].clone(),
            "agentId": event.payload["agentId"].clone(),
            "mode": "task-notification",
            "priority": "later"
        }
    })
}

fn wrap_in_system_reminder(content: &str) -> String {
    format!("<system-reminder>\n{content}\n</system-reminder>")
}
