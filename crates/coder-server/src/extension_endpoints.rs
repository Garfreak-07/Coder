use axum::{
    extract::{Path, Query, State},
    Json,
};
use coder_core::RunId;
use coder_extensions::{
    builtin_plugin_manifests, builtin_remote_skill_entries, discover_skills_payload,
    extension_search, installed_skills_payload, remote_skill_summary, validate_plugin_manifest,
    validate_skill_manifest, DiscoverSkillsPayload, InstalledSkillsPayload,
    PluginManifestValidation, RemoteSkillEntry, SkillManifestValidation, SkillSummary,
    SkillUpdateInfo,
};
use serde_json::json;
use std::collections::BTreeSet;

use crate::stored_run_exists;
use crate::{
    estimate_text_tokens, truncate_text_to_chars, ApiError, ApiState, ExtensionInstalledResponse,
    ExtensionPluginListResponse, ExtensionPluginValidationRequest, ExtensionSearchQuery,
    ExtensionSkillListResponse, HookSummary, HooksResponse, InstalledSkillRecord,
    PluginListResponse, PluginMarketplace, PluginMarketplaceActionResponse,
    PluginMarketplaceListResponse, PluginMarketplaceRemoveResponse, PluginMarketplaceRequest,
    PluginMarketplaceUpgradeResponse, PluginReadResponse, PluginSkillReadResponse,
    SkillActionResponse, SkillExtraRoot, SkillExtraRootRequest, SkillExtraRootsResponse,
    SkillInstallRequest, SkillInvocationRecordRequest, SkillInvocationRecordResponse,
    SkillManifestValidationRequest, SkillPinRequest, SkillRegistryQuery, SkillUpdatePolicyRequest,
    SkillUpdateRequest, SkillUpdatesResponse, INVOKED_SKILL_CONTRACT, INVOKED_SKILL_EVENT_KIND,
    POST_COMPACT_MAX_CHARS_PER_SKILL,
};

pub(crate) async fn list_extension_plugins() -> Json<ExtensionPluginListResponse> {
    Json(ExtensionPluginListResponse {
        plugins: builtin_plugin_manifests(),
    })
}

pub(crate) async fn validate_extension_plugin(
    Json(request): Json<ExtensionPluginValidationRequest>,
) -> Json<PluginManifestValidation> {
    Json(validate_plugin_manifest(&request.manifest))
}

pub(crate) async fn validate_extension_skill(
    Json(request): Json<SkillManifestValidationRequest>,
) -> Json<SkillManifestValidation> {
    Json(validate_skill_manifest(&request.manifest))
}

pub(crate) async fn list_extension_skills(
    State(state): State<ApiState>,
) -> Json<ExtensionSkillListResponse> {
    let skills = installed_skill_summaries(&state);
    let extensions = extension_search("", &[], &skills);
    Json(ExtensionSkillListResponse {
        skills: extensions
            .into_iter()
            .filter(|extension| extension.extension_type == "skill")
            .collect(),
    })
}

pub(crate) async fn list_extensions_installed(
    State(state): State<ApiState>,
) -> Json<ExtensionInstalledResponse> {
    let skills = installed_skill_summaries(&state);
    Json(ExtensionInstalledResponse {
        extensions: extension_search("", &builtin_plugin_manifests(), &skills),
    })
}

pub(crate) async fn search_extensions_endpoint(
    State(state): State<ApiState>,
    Query(query): Query<ExtensionSearchQuery>,
) -> Json<ExtensionInstalledResponse> {
    let skills = installed_skill_summaries(&state);
    Json(ExtensionInstalledResponse {
        extensions: extension_search(
            query.q.as_deref().unwrap_or_default(),
            &builtin_plugin_manifests(),
            &skills,
        ),
    })
}

pub(crate) async fn list_installed_skills(
    State(state): State<ApiState>,
) -> Json<InstalledSkillsPayload> {
    Json(installed_skills_payload(installed_skill_summaries(&state)))
}

pub(crate) async fn discover_skills_endpoint(
    State(state): State<ApiState>,
    Query(query): Query<SkillRegistryQuery>,
) -> Json<DiscoverSkillsPayload> {
    let installed_ids = state
        .installed_skills
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    Json(discover_skills_payload(
        query.registry_url.as_deref().unwrap_or_default(),
        &installed_ids,
    ))
}

pub(crate) async fn list_skill_updates(
    State(state): State<ApiState>,
    Query(_query): Query<SkillRegistryQuery>,
) -> Json<SkillUpdatesResponse> {
    let installed = state.installed_skills.lock().unwrap();
    let updates = installed
        .values()
        .map(skill_update_info)
        .collect::<Vec<_>>();
    Json(SkillUpdatesResponse { updates })
}

pub(crate) async fn install_skill(
    State(state): State<ApiState>,
    Json(request): Json<SkillInstallRequest>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let entry = available_skill(&request.skill_id).ok_or_else(|| {
        ApiError::not_found(format!("skill '{}' was not found", request.skill_id))
    })?;
    let mut installed = state.installed_skills.lock().unwrap();
    let previous = installed.get(&entry.id).cloned();
    let mut record = InstalledSkillRecord::from_remote(&entry, true, request.registry_url);
    if let Some(previous) = previous {
        record.history = previous.history;
        record.history.push(previous.summary);
        record.pinned_version = previous.pinned_version;
        record.update_policy = previous.update_policy;
    }
    let summary = record.summary.clone();
    installed.insert(summary.id.clone(), record);
    Ok(Json(SkillActionResponse {
        skill_id: summary.id.clone(),
        status: "installed".to_owned(),
        skill: Some(summary),
        deleted: false,
        updated: Vec::new(),
    }))
}

pub(crate) async fn update_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
    Json(request): Json<SkillUpdateRequest>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let entry = available_skill(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' was not found")))?;
    let mut installed = state.installed_skills.lock().unwrap();
    let current = installed
        .get(&skill_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    if current.pinned_version.is_some() && current.pinned_version.as_deref() != Some(&entry.version)
    {
        return Ok(Json(SkillActionResponse {
            skill_id,
            status: "pinned".to_owned(),
            skill: Some(current.summary),
            deleted: false,
            updated: Vec::new(),
        }));
    }
    let mut next =
        InstalledSkillRecord::from_remote(&entry, current.summary.enabled, request.registry_url);
    next.history = current.history;
    if current.summary.version != next.summary.version {
        next.history.push(current.summary);
    }
    next.pinned_version = current.pinned_version;
    next.update_policy = current.update_policy;
    let summary = next.summary.clone();
    installed.insert(skill_id.clone(), next);
    Ok(Json(SkillActionResponse {
        skill_id,
        status: "updated".to_owned(),
        skill: Some(summary),
        deleted: false,
        updated: Vec::new(),
    }))
}

pub(crate) async fn auto_update_skills(
    State(state): State<ApiState>,
    Json(_request): Json<SkillRegistryQuery>,
) -> Json<SkillActionResponse> {
    let mut installed = state.installed_skills.lock().unwrap();
    let mut updated = Vec::new();
    let ids = installed.keys().cloned().collect::<Vec<_>>();
    for skill_id in ids {
        let Some(current) = installed.get(&skill_id).cloned() else {
            continue;
        };
        if !auto_update_allowed(&current) {
            continue;
        }
        let Some(entry) = available_skill(&skill_id) else {
            continue;
        };
        if entry.version == current.summary.version {
            continue;
        }
        let mut next =
            InstalledSkillRecord::from_remote(&entry, current.summary.enabled, current.source_url);
        next.history = current.history;
        next.history.push(current.summary);
        next.update_policy = current.update_policy;
        updated.push(next.summary.clone());
        installed.insert(skill_id, next);
    }
    Json(SkillActionResponse {
        skill_id: "all".to_owned(),
        status: "auto_update_completed".to_owned(),
        skill: None,
        deleted: false,
        updated,
    })
}

pub(crate) async fn enable_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    set_skill_enabled(state, skill_id, true)
}

pub(crate) async fn disable_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    set_skill_enabled(state, skill_id, false)
}

pub(crate) async fn remove_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let removed = state
        .installed_skills
        .lock()
        .unwrap()
        .remove(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    Ok(Json(SkillActionResponse {
        skill_id: removed.summary.id,
        status: "removed".to_owned(),
        skill: None,
        deleted: true,
        updated: Vec::new(),
    }))
}

pub(crate) async fn pin_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
    Json(request): Json<SkillPinRequest>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let mut installed = state.installed_skills.lock().unwrap();
    let record = installed
        .get_mut(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    let version = request
        .version
        .filter(|version| !version.trim().is_empty())
        .unwrap_or_else(|| record.summary.version.clone());
    let available_versions = record
        .history
        .iter()
        .map(|skill| skill.version.clone())
        .chain(std::iter::once(record.summary.version.clone()))
        .collect::<BTreeSet<_>>();
    if !available_versions.contains(&version) {
        return Err(ApiError::bad_request(format!(
            "version '{version}' is not available for skill '{skill_id}'"
        )));
    }
    record.pinned_version = Some(version);
    record.update_policy = "manual".to_owned();
    Ok(Json(SkillActionResponse {
        skill_id,
        status: "pinned".to_owned(),
        skill: Some(record.summary.clone()),
        deleted: false,
        updated: Vec::new(),
    }))
}

pub(crate) async fn unpin_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let mut installed = state.installed_skills.lock().unwrap();
    let record = installed
        .get_mut(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    record.pinned_version = None;
    Ok(Json(SkillActionResponse {
        skill_id,
        status: "unpinned".to_owned(),
        skill: Some(record.summary.clone()),
        deleted: false,
        updated: Vec::new(),
    }))
}

pub(crate) async fn rollback_skill(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
    Json(_request): Json<SkillPinRequest>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let mut installed = state.installed_skills.lock().unwrap();
    let record = installed
        .get_mut(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    let status = if let Some(previous) = record.history.pop() {
        record.summary = previous;
        "rolled_back"
    } else {
        "no_history"
    };
    Ok(Json(SkillActionResponse {
        skill_id,
        status: status.to_owned(),
        skill: Some(record.summary.clone()),
        deleted: false,
        updated: Vec::new(),
    }))
}

pub(crate) async fn set_skill_update_policy(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
    Json(request): Json<SkillUpdatePolicyRequest>,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let mut installed = state.installed_skills.lock().unwrap();
    let record = installed
        .get_mut(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    match request.update_policy.as_str() {
        "manual" => record.update_policy = "manual".to_owned(),
        "auto_official_low_risk" if auto_update_allowed(record) => {
            record.update_policy = "auto_official_low_risk".to_owned();
        }
        "auto_official_low_risk" => {
            return Err(ApiError::bad_request(
                "auto-update is only allowed for official low-risk skills without external effects",
            ));
        }
        other => {
            return Err(ApiError::bad_request(format!(
                "unsupported update_policy '{other}'"
            )));
        }
    }
    Ok(Json(SkillActionResponse {
        skill_id,
        status: "update_policy_set".to_owned(),
        skill: Some(record.summary.clone()),
        deleted: false,
        updated: Vec::new(),
    }))
}

pub(crate) async fn developer_import_skill() -> Result<Json<SkillActionResponse>, ApiError> {
    Err(ApiError::forbidden(
        "developer skill import is disabled in Rust v3 baseline; use explicit user-controlled install flow",
    ))
}

pub(crate) async fn list_plugin_marketplaces(
    State(state): State<ApiState>,
) -> Json<PluginMarketplaceListResponse> {
    Json(PluginMarketplaceListResponse {
        marketplaces: state
            .plugin_marketplaces
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect(),
    })
}

pub(crate) async fn add_plugin_marketplace(
    State(state): State<ApiState>,
    Json(request): Json<PluginMarketplaceRequest>,
) -> Result<Json<PluginMarketplaceActionResponse>, ApiError> {
    if request.name.trim().is_empty() || request.url.trim().is_empty() {
        return Err(ApiError::bad_request(
            "marketplace name and url must not be empty",
        ));
    }
    let marketplace = PluginMarketplace {
        name: request.name,
        url: request.url,
        enabled: request.enabled.unwrap_or(true),
    };
    state
        .plugin_marketplaces
        .lock()
        .unwrap()
        .insert(marketplace.name.clone(), marketplace.clone());
    Ok(Json(PluginMarketplaceActionResponse {
        status: "added".to_owned(),
        marketplace,
    }))
}

pub(crate) async fn remove_plugin_marketplace(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<Json<PluginMarketplaceRemoveResponse>, ApiError> {
    if name == "builtin" {
        return Err(ApiError::bad_request(
            "the builtin marketplace cannot be removed",
        ));
    }
    let removed = state
        .plugin_marketplaces
        .lock()
        .unwrap()
        .remove(&name)
        .is_some();
    Ok(Json(PluginMarketplaceRemoveResponse { name, removed }))
}

pub(crate) async fn upgrade_plugin_marketplace(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<Json<PluginMarketplaceUpgradeResponse>, ApiError> {
    if !state
        .plugin_marketplaces
        .lock()
        .unwrap()
        .contains_key(&name)
    {
        return Err(ApiError::not_found(format!(
            "plugin marketplace '{name}' was not found"
        )));
    }
    Ok(Json(PluginMarketplaceUpgradeResponse {
        name,
        status: "up_to_date".to_owned(),
        updated_plugins: Vec::new(),
        updated_skills: Vec::new(),
    }))
}

pub(crate) async fn list_plugins() -> Json<PluginListResponse> {
    Json(PluginListResponse {
        plugins: builtin_plugin_manifests(),
    })
}

pub(crate) async fn list_installed_plugins() -> Json<PluginListResponse> {
    Json(PluginListResponse {
        plugins: builtin_plugin_manifests()
            .into_iter()
            .filter(|plugin| plugin.installed)
            .collect(),
    })
}

pub(crate) async fn read_plugin(
    Path(plugin_id): Path<String>,
) -> Result<Json<PluginReadResponse>, ApiError> {
    let plugin = builtin_plugin_manifests()
        .into_iter()
        .find(|plugin| plugin.id == plugin_id)
        .ok_or_else(|| ApiError::not_found(format!("plugin '{plugin_id}' was not found")))?;
    Ok(Json(PluginReadResponse {
        plugin,
        skills: builtin_remote_skill_entries(),
        mcp_dependencies: Vec::new(),
        hooks: builtin_hooks(),
    }))
}

pub(crate) async fn read_plugin_skill(
    Path((plugin_id, skill_name)): Path<(String, String)>,
) -> Result<Json<PluginSkillReadResponse>, ApiError> {
    if !builtin_plugin_manifests()
        .into_iter()
        .any(|plugin| plugin.id == plugin_id)
    {
        return Err(ApiError::not_found(format!(
            "plugin '{plugin_id}' was not found"
        )));
    }
    let skill = builtin_remote_skill_entries()
        .into_iter()
        .find(|skill| skill.id == skill_name || skill.name == skill_name)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_name}' was not found")))?;
    Ok(Json(PluginSkillReadResponse { plugin_id, skill }))
}

pub(crate) async fn record_invoked_skill(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    Json(request): Json<SkillInvocationRecordRequest>,
) -> Result<Json<SkillInvocationRecordResponse>, ApiError> {
    let run_id = RunId::from_string(run_id);
    if state.store.read_events(&run_id)?.is_empty() && !stored_run_exists(&state.store, &run_id)? {
        return Err(ApiError::not_found(format!(
            "run '{}' was not found",
            run_id.as_str()
        )));
    }
    let skill_name = request.skill_name.trim();
    let skill_path = request.skill_path.trim();
    if skill_name.is_empty() {
        return Err(ApiError::bad_request("skill_name is required"));
    }
    if skill_path.is_empty() {
        return Err(ApiError::bad_request("skill_path is required"));
    }
    if request.content.trim().is_empty() {
        return Err(ApiError::bad_request("content is required"));
    }
    let agent_id = request
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let (content, content_truncated) =
        truncate_text_to_chars(&request.content, POST_COMPACT_MAX_CHARS_PER_SKILL);
    let content_estimated_tokens = estimate_text_tokens(&content);
    let sequence = state.store.event_count(&run_id)? as u64 + 1;
    let event = coder_events::CoderEvent::new(
        run_id.clone(),
        sequence,
        INVOKED_SKILL_EVENT_KIND,
        json!({
            "contract": INVOKED_SKILL_CONTRACT,
            "source": "coder-server",
            "skill_name": skill_name,
            "skill_path": skill_path,
            "content": content,
            "content_truncated": content_truncated,
            "content_estimated_tokens": content_estimated_tokens,
            "agent_id": agent_id
        }),
    );
    state.store.append_event(&run_id, &event)?;
    Ok(Json(SkillInvocationRecordResponse {
        contract: INVOKED_SKILL_CONTRACT,
        source: "coder-server",
        run_id: run_id.to_string(),
        skill_name: skill_name.to_owned(),
        skill_path: skill_path.to_owned(),
        agent_id,
        event_sequence: sequence,
        content_truncated,
        content_estimated_tokens,
    }))
}

pub(crate) async fn list_skill_extra_roots(
    State(state): State<ApiState>,
) -> Json<SkillExtraRootsResponse> {
    Json(SkillExtraRootsResponse {
        roots: state.skill_extra_roots.lock().unwrap().clone(),
    })
}

pub(crate) async fn add_skill_extra_root(
    State(state): State<ApiState>,
    Json(request): Json<SkillExtraRootRequest>,
) -> Result<Json<SkillExtraRootsResponse>, ApiError> {
    if request.path.trim().is_empty() {
        return Err(ApiError::bad_request("skill root path must not be empty"));
    }
    let root = SkillExtraRoot {
        path: request.path,
        scope: request.scope.unwrap_or_else(|| "user".to_owned()),
        enabled: request.enabled.unwrap_or(true),
    };
    let mut roots = state.skill_extra_roots.lock().unwrap();
    if !roots.iter().any(|item| item.path == root.path) {
        roots.push(root);
    }
    Ok(Json(SkillExtraRootsResponse {
        roots: roots.clone(),
    }))
}

pub(crate) async fn list_hooks() -> Json<HooksResponse> {
    Json(HooksResponse {
        hooks: builtin_hooks(),
    })
}

impl InstalledSkillRecord {
    fn from_remote(entry: &RemoteSkillEntry, enabled: bool, source_url: Option<String>) -> Self {
        Self {
            summary: remote_skill_summary(entry, enabled),
            source_url,
            pinned_version: None,
            update_policy: "manual".to_owned(),
            history: Vec::new(),
        }
    }
}

fn installed_skill_summaries(state: &ApiState) -> Vec<SkillSummary> {
    state
        .installed_skills
        .lock()
        .unwrap()
        .values()
        .map(|record| record.summary.clone())
        .collect()
}

fn available_skill(skill_id: &str) -> Option<RemoteSkillEntry> {
    builtin_remote_skill_entries()
        .into_iter()
        .find(|entry| entry.id == skill_id)
}

fn skill_update_info(record: &InstalledSkillRecord) -> SkillUpdateInfo {
    let available = available_skill(&record.summary.id);
    let available_version = available.as_ref().map(|entry| entry.version.clone());
    let update_available = available_version
        .as_deref()
        .map(|version| version != record.summary.version)
        .unwrap_or(false);
    SkillUpdateInfo {
        skill_id: record.summary.id.clone(),
        installed_version: record.summary.version.clone(),
        available_version,
        update_available,
        auto_update_eligible: auto_update_allowed(record),
        pinned_version: record.pinned_version.clone(),
        update_policy: record.update_policy.clone(),
        reason: if available.is_some() {
            None
        } else {
            Some("not listed in Rust v3 builtin registry".to_owned())
        },
        risk_level: record.summary.risk_level,
        trust_level: record.summary.trust_level,
        external_effect: record.summary.external_effect,
    }
}

fn auto_update_allowed(record: &InstalledSkillRecord) -> bool {
    record.summary.trust_level == coder_extensions::SkillTrustLevel::Official
        && record.summary.risk_level == coder_extensions::SkillRiskLevel::Low
        && !record.summary.external_effect
        && record.pinned_version.is_none()
}

fn builtin_hooks() -> Vec<HookSummary> {
    vec![
        HookSummary {
            id: "approval.guardian".to_owned(),
            trigger: "approval.requested".to_owned(),
            enabled: true,
            description: "Routes risky executor actions through human approval.".to_owned(),
        },
        HookSummary {
            id: "final-summary".to_owned(),
            trigger: "run.finalizing".to_owned(),
            enabled: true,
            description: "Builds the evidence-backed final summary.".to_owned(),
        },
    ]
}

fn set_skill_enabled(
    state: ApiState,
    skill_id: String,
    enabled: bool,
) -> Result<Json<SkillActionResponse>, ApiError> {
    let mut installed = state.installed_skills.lock().unwrap();
    let record = installed
        .get_mut(&skill_id)
        .ok_or_else(|| ApiError::not_found(format!("skill '{skill_id}' is not installed")))?;
    record.summary.enabled = enabled;
    Ok(Json(SkillActionResponse {
        skill_id,
        status: if enabled { "enabled" } else { "disabled" }.to_owned(),
        skill: Some(record.summary.clone()),
        deleted: false,
        updated: Vec::new(),
    }))
}
