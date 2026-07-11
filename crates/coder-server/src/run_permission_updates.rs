use axum::{
    extract::{Path, State},
    Json,
};
use coder_config::{
    apply_permission_updates_to_policy, apply_permission_updates_to_settings,
    permission_settings_update_applied, permission_update_application_applied,
    permission_update_destination_supports_persistence, validate_project_config,
    PermissionDecision, PermissionRuleValue, PermissionSettingsRecord, PermissionUpdate,
    PermissionUpdateDestination, ProjectConfig, ValidationLevel,
};
use coder_core::RunId;
use coder_store::{DurableJsonlPageOptions, RunStore};
use serde_json::{json, Value};

use crate::planner_runtime::validation_issue_summary;
use crate::{
    default_project_config, now_timestamp_string, stored_run_exists, ApiError, ApiState,
    RunPermissionUpdatePersistence, RunPermissionUpdateRequest, RunPermissionUpdateResponse,
};

const PERMISSION_UPDATE_EVENT_KIND: &str = "permission.updated";
const RUN_PERMISSION_UPDATE_CONTRACT: &str = "coder.run_permission_update.v1";
const DEFAULT_RUN_PERMISSION_HARNESS_ID: &str = "native-code-edit";

pub(crate) async fn apply_run_permission_updates(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    Json(request): Json<RunPermissionUpdateRequest>,
) -> Result<Json<RunPermissionUpdateResponse>, ApiError> {
    if request.updates.is_empty() {
        return Err(ApiError::bad_request(
            "at least one permission update is required",
        ));
    }
    let run_id = RunId::from_string(run_id);
    if !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }

    let (mut config, config_source) = read_run_config_snapshot_or_default(&state.store, &run_id)?;
    let harness_id = request
        .harness_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| latest_run_permission_harness_id(&state.store, &run_id))
        .unwrap_or_else(|| DEFAULT_RUN_PERMISSION_HARNESS_ID.to_owned());
    let harness = config.harnesses.get_mut(&harness_id).ok_or_else(|| {
        ApiError::bad_request(format!(
            "harness '{harness_id}' was not found in run config"
        ))
    })?;
    let applications =
        apply_permission_updates_to_policy(&mut harness.permissions, &request.updates);
    let validation = validate_project_config(&config);
    if validation
        .issues
        .iter()
        .any(|issue| issue.level == ValidationLevel::Error)
    {
        return Err(ApiError::bad_request(format!(
            "permission updates produced invalid run config: {}",
            validation_issue_summary(&validation)
        )));
    }

    let persistence = persist_permission_update_settings(
        &state.store,
        &request.updates,
        request.source.as_deref().unwrap_or("api"),
    )?;
    let applied = permission_update_application_applied(&applications);
    let persisted = persistence
        .iter()
        .any(|result| result.status == "persisted");
    let runtime_agent_rules = runtime_agent_permission_rules_payload(&request.updates);
    let runtime_rules_applied = runtime_agent_rules
        .get("rule_count")
        .and_then(Value::as_u64)
        .unwrap_or_default()
        > 0;
    let completed = applied || persisted || runtime_rules_applied;
    let config_ref = if applied {
        Some(state.store.write_run_config_snapshot(&run_id, &config)?)
    } else {
        None
    };
    let event_sequence = if completed {
        let sequence = state.store.event_count(&run_id)? as u64 + 1;
        let event = coder_events::CoderEvent::new(
            run_id.clone(),
            sequence,
            PERMISSION_UPDATE_EVENT_KIND,
            json!({
                "contract": RUN_PERMISSION_UPDATE_CONTRACT,
                "source": "coder-server",
                "run_id": run_id.as_str(),
                "harness_id": harness_id.clone(),
                "config_source": config_source.clone(),
                "config_ref": config_ref.clone(),
                "request_source": request.source.as_deref().unwrap_or("api"),
                "updates": &request.updates,
                "runtime_agent_rules": runtime_agent_rules.clone(),
                "applications": applications.clone(),
                "persistence": persistence.clone(),
                "claude_sources": claude_permission_update_sources()
            }),
        );
        state.store.append_event(&run_id, &event)?;
        Some(sequence)
    } else {
        None
    };

    Ok(Json(RunPermissionUpdateResponse {
        contract: RUN_PERMISSION_UPDATE_CONTRACT,
        source: "coder-server",
        run_id: run_id.to_string(),
        harness_id,
        status: if completed { "completed" } else { "skipped" }.to_owned(),
        config_source,
        config_ref,
        event_sequence,
        applications,
        persistence,
        validation,
        claude_sources: claude_permission_update_sources(),
    }))
}

fn runtime_agent_permission_rules_payload(updates: &[PermissionUpdate]) -> Value {
    let mut runtime_updates = Vec::new();
    let mut rule_count = 0usize;
    for update in updates {
        let destination = update.destination();
        if !matches!(
            destination,
            PermissionUpdateDestination::Session | PermissionUpdateDestination::CliArg
        ) {
            continue;
        }
        let Some((update_type, behavior, rules)) = permission_update_rule_parts(update) else {
            continue;
        };
        if behavior != PermissionDecision::Deny {
            continue;
        }
        let agent_rules = rules
            .iter()
            .filter(|rule| content_specific_agent_rule(rule))
            .cloned()
            .collect::<Vec<_>>();
        if agent_rules.is_empty() {
            continue;
        }
        rule_count += agent_rules.len();
        runtime_updates.push(json!({
            "update_type": update_type,
            "destination": destination.as_str(),
            "behavior": behavior,
            "rules": agent_rules
        }));
    }
    json!({
        "contract": "coder.runtime_agent_permission_rules.v1",
        "source": "non-persisted PermissionUpdate session/cliArg rules",
        "rule_count": rule_count,
        "updates": runtime_updates,
        "claude_sources": [
            "src/utils/permissions/permissions.ts getDenyRuleForAgent",
            "src/utils/permissions/PermissionUpdate.ts applyPermissionUpdate"
        ]
    })
}

fn permission_update_rule_parts(
    update: &PermissionUpdate,
) -> Option<(&'static str, PermissionDecision, &[PermissionRuleValue])> {
    match update {
        PermissionUpdate::AddRules {
            rules, behavior, ..
        } => Some(("addRules", *behavior, rules)),
        PermissionUpdate::ReplaceRules {
            rules, behavior, ..
        } => Some(("replaceRules", *behavior, rules)),
        PermissionUpdate::RemoveRules {
            rules, behavior, ..
        } => Some(("removeRules", *behavior, rules)),
        _ => None,
    }
}

fn content_specific_agent_rule(rule: &PermissionRuleValue) -> bool {
    let tool_name = rule.tool_name.trim();
    let agent_tool = matches!(
        tool_name,
        "Agent" | "agent" | "Task" | "task" | "agent_subagent" | "subagent"
    );
    agent_tool
        && rule
            .rule_content
            .as_deref()
            .map(str::trim)
            .is_some_and(|content| !content.is_empty() && content != "*")
}

fn persist_permission_update_settings(
    store: &RunStore,
    updates: &[PermissionUpdate],
    request_source: &str,
) -> Result<Vec<RunPermissionUpdatePersistence>, ApiError> {
    let mut results = Vec::new();
    for destination in [
        PermissionUpdateDestination::UserSettings,
        PermissionUpdateDestination::ProjectSettings,
        PermissionUpdateDestination::LocalSettings,
        PermissionUpdateDestination::Session,
        PermissionUpdateDestination::CliArg,
    ] {
        let destination_updates = updates
            .iter()
            .filter(|update| update.destination() == destination)
            .cloned()
            .collect::<Vec<_>>();
        if destination_updates.is_empty() {
            continue;
        }

        if !permission_update_destination_supports_persistence(destination) {
            results.push(RunPermissionUpdatePersistence {
                destination,
                status: "not_persisted".to_owned(),
                update_count: destination_updates.len(),
                settings_ref: None,
                applications: Vec::new(),
                reason: Some(
                    "Claude Code persists only userSettings, projectSettings, and localSettings"
                        .to_owned(),
                ),
            });
            continue;
        }

        let mut settings = store
            .read_permission_settings::<PermissionSettingsRecord>(destination.as_str())?
            .unwrap_or_else(|| PermissionSettingsRecord::new(destination));
        settings.destination = destination;
        settings.source = "coder-server".to_owned();
        let applications =
            apply_permission_updates_to_settings(&mut settings, &destination_updates);
        if permission_settings_update_applied(&applications) {
            settings.updated_at = Some(now_timestamp_string());
            settings.last_update_source = Some(request_source.to_owned());
            let settings_ref = store.write_permission_settings(destination.as_str(), &settings)?;
            results.push(RunPermissionUpdatePersistence {
                destination,
                status: "persisted".to_owned(),
                update_count: destination_updates.len(),
                settings_ref: Some(settings_ref),
                applications,
                reason: None,
            });
        } else {
            results.push(RunPermissionUpdatePersistence {
                destination,
                status: "skipped".to_owned(),
                update_count: destination_updates.len(),
                settings_ref: None,
                applications,
                reason: Some("no settings update was applied".to_owned()),
            });
        }
    }
    Ok(results)
}

fn read_run_config_snapshot_or_default(
    store: &RunStore,
    run_id: &RunId,
) -> Result<(ProjectConfig, String), ApiError> {
    match store.read_run_config_snapshot_json(run_id)? {
        Some(value) => serde_json::from_value(value)
            .map(|config| (config, "run_config_snapshot".to_owned()))
            .map_err(|error| {
                ApiError::internal(format!(
                    "failed to parse run config snapshot for '{}': {error}",
                    run_id.as_str()
                ))
            }),
        None => Ok((
            default_project_config(),
            "default_project_config_created_for_run".to_owned(),
        )),
    }
}

fn latest_run_permission_harness_id(store: &RunStore, run_id: &RunId) -> Option<String> {
    let page = store
        .read_events_page(run_id, DurableJsonlPageOptions::tail(1000).ok()?)
        .ok()?;
    page.records.iter().rev().find_map(|event| {
        if event.kind != "node.started" {
            return None;
        }
        event
            .payload
            .get("harness")
            .or_else(|| event.payload.get("harness_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|harness_id| !harness_id.is_empty())
            .map(str::to_owned)
    })
}

fn claude_permission_update_sources() -> Vec<&'static str> {
    vec![
        "utils/permissions/PermissionUpdateSchema.ts",
        "utils/permissions/PermissionUpdate.ts applyPermissionUpdate",
        "utils/permissions/PermissionUpdate.ts persistPermissionUpdate",
        "cli/structuredIO.ts executePermissionRequestHooksForSDK",
    ]
}
