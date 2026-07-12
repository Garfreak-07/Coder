use coder_workflow::{ModelToolResultBlock, TurnContext};
use serde_json::{json, Value};

const MODEL_TOOL_TURN_ATTACHMENT_CONTRACT: &str = "coder.model_tool_turn_attachment.v1";
const SKILL_CONTEXT_MODIFIER_CONTRACT: &str = "coder.skill_context_modifier.v1";
const SKILL_CONTEXT_MODIFIER_PERMISSION_CONTRACT: &str =
    "coder.skill_context_modifier_permission.v1";

pub(crate) struct SkillContextModifierPermissionDecision {
    pub(crate) allowed: bool,
    pub(crate) payload: Option<Value>,
}

pub(crate) fn skill_context_modifier_attachments(
    host_context: &TurnContext,
    results: &[ModelToolResultBlock],
) -> Vec<Value> {
    results
        .iter()
        .filter_map(|result| skill_context_modifier_attachment(host_context, result))
        .collect()
}

pub(crate) fn model_tool_skill_context_modifier_permission_decision(
    required_permission: Option<&str>,
    canonical_tool_name: &str,
    input: &Value,
    host_context: &TurnContext,
    base_policy_decision_status: &str,
) -> SkillContextModifierPermissionDecision {
    if host_context.skill_context_modifiers.is_empty() {
        return SkillContextModifierPermissionDecision {
            allowed: false,
            payload: None,
        };
    }

    let active_modifiers = host_context
        .skill_context_modifiers
        .iter()
        .filter(|modifier| is_active_skill_context_modifier(modifier))
        .collect::<Vec<_>>();
    let mut payload = json!({
        "contract": SKILL_CONTEXT_MODIFIER_PERMISSION_CONTRACT,
        "source": "coder-server",
        "policy": "allowed_tools_read_and_scoped_commands",
        "allowed_by_modifier": false,
        "status": "not_matched",
        "required_permission": required_permission,
        "canonical_tool_name": canonical_tool_name,
        "base_policy_decision_status": base_policy_decision_status,
        "submitted_modifier_count": host_context.skill_context_modifiers.len(),
        "active_modifier_count": active_modifiers.len()
    });

    if active_modifiers.is_empty() {
        set_json_field(&mut payload, "status", json!("no_active_modifier"));
        return SkillContextModifierPermissionDecision {
            allowed: false,
            payload: Some(payload),
        };
    }
    if !matches!(required_permission, Some("read_files" | "run_commands")) {
        set_json_field(
            &mut payload,
            "status",
            json!("not_applicable_required_permission"),
        );
        return SkillContextModifierPermissionDecision {
            allowed: false,
            payload: Some(payload),
        };
    }
    if base_policy_decision_status != "requires_confirmation" {
        set_json_field(
            &mut payload,
            "status",
            json!(match base_policy_decision_status {
                "allowed_by_policy" => "not_needed_policy_allowed",
                "confirmation_supplied" => "not_needed_confirmation_supplied",
                "denied_by_policy" => "not_applied_policy_denied",
                _ => "not_applied_base_policy_status",
            }),
        );
        return SkillContextModifierPermissionDecision {
            allowed: false,
            payload: Some(payload),
        };
    }

    if let Some((modifier, allowed_tool_match)) = find_matching_skill_context_allowed_tool(
        required_permission,
        canonical_tool_name,
        input,
        &active_modifiers,
    ) {
        set_json_field(&mut payload, "status", json!("allowed"));
        set_json_field(&mut payload, "allowed_by_modifier", json!(true));
        set_json_field(&mut payload, "matched_tool", json!(canonical_tool_name));
        set_json_field(
            &mut payload,
            "matched_allowed_tool",
            json!(allowed_tool_match.allowed_tool),
        );
        set_json_field(
            &mut payload,
            "matched_allowed_tool_kind",
            json!(allowed_tool_match.kind),
        );
        if let Some(command) = allowed_tool_match.command {
            set_json_field(&mut payload, "matched_command", json!(command));
        }
        if let Some(rule_content) = allowed_tool_match.rule_content {
            set_json_field(&mut payload, "matched_rule_content", json!(rule_content));
        }
        set_json_field(
            &mut payload,
            "modifier_contract",
            modifier
                .get("modifier_contract")
                .cloned()
                .unwrap_or(Value::Null),
        );
        set_json_field(
            &mut payload,
            "skill_name",
            modifier.get("skill_name").cloned().unwrap_or(Value::Null),
        );
        set_json_field(
            &mut payload,
            "source_tool_use_id",
            modifier.get("tool_use_id").cloned().unwrap_or(Value::Null),
        );
        return SkillContextModifierPermissionDecision {
            allowed: true,
            payload: Some(payload),
        };
    }

    set_json_field(&mut payload, "status", json!("no_matching_allowed_tool"));
    SkillContextModifierPermissionDecision {
        allowed: false,
        payload: Some(payload),
    }
}

fn skill_context_modifier_attachment(
    host_context: &TurnContext,
    result: &ModelToolResultBlock,
) -> Option<Value> {
    if result.is_error {
        return None;
    }
    if result.payload.get("contract").and_then(Value::as_str) != Some("coder.skill_tool_result.v1")
    {
        return None;
    }
    let policy = result.payload.get("execution_policy")?;
    let allowed_tools = policy
        .get("allowed_tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>();
    let requested_model = policy
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let model = requested_model.as_ref().map(|model| {
        host_context
            .current_model
            .as_deref()
            .map(|current_model| resolve_skill_model_override(model, current_model))
            .unwrap_or_else(|| model.clone())
    });
    let effort = policy
        .get("effort")
        .filter(|value| !value.is_null())
        .cloned();
    if allowed_tools.is_empty() && model.is_none() && effort.is_none() {
        return None;
    }

    Some(json!({
        "contract": MODEL_TOOL_TURN_ATTACHMENT_CONTRACT,
        "source": "coder-server",
        "type": "skill_context_modifier",
        "modifier_contract": SKILL_CONTEXT_MODIFIER_CONTRACT,
        "tool_use_id": &result.tool_use_id,
        "tool_name": &result.tool_name,
        "skill_name": result.payload["skill_name"].clone(),
        "display_name": result.payload["display_name"].clone(),
        "skill_path": result.payload["skill_path"].clone(),
        "applies_to": "next_model_turn",
        "application_status": "propagated_for_next_model_turn",
        "modifier": {
            "allowed_tools": allowed_tools,
            "model": model,
            "requested_model": requested_model,
            "current_model": host_context.current_model.clone(),
            "effort": effort
        },
        "execution_policy": policy.clone()
    }))
}

fn resolve_skill_model_override(skill_model: &str, current_model: &str) -> String {
    if has_one_m_context(skill_model) || !has_one_m_context(current_model) {
        return skill_model.to_owned();
    }
    if skill_model_supports_one_m(skill_model) {
        return format!("{skill_model}[1m]");
    }
    skill_model.to_owned()
}

fn has_one_m_context(model: &str) -> bool {
    model.trim().to_ascii_lowercase().ends_with("[1m]")
}

fn skill_model_supports_one_m(skill_model: &str) -> bool {
    let normalized = skill_model
        .trim()
        .trim_end_matches(|character: char| character.is_ascii_whitespace())
        .trim_end_matches("[1m]")
        .trim()
        .to_ascii_lowercase();
    normalized == "opus"
        || normalized == "sonnet"
        || normalized.contains("claude-opus")
        || normalized.contains("claude-sonnet")
}

fn set_json_field(payload: &mut Value, key: &str, value: Value) {
    if let Value::Object(object) = payload {
        object.insert(key.to_owned(), value);
    }
}

fn is_active_skill_context_modifier(modifier: &Value) -> bool {
    modifier.get("type").and_then(Value::as_str) == Some("skill_context_modifier")
        && modifier.get("modifier_contract").and_then(Value::as_str)
            == Some(SKILL_CONTEXT_MODIFIER_CONTRACT)
        && modifier.get("applies_to").and_then(Value::as_str) == Some("next_model_turn")
}

struct SkillContextAllowedToolMatch {
    allowed_tool: String,
    kind: &'static str,
    command: Option<String>,
    rule_content: Option<String>,
}

fn find_matching_skill_context_allowed_tool<'a>(
    required_permission: Option<&str>,
    canonical_tool_name: &str,
    input: &Value,
    active_modifiers: &[&'a Value],
) -> Option<(&'a Value, SkillContextAllowedToolMatch)> {
    for modifier in active_modifiers {
        let allowed_tools = modifier
            .get("modifier")
            .and_then(|modifier| modifier.get("allowed_tools"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        for allowed_tool in allowed_tools {
            let Some(allowed_tool) = allowed_tool.as_str().map(str::trim) else {
                continue;
            };
            if let Some(allowed_tool_match) = skill_context_allowed_tool_matches(
                allowed_tool,
                required_permission,
                canonical_tool_name,
                input,
            ) {
                return Some((*modifier, allowed_tool_match));
            }
        }
    }
    None
}

fn skill_context_allowed_tool_matches(
    allowed_tool: &str,
    required_permission: Option<&str>,
    canonical_tool_name: &str,
    input: &Value,
) -> Option<SkillContextAllowedToolMatch> {
    match required_permission {
        Some("read_files") => skill_context_read_tool_match(allowed_tool, canonical_tool_name),
        Some("run_commands") => {
            skill_context_command_tool_match(allowed_tool, canonical_tool_name, input)
        }
        _ => None,
    }
}

fn skill_context_read_tool_match(
    allowed_tool: &str,
    canonical_tool_name: &str,
) -> Option<SkillContextAllowedToolMatch> {
    let normalized = allowed_tool.trim().to_ascii_lowercase();
    let matches = match normalized.as_str() {
        "read"
        | "view"
        | "repo_read_file"
        | "read_file"
        | "repo_read_file_range"
        | "read_file_range" => matches!(
            canonical_tool_name,
            "repo_read_file" | "repo_read_file_range"
        ),
        "grep" | "repo_search_text" | "repo_search" | "search_text" => {
            canonical_tool_name == "repo_search_text"
        }
        "glob" | "repo_find_files" | "find_files" | "repo_files" | "search_files" => {
            canonical_tool_name == "repo_find_files"
        }
        _ => false,
    };
    matches.then(|| SkillContextAllowedToolMatch {
        allowed_tool: allowed_tool.to_owned(),
        kind: "read",
        command: None,
        rule_content: None,
    })
}

fn skill_context_command_tool_match(
    allowed_tool: &str,
    canonical_tool_name: &str,
    input: &Value,
) -> Option<SkillContextAllowedToolMatch> {
    if !matches!(canonical_tool_name, "command_run" | "command_background") {
        return None;
    }
    let rule = skill_context_permission_rule_from_string(allowed_tool);
    if !matches!(
        rule.tool_name.to_ascii_lowercase().as_str(),
        "bash" | "command_run" | "command_background" | "run_command" | "run_commands"
    ) {
        return None;
    }
    let command = model_tool_command_text(input)?;
    match rule.rule_content.as_deref() {
        None => Some(SkillContextAllowedToolMatch {
            allowed_tool: allowed_tool.to_owned(),
            kind: "command_tool",
            command: Some(command),
            rule_content: None,
        }),
        Some(rule_content)
            if skill_context_shell_permission_rule_matches(rule_content, &command) =>
        {
            Some(SkillContextAllowedToolMatch {
                allowed_tool: allowed_tool.to_owned(),
                kind: "command_rule",
                command: Some(command),
                rule_content: Some(rule_content.to_owned()),
            })
        }
        _ => None,
    }
}

struct SkillContextPermissionRule {
    tool_name: String,
    rule_content: Option<String>,
}

fn skill_context_permission_rule_from_string(rule: &str) -> SkillContextPermissionRule {
    let rule = rule.trim();
    let Some(open_index) = find_first_unescaped_char(rule, '(') else {
        return SkillContextPermissionRule {
            tool_name: skill_context_normalize_legacy_tool_name(rule),
            rule_content: None,
        };
    };
    let Some(close_index) = find_last_unescaped_char(rule, ')') else {
        return SkillContextPermissionRule {
            tool_name: skill_context_normalize_legacy_tool_name(rule),
            rule_content: None,
        };
    };
    if close_index <= open_index || close_index != rule.len() - 1 || open_index == 0 {
        return SkillContextPermissionRule {
            tool_name: skill_context_normalize_legacy_tool_name(rule),
            rule_content: None,
        };
    }
    let tool_name = &rule[..open_index];
    let raw_content = &rule[open_index + 1..close_index];
    let rule_content = if raw_content.is_empty() || raw_content == "*" {
        None
    } else {
        Some(skill_context_unescape_rule_content(raw_content))
    };
    SkillContextPermissionRule {
        tool_name: skill_context_normalize_legacy_tool_name(tool_name),
        rule_content,
    }
}

fn skill_context_normalize_legacy_tool_name(tool_name: &str) -> String {
    match tool_name.trim() {
        "BashTool" => "Bash",
        other => other,
    }
    .to_owned()
}

fn skill_context_unescape_rule_content(content: &str) -> String {
    content
        .replace("\\(", "(")
        .replace("\\)", ")")
        .replace("\\\\", "\\")
}

fn model_tool_command_text(input: &Value) -> Option<String> {
    let argv = input.get("argv")?.as_array()?;
    let parts = argv
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn skill_context_shell_permission_rule_matches(rule_content: &str, command: &str) -> bool {
    let rule_content = rule_content.trim();
    if let Some(prefix) = rule_content.strip_suffix(":*") {
        let prefix = prefix.trim();
        return command == prefix || command.starts_with(&format!("{prefix} "));
    }
    if skill_context_has_unescaped_star(rule_content) {
        return skill_context_match_wildcard_pattern(rule_content, command);
    }
    command == rule_content
}

fn skill_context_has_unescaped_star(pattern: &str) -> bool {
    let chars = pattern.chars().collect::<Vec<_>>();
    chars.iter().enumerate().any(|(index, character)| {
        *character == '*' && !skill_context_char_is_escaped(&chars, index)
    })
}

fn skill_context_match_wildcard_pattern(pattern: &str, command: &str) -> bool {
    let trimmed_pattern = pattern.trim();
    if skill_context_has_single_trailing_space_wildcard(trimmed_pattern) {
        let bare = &trimmed_pattern[..trimmed_pattern.len() - 2];
        if command == bare {
            return true;
        }
    }
    let tokens = skill_context_wildcard_tokens(trimmed_pattern);
    let command_chars = command.chars().collect::<Vec<_>>();
    let mut reachable = vec![false; command_chars.len() + 1];
    reachable[0] = true;
    for token in tokens {
        let mut next = vec![false; command_chars.len() + 1];
        match token {
            SkillContextWildcardToken::Star => {
                let mut seen = false;
                for index in 0..=command_chars.len() {
                    seen |= reachable[index];
                    next[index] = seen;
                }
            }
            SkillContextWildcardToken::Char(expected) => {
                for index in 0..command_chars.len() {
                    if reachable[index] && command_chars[index] == expected {
                        next[index + 1] = true;
                    }
                }
            }
        }
        reachable = next;
    }
    reachable[command_chars.len()]
}

fn skill_context_has_single_trailing_space_wildcard(pattern: &str) -> bool {
    if !pattern.ends_with(" *") {
        return false;
    }
    let chars = pattern.chars().collect::<Vec<_>>();
    chars
        .iter()
        .enumerate()
        .filter(|(index, character)| {
            **character == '*' && !skill_context_char_is_escaped(&chars, *index)
        })
        .count()
        == 1
}

#[derive(Debug, Clone, Copy)]
enum SkillContextWildcardToken {
    Char(char),
    Star,
}

fn skill_context_wildcard_tokens(pattern: &str) -> Vec<SkillContextWildcardToken> {
    let mut tokens = Vec::new();
    let mut chars = pattern.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\\' {
            if let Some(next) = chars.peek().copied() {
                if next == '*' || next == '\\' {
                    tokens.push(SkillContextWildcardToken::Char(next));
                    chars.next();
                    continue;
                }
            }
            tokens.push(SkillContextWildcardToken::Char(character));
            continue;
        }
        if character == '*' {
            tokens.push(SkillContextWildcardToken::Star);
        } else {
            tokens.push(SkillContextWildcardToken::Char(character));
        }
    }
    tokens
}

fn find_first_unescaped_char(value: &str, target: char) -> Option<usize> {
    value.char_indices().find_map(|(index, character)| {
        (character == target && !skill_context_byte_is_escaped(value.as_bytes(), index))
            .then_some(index)
    })
}

fn find_last_unescaped_char(value: &str, target: char) -> Option<usize> {
    value.char_indices().rev().find_map(|(index, character)| {
        (character == target && !skill_context_byte_is_escaped(value.as_bytes(), index))
            .then_some(index)
    })
}

fn skill_context_byte_is_escaped(bytes: &[u8], index: usize) -> bool {
    let mut backslashes = 0usize;
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        if bytes[cursor] == b'\\' {
            backslashes += 1;
        } else {
            break;
        }
    }
    backslashes % 2 == 1
}

fn skill_context_char_is_escaped(chars: &[char], index: usize) -> bool {
    let mut backslashes = 0usize;
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        if chars[cursor] == '\\' {
            backslashes += 1;
        } else {
            break;
        }
    }
    backslashes % 2 == 1
}
