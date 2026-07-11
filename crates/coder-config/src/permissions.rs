use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{PermissionDecision, PermissionPolicy};

pub const PERMISSION_FIELDS: &[&str] = &[
    "read_files",
    "write_files",
    "run_commands",
    "child_harness_permissions",
    "network",
    "secrets",
    "publish_external",
    "git_commit",
    "git_push",
    "deploy",
];

pub const CLAUDE_PERMISSION_CONTRACT_SOURCES: &[&str] = &[
    "types/permissions.ts",
    "utils/permissions/PermissionMode.ts",
    "utils/permissions/PermissionRule.ts",
    "utils/permissions/PermissionUpdateSchema.ts",
    "utils/permissions/permissions.ts",
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    #[default]
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "acceptEdits", alias = "accept_edits")]
    AcceptEdits,
    #[serde(rename = "bypassPermissions", alias = "bypass_permissions")]
    BypassPermissions,
    #[serde(rename = "dontAsk", alias = "dont_ask")]
    DontAsk,
    #[serde(rename = "auto")]
    Auto,
}

impl PermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Plan => "plan",
            Self::AcceptEdits => "acceptEdits",
            Self::BypassPermissions => "bypassPermissions",
            Self::DontAsk => "dontAsk",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionRuleSource {
    #[serde(rename = "userSettings", alias = "user_settings")]
    UserSettings,
    #[serde(rename = "projectSettings", alias = "project_settings")]
    ProjectSettings,
    #[serde(rename = "localSettings", alias = "local_settings")]
    LocalSettings,
    #[serde(rename = "flagSettings", alias = "flag_settings")]
    FlagSettings,
    #[serde(rename = "policySettings", alias = "policy_settings")]
    PolicySettings,
    #[serde(rename = "cliArg", alias = "cli_arg")]
    CliArg,
    #[serde(rename = "command")]
    Command,
    #[serde(rename = "session")]
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRuleValue {
    #[serde(rename = "toolName")]
    pub tool_name: String,
    #[serde(
        rename = "ruleContent",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub rule_content: Option<String>,
}

impl PermissionRuleValue {
    pub fn new(tool_name: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            rule_content: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub source: PermissionRuleSource,
    #[serde(rename = "ruleBehavior")]
    pub rule_behavior: PermissionDecision,
    #[serde(rename = "ruleValue")]
    pub rule_value: PermissionRuleValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PermissionDecisionReason {
    Rule {
        rule: PermissionRule,
    },
    Mode {
        mode: PermissionMode,
    },
    Hook {
        #[serde(rename = "hookName")]
        hook_name: String,
        #[serde(
            rename = "hookSource",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        hook_source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    AsyncAgent {
        reason: String,
    },
    SandboxOverride {
        reason: String,
    },
    Classifier {
        classifier: String,
        reason: String,
    },
    WorkingDir {
        reason: String,
    },
    SafetyCheck {
        reason: String,
        #[serde(rename = "classifierApprovable")]
        classifier_approvable: bool,
    },
    Other {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionUpdateDestination {
    #[serde(rename = "userSettings", alias = "user_settings")]
    UserSettings,
    #[serde(rename = "projectSettings", alias = "project_settings")]
    ProjectSettings,
    #[serde(rename = "localSettings", alias = "local_settings")]
    LocalSettings,
    #[serde(rename = "session")]
    Session,
    #[serde(rename = "cliArg", alias = "cli_arg")]
    CliArg,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PermissionUpdate {
    AddRules {
        destination: PermissionUpdateDestination,
        rules: Vec<PermissionRuleValue>,
        behavior: PermissionDecision,
    },
    ReplaceRules {
        destination: PermissionUpdateDestination,
        rules: Vec<PermissionRuleValue>,
        behavior: PermissionDecision,
    },
    RemoveRules {
        destination: PermissionUpdateDestination,
        rules: Vec<PermissionRuleValue>,
        behavior: PermissionDecision,
    },
    SetMode {
        destination: PermissionUpdateDestination,
        mode: PermissionMode,
    },
    AddDirectories {
        directories: Vec<String>,
        destination: PermissionUpdateDestination,
    },
    RemoveDirectories {
        directories: Vec<String>,
        destination: PermissionUpdateDestination,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionEvaluation {
    pub permission: String,
    pub behavior: PermissionDecision,
    pub message: String,
    #[serde(rename = "decisionReason")]
    pub decision_reason: PermissionDecisionReason,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<PermissionUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionUpdateApplication {
    pub update_type: String,
    pub destination: PermissionUpdateDestination,
    pub status: String,
    #[serde(default)]
    pub applied_permissions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSettingsRecord {
    pub contract: String,
    pub source: String,
    pub destination: PermissionUpdateDestination,
    pub default_mode: PermissionMode,
    pub rules: PermissionSettingsRules,
    #[serde(default)]
    pub additional_directories: Vec<String>,
    #[serde(default)]
    pub updates_applied: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_update_source: Option<String>,
    #[serde(default)]
    pub claude_sources: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSettingsRules {
    #[serde(default)]
    pub allow: Vec<PermissionRuleValue>,
    #[serde(default)]
    pub ask: Vec<PermissionRuleValue>,
    #[serde(default)]
    pub deny: Vec<PermissionRuleValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSettingsUpdateApplication {
    pub update_type: String,
    pub destination: PermissionUpdateDestination,
    pub status: String,
    #[serde(default)]
    pub affected_rules: usize,
    #[serde(default)]
    pub affected_directories: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PermissionUpdateDestination {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserSettings => "userSettings",
            Self::ProjectSettings => "projectSettings",
            Self::LocalSettings => "localSettings",
            Self::Session => "session",
            Self::CliArg => "cliArg",
        }
    }
}

impl PermissionUpdate {
    pub fn destination(&self) -> PermissionUpdateDestination {
        match self {
            Self::AddRules { destination, .. }
            | Self::ReplaceRules { destination, .. }
            | Self::RemoveRules { destination, .. }
            | Self::SetMode { destination, .. }
            | Self::AddDirectories { destination, .. }
            | Self::RemoveDirectories { destination, .. } => *destination,
        }
    }
}

impl PermissionSettingsRecord {
    pub fn new(destination: PermissionUpdateDestination) -> Self {
        Self {
            contract: "coder.permission_settings.v1".to_owned(),
            source: "coder-config".to_owned(),
            destination,
            default_mode: PermissionMode::Default,
            rules: PermissionSettingsRules::default(),
            additional_directories: Vec::new(),
            updates_applied: 0,
            updated_at: None,
            last_update_source: None,
            claude_sources: CLAUDE_PERMISSION_CONTRACT_SOURCES
                .iter()
                .map(|source| (*source).to_owned())
                .chain(
                    [
                        "utils/permissions/PermissionUpdate.ts supportsPersistence",
                        "utils/permissions/PermissionUpdate.ts persistPermissionUpdate",
                    ]
                    .iter()
                    .map(|source| (*source).to_owned()),
                )
                .collect(),
        }
    }
}

pub fn permission_decision(
    permissions: &PermissionPolicy,
    permission: &str,
) -> Option<PermissionDecision> {
    match permission {
        "read_files" => Some(permissions.read_files),
        "write_files" => Some(permissions.write_files),
        "run_commands" => Some(permissions.run_commands),
        "child_harness_permissions" => Some(permissions.child_harness_permissions),
        "network" => Some(permissions.network),
        "secrets" => Some(permissions.secrets),
        "publish_external" => Some(permissions.publish_external),
        "git_commit" => Some(permissions.git_commit),
        "git_push" => Some(permissions.git_push),
        "deploy" => Some(permissions.deploy),
        _ => None,
    }
}

pub fn apply_permission_updates_to_policy(
    policy: &mut PermissionPolicy,
    updates: &[PermissionUpdate],
) -> Vec<PermissionUpdateApplication> {
    updates
        .iter()
        .map(|update| apply_permission_update_to_policy(policy, update))
        .collect()
}

pub fn permission_update_application_applied(applications: &[PermissionUpdateApplication]) -> bool {
    applications
        .iter()
        .any(|application| application.status == "applied")
}

pub fn permission_update_destination_supports_persistence(
    destination: PermissionUpdateDestination,
) -> bool {
    matches!(
        destination,
        PermissionUpdateDestination::LocalSettings
            | PermissionUpdateDestination::ProjectSettings
            | PermissionUpdateDestination::UserSettings
    )
}

pub fn apply_permission_updates_to_settings(
    settings: &mut PermissionSettingsRecord,
    updates: &[PermissionUpdate],
) -> Vec<PermissionSettingsUpdateApplication> {
    updates
        .iter()
        .map(|update| apply_permission_update_to_settings(settings, update))
        .collect()
}

pub fn permission_settings_update_applied(
    applications: &[PermissionSettingsUpdateApplication],
) -> bool {
    applications
        .iter()
        .any(|application| application.status == "applied")
}

fn apply_permission_update_to_policy(
    policy: &mut PermissionPolicy,
    update: &PermissionUpdate,
) -> PermissionUpdateApplication {
    match update {
        PermissionUpdate::AddRules {
            destination,
            rules,
            behavior,
        }
        | PermissionUpdate::ReplaceRules {
            destination,
            rules,
            behavior,
        } => {
            let permissions = permission_fields_for_rules(rules);
            if permissions.is_empty() {
                return permission_update_application(
                    update,
                    *destination,
                    "skipped",
                    permissions,
                    Some("no supported Coder permission field was found in the rule values"),
                );
            }
            for permission in &permissions {
                set_permission_decision(policy, permission, *behavior);
            }
            permission_update_application(update, *destination, "applied", permissions, None)
        }
        PermissionUpdate::RemoveRules {
            destination,
            rules,
            behavior,
        } => {
            let permissions = permission_fields_for_rules(rules);
            if permissions.is_empty() {
                return permission_update_application(
                    update,
                    *destination,
                    "skipped",
                    permissions,
                    Some("no supported Coder permission field was found in the rule values"),
                );
            }
            let defaults = PermissionPolicy::default();
            for permission in &permissions {
                if permission_decision(policy, permission) == Some(*behavior) {
                    if let Some(default_behavior) = permission_decision(&defaults, permission) {
                        set_permission_decision(policy, permission, default_behavior);
                    }
                }
            }
            permission_update_application(
                update,
                *destination,
                "applied",
                permissions,
                Some("removed rules reset matching flattened Coder permissions to defaults"),
            )
        }
        PermissionUpdate::SetMode { destination, mode } => {
            policy.mode = *mode;
            permission_update_application(update, *destination, "applied", Vec::new(), None)
        }
        PermissionUpdate::AddDirectories { destination, .. }
        | PermissionUpdate::RemoveDirectories { destination, .. } => permission_update_application(
            update,
            *destination,
            "skipped",
            Vec::new(),
            Some("Coder PermissionPolicy does not yet model additional working directories"),
        ),
    }
}

fn apply_permission_update_to_settings(
    settings: &mut PermissionSettingsRecord,
    update: &PermissionUpdate,
) -> PermissionSettingsUpdateApplication {
    let destination = update.destination();
    if !permission_update_destination_supports_persistence(destination) {
        return permission_settings_update_application(
            update,
            destination,
            "not_persisted",
            0,
            0,
            Some("Claude Code keeps session and cliArg permission updates runtime-only"),
        );
    }
    if destination != settings.destination {
        return permission_settings_update_application(
            update,
            destination,
            "skipped",
            0,
            0,
            Some("permission update destination does not match this settings record"),
        );
    }

    match update {
        PermissionUpdate::AddRules {
            rules, behavior, ..
        } => {
            let target = settings_rules_for_behavior_mut(&mut settings.rules, *behavior);
            for rule in rules {
                target.push(rule.clone());
            }
            settings.updates_applied += 1;
            permission_settings_update_application(
                update,
                destination,
                "applied",
                rules.len(),
                0,
                None,
            )
        }
        PermissionUpdate::ReplaceRules {
            rules, behavior, ..
        } => {
            *settings_rules_for_behavior_mut(&mut settings.rules, *behavior) = rules.clone();
            settings.updates_applied += 1;
            permission_settings_update_application(
                update,
                destination,
                "applied",
                rules.len(),
                0,
                None,
            )
        }
        PermissionUpdate::RemoveRules {
            rules, behavior, ..
        } => {
            let target = settings_rules_for_behavior_mut(&mut settings.rules, *behavior);
            let before = target.len();
            target.retain(|existing| !rules.iter().any(|rule| rule == existing));
            settings.updates_applied += 1;
            permission_settings_update_application(
                update,
                destination,
                "applied",
                before.saturating_sub(target.len()),
                0,
                None,
            )
        }
        PermissionUpdate::SetMode { mode, .. } => {
            settings.default_mode = *mode;
            settings.updates_applied += 1;
            permission_settings_update_application(update, destination, "applied", 0, 0, None)
        }
        PermissionUpdate::AddDirectories { directories, .. } => {
            let before = settings.additional_directories.len();
            for directory in directories {
                if !settings
                    .additional_directories
                    .iter()
                    .any(|existing| existing == directory)
                {
                    settings.additional_directories.push(directory.clone());
                }
            }
            settings.updates_applied += 1;
            permission_settings_update_application(
                update,
                destination,
                "applied",
                0,
                settings.additional_directories.len().saturating_sub(before),
                None,
            )
        }
        PermissionUpdate::RemoveDirectories { directories, .. } => {
            let before = settings.additional_directories.len();
            settings
                .additional_directories
                .retain(|existing| !directories.iter().any(|directory| directory == existing));
            settings.updates_applied += 1;
            permission_settings_update_application(
                update,
                destination,
                "applied",
                0,
                before.saturating_sub(settings.additional_directories.len()),
                None,
            )
        }
    }
}

fn settings_rules_for_behavior_mut(
    rules: &mut PermissionSettingsRules,
    behavior: PermissionDecision,
) -> &mut Vec<PermissionRuleValue> {
    match behavior {
        PermissionDecision::Allow => &mut rules.allow,
        PermissionDecision::Ask => &mut rules.ask,
        PermissionDecision::Deny => &mut rules.deny,
    }
}

fn permission_update_application(
    update: &PermissionUpdate,
    destination: PermissionUpdateDestination,
    status: &str,
    applied_permissions: Vec<String>,
    reason: Option<&str>,
) -> PermissionUpdateApplication {
    PermissionUpdateApplication {
        update_type: permission_update_type(update).to_owned(),
        destination,
        status: status.to_owned(),
        applied_permissions,
        reason: reason.map(str::to_owned),
    }
}

fn permission_settings_update_application(
    update: &PermissionUpdate,
    destination: PermissionUpdateDestination,
    status: &str,
    affected_rules: usize,
    affected_directories: usize,
    reason: Option<&str>,
) -> PermissionSettingsUpdateApplication {
    PermissionSettingsUpdateApplication {
        update_type: permission_update_type(update).to_owned(),
        destination,
        status: status.to_owned(),
        affected_rules,
        affected_directories,
        reason: reason.map(str::to_owned),
    }
}

fn permission_update_type(update: &PermissionUpdate) -> &'static str {
    match update {
        PermissionUpdate::AddRules { .. } => "addRules",
        PermissionUpdate::ReplaceRules { .. } => "replaceRules",
        PermissionUpdate::RemoveRules { .. } => "removeRules",
        PermissionUpdate::SetMode { .. } => "setMode",
        PermissionUpdate::AddDirectories { .. } => "addDirectories",
        PermissionUpdate::RemoveDirectories { .. } => "removeDirectories",
    }
}

fn permission_fields_for_rules(rules: &[PermissionRuleValue]) -> Vec<String> {
    let mut permissions = Vec::new();
    for rule in rules {
        for permission in permission_fields_for_rule(rule) {
            if !permissions.iter().any(|value| value == permission) {
                permissions.push(permission.to_owned());
            }
        }
    }
    permissions
}

fn permission_fields_for_rule(rule: &PermissionRuleValue) -> Vec<&'static str> {
    if is_content_specific_agent_rule(rule) {
        return Vec::new();
    }
    let mut fields = permission_fields_for_rule_part(&rule.tool_name);
    if let Some(rule_content) = rule.rule_content.as_deref() {
        for field in permission_fields_for_rule_part(rule_content) {
            if !fields.contains(&field) {
                fields.push(field);
            }
        }
    }
    fields
}

fn is_content_specific_agent_rule(rule: &PermissionRuleValue) -> bool {
    let tool_name = rule_tool_head(&rule.tool_name);
    if !matches!(
        tool_name.as_str(),
        "agent" | "task" | "agent_subagent" | "subagent"
    ) {
        return false;
    }
    rule.rule_content
        .as_deref()
        .map(str::trim)
        .is_some_and(|content| !content.is_empty() && content != "*")
}

fn permission_fields_for_rule_part(value: &str) -> Vec<&'static str> {
    match rule_tool_head(value).as_str() {
        "read_files" | "read" | "view" | "glob" | "grep" | "ls" | "notebookread" => {
            vec!["read_files"]
        }
        "write_files" | "edit" | "write" | "multiedit" | "notebookedit" | "patch_apply"
        | "apply_patch" => vec!["write_files"],
        "run_commands" | "bash" | "bashtool" | "powershell" | "powershelltool" | "command_run"
        | "run_command" | "command_background" => vec!["run_commands"],
        "child_harness_permissions" | "agent" | "task" | "agent_subagent" | "subagent" => {
            vec!["child_harness_permissions"]
        }
        "network" | "webfetch" | "websearch" | "http" => vec!["network"],
        "secrets" | "secret" => vec!["secrets"],
        "publish_external" | "publish" => vec!["publish_external"],
        "git_commit" => vec!["git_commit"],
        "git_push" => vec!["git_push"],
        "deploy" => vec!["deploy"],
        "skill" | "skilltool" => vec!["read_files"],
        _ => Vec::new(),
    }
}

fn rule_tool_head(value: &str) -> String {
    let normalized = value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_ascii_lowercase()
        .replace(['-', ' ', ':'], "_");
    normalized
        .split(['(', '[', '{'])
        .next()
        .unwrap_or(normalized.as_str())
        .trim()
        .to_owned()
}

fn set_permission_decision(
    policy: &mut PermissionPolicy,
    permission: &str,
    decision: PermissionDecision,
) {
    match permission {
        "read_files" => policy.read_files = decision,
        "write_files" => policy.write_files = decision,
        "run_commands" => policy.run_commands = decision,
        "child_harness_permissions" => policy.child_harness_permissions = decision,
        "network" => policy.network = decision,
        "secrets" => policy.secrets = decision,
        "publish_external" => policy.publish_external = decision,
        "git_commit" => policy.git_commit = decision,
        "git_push" => policy.git_push = decision,
        "deploy" => policy.deploy = decision,
        _ => {}
    }
}

pub fn permission_policy_rules(policy: &PermissionPolicy) -> Vec<PermissionRule> {
    PERMISSION_FIELDS
        .iter()
        .filter_map(|permission| {
            permission_decision(policy, permission).map(|decision| PermissionRule {
                source: PermissionRuleSource::PolicySettings,
                rule_behavior: decision,
                rule_value: PermissionRuleValue::new(*permission),
            })
        })
        .collect()
}

pub fn evaluate_permission(
    policy: &PermissionPolicy,
    permission: &str,
) -> Option<PermissionEvaluation> {
    let behavior = permission_decision(policy, permission)?;
    let rule = PermissionRule {
        source: PermissionRuleSource::PolicySettings,
        rule_behavior: behavior,
        rule_value: PermissionRuleValue::new(permission),
    };
    let message = match behavior {
        PermissionDecision::Allow => format!("Permission '{permission}' is allowed by policy"),
        PermissionDecision::Ask => format!("Permission '{permission}' requires confirmation"),
        PermissionDecision::Deny => format!("Permission '{permission}' is denied by policy"),
    };
    Some(PermissionEvaluation {
        permission: permission.to_owned(),
        behavior,
        message,
        decision_reason: PermissionDecisionReason::Rule { rule },
        suggestions: permission_suggestions(permission, behavior),
    })
}

pub fn permission_policy_explanation(policy: &PermissionPolicy) -> Value {
    let evaluations = PERMISSION_FIELDS
        .iter()
        .filter_map(|permission| evaluate_permission(policy, permission))
        .collect::<Vec<_>>();
    json!({
        "contract": "coder.permission_policy.v1",
        "claude_sources": CLAUDE_PERMISSION_CONTRACT_SOURCES,
        "mode": policy.mode,
        "mode_semantics": {
            "source": "config.permissions.mode",
            "note": "Coder keeps explicit harness permission fields authoritative; mode is exposed for UI/planner reasoning and future interactive rule updates."
        },
        "rule_precedence": ["deny", "ask", "allow"],
        "rule_source_precedence": [
            "policySettings",
            "cliArg",
            "session",
            "localSettings",
            "projectSettings",
            "userSettings"
        ],
        "rules": permission_policy_rules(policy),
        "summary": {
            "read_files": policy.read_files,
            "write_files": policy.write_files,
            "run_commands": policy.run_commands,
            "child_harness_permissions": policy.child_harness_permissions,
            "network": policy.network,
            "secrets": policy.secrets,
            "publish_external": policy.publish_external,
            "git_commit": policy.git_commit,
            "git_push": policy.git_push,
            "deploy": policy.deploy
        },
        "decisions": evaluations
    })
}

fn permission_suggestions(permission: &str, behavior: PermissionDecision) -> Vec<PermissionUpdate> {
    if behavior != PermissionDecision::Ask {
        return Vec::new();
    }
    vec![
        PermissionUpdate::AddRules {
            destination: PermissionUpdateDestination::Session,
            rules: vec![PermissionRuleValue::new(permission)],
            behavior: PermissionDecision::Allow,
        },
        PermissionUpdate::AddRules {
            destination: PermissionUpdateDestination::Session,
            rules: vec![PermissionRuleValue::new(permission)],
            behavior: PermissionDecision::Deny,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_policy_explanation_uses_claude_rule_contract() {
        let policy = PermissionPolicy::default();
        let explanation = permission_policy_explanation(&policy);

        assert_eq!(explanation["contract"], "coder.permission_policy.v1");
        assert_eq!(explanation["mode"], "default");
        assert_eq!(
            explanation["rules"][0]["source"],
            serde_json::json!("policySettings")
        );
        assert_eq!(
            explanation["rules"][0]["ruleValue"]["toolName"],
            serde_json::json!("read_files")
        );

        let write_decision = explanation["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|decision| decision["permission"] == "write_files")
            .unwrap();
        assert_eq!(write_decision["behavior"], "ask");
        assert_eq!(write_decision["decisionReason"]["type"], "rule");
        assert_eq!(write_decision["suggestions"][0]["type"], "addRules");
        assert_eq!(write_decision["suggestions"][0]["destination"], "session");

        let child_decision = explanation["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|decision| decision["permission"] == "child_harness_permissions")
            .unwrap();
        assert_eq!(child_decision["behavior"], "ask");
        assert_eq!(
            explanation["summary"]["child_harness_permissions"],
            serde_json::json!("ask")
        );
    }

    #[test]
    fn permission_mode_accepts_claude_camel_case_names() {
        let policy: PermissionPolicy = serde_yaml::from_str(
            r#"
mode: acceptEdits
read_files: allow
write_files: ask
run_commands: ask
"#,
        )
        .unwrap();

        assert_eq!(policy.mode, PermissionMode::AcceptEdits);
        assert_eq!(policy.read_files, PermissionDecision::Allow);
    }

    #[test]
    fn permission_updates_apply_claude_tool_rules_to_coder_policy_fields() {
        let mut policy = PermissionPolicy::default();
        let applications = apply_permission_updates_to_policy(
            &mut policy,
            &[PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::Session,
                rules: vec![PermissionRuleValue {
                    tool_name: "Bash".to_owned(),
                    rule_content: Some("Bash(*)".to_owned()),
                }],
                behavior: PermissionDecision::Allow,
            }],
        );

        assert!(permission_update_application_applied(&applications));
        assert_eq!(applications[0].status, "applied");
        assert_eq!(applications[0].applied_permissions, vec!["run_commands"]);
        assert_eq!(policy.run_commands, PermissionDecision::Allow);

        let applications = apply_permission_updates_to_policy(
            &mut policy,
            &[PermissionUpdate::RemoveRules {
                destination: PermissionUpdateDestination::Session,
                rules: vec![PermissionRuleValue::new("run_commands")],
                behavior: PermissionDecision::Allow,
            }],
        );

        assert_eq!(applications[0].status, "applied");
        assert_eq!(
            policy.run_commands,
            PermissionPolicy::default().run_commands
        );
    }

    #[test]
    fn content_specific_agent_rules_do_not_flatten_to_whole_subagent_permission() {
        let mut policy = PermissionPolicy::default();
        let applications = apply_permission_updates_to_policy(
            &mut policy,
            &[PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                rules: vec![PermissionRuleValue {
                    tool_name: "Agent".to_owned(),
                    rule_content: Some("reviewer".to_owned()),
                }],
                behavior: PermissionDecision::Deny,
            }],
        );

        assert!(!permission_update_application_applied(&applications));
        assert_eq!(applications[0].status, "skipped");
        assert_eq!(applications[0].applied_permissions, Vec::<String>::new());
        assert_eq!(
            policy.child_harness_permissions,
            PermissionPolicy::default().child_harness_permissions
        );

        let applications = apply_permission_updates_to_policy(
            &mut policy,
            &[PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                rules: vec![PermissionRuleValue::new("Agent")],
                behavior: PermissionDecision::Deny,
            }],
        );

        assert!(permission_update_application_applied(&applications));
        assert_eq!(
            applications[0].applied_permissions,
            vec!["child_harness_permissions"]
        );
        assert_eq!(policy.child_harness_permissions, PermissionDecision::Deny);
    }

    #[test]
    fn permission_updates_apply_to_persisted_settings_only_for_settings_destinations() {
        let mut settings =
            PermissionSettingsRecord::new(PermissionUpdateDestination::LocalSettings);
        let applications = apply_permission_updates_to_settings(
            &mut settings,
            &[
                PermissionUpdate::AddRules {
                    destination: PermissionUpdateDestination::LocalSettings,
                    rules: vec![PermissionRuleValue::new("Bash")],
                    behavior: PermissionDecision::Allow,
                },
                PermissionUpdate::AddDirectories {
                    destination: PermissionUpdateDestination::LocalSettings,
                    directories: vec!["F:/work".to_owned(), "F:/work".to_owned()],
                },
                PermissionUpdate::SetMode {
                    destination: PermissionUpdateDestination::LocalSettings,
                    mode: PermissionMode::AcceptEdits,
                },
                PermissionUpdate::AddRules {
                    destination: PermissionUpdateDestination::Session,
                    rules: vec![PermissionRuleValue::new("Read")],
                    behavior: PermissionDecision::Allow,
                },
            ],
        );

        assert_eq!(applications[0].status, "applied");
        assert_eq!(applications[1].affected_directories, 1);
        assert_eq!(applications[2].status, "applied");
        assert_eq!(applications[3].status, "not_persisted");
        assert_eq!(settings.default_mode, PermissionMode::AcceptEdits);
        assert_eq!(settings.rules.allow, vec![PermissionRuleValue::new("Bash")]);
        assert_eq!(settings.additional_directories, vec!["F:/work"]);
        assert_eq!(settings.updates_applied, 3);
        assert!(permission_settings_update_applied(&applications));
    }
}
