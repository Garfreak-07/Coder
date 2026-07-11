use coder_config::HookEvent;
use serde_json::Value;

pub(crate) const MODEL_TOOL_HOOK_OUTPUT_LIMIT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Default)]
pub(crate) struct ModelToolHookEffects {
    pub(crate) permission_behavior: Option<&'static str>,
    pub(crate) permission_decision_reason: Option<String>,
    pub(crate) updated_input: Option<Value>,
    pub(crate) additional_context: Option<Value>,
    pub(crate) updated_tool_output: Option<Value>,
    pub(crate) prevent_continuation: bool,
    pub(crate) stop_reason: Option<String>,
}

pub(crate) struct ParsedModelToolHookOutput {
    pub(crate) kind: &'static str,
    pub(crate) json_output: Option<Value>,
    pub(crate) validation_error: Option<String>,
    pub(crate) blocking_error: Option<String>,
    pub(crate) effects: ModelToolHookEffects,
}

pub(crate) fn bounded_hook_output_preview(output: &str) -> (String, bool) {
    let mut bytes = 0usize;
    let mut preview = String::new();
    for character in output.chars() {
        let length = character.len_utf8();
        if bytes.saturating_add(length) > MODEL_TOOL_HOOK_OUTPUT_LIMIT_BYTES {
            return (preview, true);
        }
        preview.push(character);
        bytes += length;
    }
    (preview, false)
}

pub(crate) fn parse_model_tool_hook_output(
    output: &str,
    expected_event: HookEvent,
    command: &str,
) -> ParsedModelToolHookOutput {
    let trimmed = output.trim();
    if !trimmed.starts_with('{') {
        return ParsedModelToolHookOutput {
            kind: "plain_text",
            json_output: None,
            validation_error: None,
            blocking_error: None,
            effects: ModelToolHookEffects::default(),
        };
    }
    let json_output = match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => value,
        Err(error) => {
            return ParsedModelToolHookOutput {
                kind: "invalid_json",
                json_output: None,
                validation_error: Some(error.to_string()),
                blocking_error: None,
                effects: ModelToolHookEffects::default(),
            };
        }
    };
    let mut effects = ModelToolHookEffects::default();
    let mut blocking_error = None;

    if json_output
        .get("continue")
        .and_then(Value::as_bool)
        .is_some_and(|should_continue| !should_continue)
    {
        effects.prevent_continuation = true;
        effects.stop_reason = json_output
            .get("stopReason")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    if let Some(decision) = json_output.get("decision").and_then(Value::as_str) {
        match decision {
            "approve" => effects.permission_behavior = Some("allow"),
            "block" => {
                effects.permission_behavior = Some("deny");
                blocking_error = Some(format!(
                    "[{}]: {}",
                    command,
                    json_output
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("Blocked by hook")
                ));
            }
            _ => {
                return ParsedModelToolHookOutput {
                    kind: "invalid_hook_json",
                    json_output: Some(json_output.clone()),
                    validation_error: Some(format!(
                        "unknown hook decision '{decision}'; expected approve or block"
                    )),
                    blocking_error: None,
                    effects,
                };
            }
        }
    }
    if effects.permission_behavior.is_some() {
        effects.permission_decision_reason = json_output
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }

    if let Some(hook_specific) = json_output
        .get("hookSpecificOutput")
        .and_then(Value::as_object)
    {
        let Some(event_name) = hook_specific.get("hookEventName").and_then(Value::as_str) else {
            return ParsedModelToolHookOutput {
                kind: "invalid_hook_json",
                json_output: Some(json_output),
                validation_error: Some("hookSpecificOutput.hookEventName is required".to_owned()),
                blocking_error,
                effects,
            };
        };
        if event_name != hook_event_name(expected_event) {
            return ParsedModelToolHookOutput {
                kind: "invalid_hook_json",
                json_output: Some(json_output.clone()),
                validation_error: Some(format!(
                    "hookSpecificOutput.hookEventName expected '{}' but got '{event_name}'",
                    hook_event_name(expected_event)
                )),
                blocking_error,
                effects,
            };
        }

        match expected_event {
            HookEvent::PreToolUse => {
                if let Some(permission_decision) = hook_specific
                    .get("permissionDecision")
                    .and_then(Value::as_str)
                {
                    match permission_decision {
                        "allow" => effects.permission_behavior = Some("allow"),
                        "ask" => effects.permission_behavior = Some("ask"),
                        "passthrough" => {}
                        "deny" => {
                            effects.permission_behavior = Some("deny");
                            blocking_error = Some(format!(
                                "[{}]: {}",
                                command,
                                hook_specific
                                    .get("permissionDecisionReason")
                                    .or_else(|| json_output.get("reason"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("Blocked by hook")
                            ));
                        }
                        _ => {
                            return ParsedModelToolHookOutput {
                                kind: "invalid_hook_json",
                                json_output: Some(json_output.clone()),
                                validation_error: Some(format!(
                                    "unknown PreToolUse permissionDecision '{permission_decision}'"
                                )),
                                blocking_error,
                                effects,
                            };
                        }
                    }
                }
                effects.permission_decision_reason = hook_specific
                    .get("permissionDecisionReason")
                    .or_else(|| json_output.get("reason"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(updated_input) = hook_specific.get("updatedInput") {
                    if updated_input.is_object() {
                        effects.updated_input = Some(updated_input.clone());
                    } else {
                        return ParsedModelToolHookOutput {
                            kind: "invalid_hook_json",
                            json_output: Some(json_output),
                            validation_error: Some(
                                "hookSpecificOutput.updatedInput must be an object".to_owned(),
                            ),
                            blocking_error,
                            effects,
                        };
                    }
                }
                effects.additional_context = hook_specific.get("additionalContext").cloned();
            }
            HookEvent::PostToolUse => {
                effects.additional_context = hook_specific.get("additionalContext").cloned();
                effects.updated_tool_output = hook_specific.get("updatedMCPToolOutput").cloned();
            }
            HookEvent::PostToolUseFailure => {
                effects.additional_context = hook_specific.get("additionalContext").cloned();
            }
        }
    }

    ParsedModelToolHookOutput {
        kind: "hook_json",
        json_output: Some(json_output),
        validation_error: None,
        blocking_error,
        effects,
    }
}

pub(crate) fn merge_hook_permission_behavior(
    current: Option<&'static str>,
    next: Option<&'static str>,
) -> Option<&'static str> {
    match (current, next) {
        (Some("deny"), _) | (_, Some("deny")) => Some("deny"),
        (Some("ask"), _) | (_, Some("ask")) => Some("ask"),
        (Some("allow"), _) | (_, Some("allow")) => Some("allow"),
        _ => None,
    }
}

fn hook_event_name(event: HookEvent) -> &'static str {
    match event {
        HookEvent::PreToolUse => "PreToolUse",
        HookEvent::PostToolUse => "PostToolUse",
        HookEvent::PostToolUseFailure => "PostToolUseFailure",
    }
}
