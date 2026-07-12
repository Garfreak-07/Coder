use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

mod stdio_mcp;

pub use stdio_mcp::{
    StdioMcpCallOutput, StdioMcpError, StdioMcpRuntime, DEFAULT_MCP_STARTUP_TIMEOUT_SECONDS,
    DEFAULT_MCP_TOOL_TIMEOUT_SECONDS,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionType {
    #[default]
    Plugin,
    HarnessRuntime,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRiskLevel {
    #[default]
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionTrustLevel {
    Official,
    Verified,
    Community,
    #[default]
    Local,
    Untrusted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub extension_type: ExtensionType,
    #[serde(default = "default_true")]
    pub installed: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub risk_level: ExtensionRiskLevel,
    #[serde(default)]
    pub trust_level: ExtensionTrustLevel,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub operations: Vec<String>,
    #[serde(default)]
    pub external_effect: bool,
    #[serde(default)]
    pub requires_preview: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifestValidation {
    pub ok: bool,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub manifest: Option<PluginManifest>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillRiskLevel {
    #[default]
    Low,
    Medium,
    High,
}

impl SkillRiskLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillTrustLevel {
    Official,
    Verified,
    Community,
    #[default]
    Local,
    Untrusted,
}

impl SkillTrustLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Official => "official",
            Self::Verified => "verified",
            Self::Community => "community",
            Self::Local => "local",
            Self::Untrusted => "untrusted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorOperationSummary {
    pub connector_id: String,
    pub operation_id: String,
    #[serde(default)]
    pub risk_level: SkillRiskLevel,
    #[serde(default)]
    pub external_effect: bool,
    #[serde(default)]
    pub requires_preview: bool,
    #[serde(default)]
    pub requires_human_approval: bool,
    #[serde(default)]
    pub descriptor_sha256: Option<String>,
    #[serde(default)]
    pub package_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillPackageManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub category: String,
    #[serde(default)]
    pub risk_level: SkillRiskLevel,
    pub publisher: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub produces: Vec<String>,
    #[serde(default)]
    pub connectors: Vec<String>,
    #[serde(default)]
    pub connector_operations: Vec<ConnectorOperationSummary>,
    #[serde(default)]
    pub trust_level: SkillTrustLevel,
    #[serde(default)]
    pub external_effect: bool,
    #[serde(default)]
    pub requires_preview: bool,
    #[serde(default)]
    pub requires_human_approval: bool,
    #[serde(default)]
    pub trigger_hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub category: String,
    pub risk_level: SkillRiskLevel,
    pub publisher: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub produces: Vec<String>,
    #[serde(default)]
    pub connectors: Vec<String>,
    #[serde(default)]
    pub connector_operations: Vec<ConnectorOperationSummary>,
    pub trust_level: SkillTrustLevel,
    pub enabled: bool,
    pub external_effect: bool,
    #[serde(default)]
    pub when_to_use: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillIndexEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub when_to_use: Vec<String>,
    pub category: String,
    pub risk_level: SkillRiskLevel,
    #[serde(default)]
    pub produces: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub connectors: Vec<String>,
    #[serde(default)]
    pub connector_operations: Vec<ConnectorOperationSummary>,
    pub trust_level: SkillTrustLevel,
    pub enabled: bool,
    pub max_skill_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillIndexPayload {
    pub skills: Vec<SkillIndexEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledSkillsPayload {
    pub skills: Vec<SkillSummary>,
    pub index: SkillIndexPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSkillEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub category: String,
    pub publisher: String,
    pub package_url: String,
    pub manifest_url: Option<String>,
    pub sha256: String,
    pub signature: Option<String>,
    pub risk_level: SkillRiskLevel,
    #[serde(default)]
    pub external_effect: bool,
    #[serde(default)]
    pub requires_connectors: Vec<String>,
    #[serde(default)]
    pub connector_operations: Vec<ConnectorOperationSummary>,
    pub trust_level: SkillTrustLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverSkillEntry {
    #[serde(flatten)]
    pub entry: RemoteSkillEntry,
    pub installed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSkillRegistry {
    pub registry_version: String,
    pub generated_at: String,
    pub skills: Vec<RemoteSkillEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverSkillsPayload {
    pub registry: RemoteSkillRegistry,
    pub skills: Vec<DiscoverSkillEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillUpdateInfo {
    pub skill_id: String,
    pub installed_version: String,
    pub available_version: Option<String>,
    pub update_available: bool,
    pub auto_update_eligible: bool,
    pub pinned_version: Option<String>,
    pub update_policy: String,
    pub reason: Option<String>,
    pub risk_level: SkillRiskLevel,
    pub trust_level: SkillTrustLevel,
    pub external_effect: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillManifestValidation {
    pub ok: bool,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub manifest: Option<SkillSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionManifestSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub extension_type: String,
    pub installed: bool,
    pub enabled: bool,
    pub risk_level: String,
    pub trust_level: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ExtensionError {
    #[error("plugin manifest must be a JSON object")]
    NotObject,
    #[error("unsupported extension_type '{0}'")]
    UnsupportedExtensionType(String),
    #[error("unsupported risk_level '{0}'")]
    UnsupportedRiskLevel(String),
    #[error("unsupported trust_level '{0}'")]
    UnsupportedTrustLevel(String),
}

pub fn validate_plugin_manifest(raw: &Value) -> PluginManifestValidation {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let manifest = match parse_plugin_manifest(raw) {
        Ok(manifest) => manifest,
        Err(error) => {
            return PluginManifestValidation {
                ok: false,
                errors: vec![error.to_string()],
                warnings,
                manifest: None,
            };
        }
    };

    if manifest.id.is_empty() {
        errors.push("id is required".to_owned());
    }
    if manifest.name.is_empty() {
        errors.push("name is required".to_owned());
    }
    if manifest.operations.is_empty() {
        errors.push("at least one operation is required".to_owned());
    }
    if manifest.external_effect && !manifest.requires_preview {
        errors.push("external_effect plugins must require preview".to_owned());
    }
    if manifest.external_effect && manifest.risk_level == ExtensionRiskLevel::Low {
        warnings.push("external_effect plugin is declared low risk".to_owned());
    }

    PluginManifestValidation {
        ok: errors.is_empty(),
        errors,
        warnings,
        manifest: Some(manifest),
    }
}

pub fn validate_skill_manifest(raw: &Value) -> SkillManifestValidation {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let manifest = match parse_skill_manifest(raw) {
        Ok(manifest) => manifest,
        Err(error) => {
            return SkillManifestValidation {
                ok: false,
                errors: vec![error],
                warnings,
                manifest: None,
            };
        }
    };

    validate_skill_manifest_fields(&manifest, &mut errors, &mut warnings);
    SkillManifestValidation {
        ok: errors.is_empty(),
        errors,
        warnings,
        manifest: Some(skill_summary_from_manifest(&manifest, true)),
    }
}

pub fn parse_skill_manifest(raw: &Value) -> Result<SkillPackageManifest, String> {
    let object = raw
        .as_object()
        .ok_or_else(|| "skill manifest must be a JSON object".to_owned())?;
    let manifest = SkillPackageManifest {
        id: string_field(object.get("id")),
        name: string_field(object.get("name")),
        version: string_field(object.get("version")).or_else(|| "0.1.0".to_owned()),
        description: string_field(object.get("description")),
        category: string_field(object.get("category")).or_else(|| "general".to_owned()),
        risk_level: parse_skill_risk_level(object.get("risk_level"))?,
        publisher: string_field(object.get("publisher")).or_else(|| "local".to_owned()),
        requires: string_list_field(object.get("requires")),
        produces: string_list_field(object.get("produces")),
        connectors: string_list_field(object.get("connectors")),
        connector_operations: parse_connector_operations(object.get("connector_operations"))?,
        trust_level: parse_skill_trust_level(object.get("trust_level"))?,
        external_effect: bool_field(object.get("external_effect"), false),
        requires_preview: bool_field(object.get("requires_preview"), false),
        requires_human_approval: bool_field(object.get("requires_human_approval"), false),
        trigger_hints: string_list_field(
            object
                .get("trigger_hints")
                .or_else(|| object.get("when_to_use")),
        ),
    };
    Ok(manifest)
}

pub fn builtin_remote_skill_entries() -> Vec<RemoteSkillEntry> {
    vec![
        RemoteSkillEntry {
            id: "coder.repo-review".to_owned(),
            name: "Repository Review".to_owned(),
            version: "0.1.0".to_owned(),
            description: "Review repository changes using bounded repo evidence.".to_owned(),
            category: "coding".to_owned(),
            publisher: "coder-official".to_owned(),
            package_url: "builtin://skills/coder.repo-review".to_owned(),
            manifest_url: None,
            sha256: "0000000000000000000000000000000000000000000000000000000000000001".to_owned(),
            signature: None,
            risk_level: SkillRiskLevel::Low,
            external_effect: false,
            requires_connectors: Vec::new(),
            connector_operations: Vec::new(),
            trust_level: SkillTrustLevel::Official,
        },
        RemoteSkillEntry {
            id: "coder.patch-workflow".to_owned(),
            name: "Patch Workflow".to_owned(),
            version: "0.1.0".to_owned(),
            description: "Guide patch preview, approval, and verification workflows.".to_owned(),
            category: "coding".to_owned(),
            publisher: "coder-official".to_owned(),
            package_url: "builtin://skills/coder.patch-workflow".to_owned(),
            manifest_url: None,
            sha256: "0000000000000000000000000000000000000000000000000000000000000002".to_owned(),
            signature: None,
            risk_level: SkillRiskLevel::Medium,
            external_effect: true,
            requires_connectors: Vec::new(),
            connector_operations: Vec::new(),
            trust_level: SkillTrustLevel::Official,
        },
    ]
}

pub fn remote_skill_summary(entry: &RemoteSkillEntry, enabled: bool) -> SkillSummary {
    SkillSummary {
        id: entry.id.clone(),
        name: entry.name.clone(),
        version: entry.version.clone(),
        description: entry.description.clone(),
        category: entry.category.clone(),
        risk_level: entry.risk_level,
        publisher: entry.publisher.clone(),
        requires: Vec::new(),
        produces: Vec::new(),
        connectors: entry.requires_connectors.clone(),
        connector_operations: entry.connector_operations.clone(),
        trust_level: entry.trust_level,
        enabled,
        external_effect: entry.external_effect,
        when_to_use: vec![entry.description.clone()],
    }
}

pub fn installed_skills_payload(skills: Vec<SkillSummary>) -> InstalledSkillsPayload {
    let index = SkillIndexPayload {
        skills: skills
            .iter()
            .map(|skill| SkillIndexEntry {
                id: skill.id.clone(),
                name: skill.name.clone(),
                description: skill.description.clone(),
                when_to_use: skill.when_to_use.clone(),
                category: skill.category.clone(),
                risk_level: skill.risk_level,
                produces: skill.produces.clone(),
                requires: skill.requires.clone(),
                connectors: skill.connectors.clone(),
                connector_operations: skill.connector_operations.clone(),
                trust_level: skill.trust_level,
                enabled: skill.enabled,
                max_skill_tokens: 1200,
            })
            .collect(),
    };
    InstalledSkillsPayload { skills, index }
}

pub fn discover_skills_payload(
    registry_url: &str,
    installed_ids: &std::collections::BTreeSet<String>,
) -> DiscoverSkillsPayload {
    let entries = builtin_remote_skill_entries();
    let skills = entries
        .iter()
        .cloned()
        .map(|entry| DiscoverSkillEntry {
            installed: installed_ids.contains(&entry.id),
            entry,
        })
        .collect();
    DiscoverSkillsPayload {
        registry: RemoteSkillRegistry {
            registry_version: if registry_url.trim().is_empty() {
                "builtin".to_owned()
            } else {
                "builtin-projection".to_owned()
            },
            generated_at: "1970-01-01T00:00:00Z".to_owned(),
            skills: entries,
        },
        skills,
    }
}

pub fn extension_search(
    query: &str,
    plugins: &[PluginManifest],
    skills: &[SkillSummary],
) -> Vec<ExtensionManifestSummary> {
    let normalized = query.trim().to_ascii_lowercase();
    let mut candidates = plugins
        .iter()
        .map(extension_summary_from_plugin)
        .chain(skills.iter().map(extension_summary_from_skill))
        .collect::<Vec<_>>();
    if normalized.is_empty() {
        return candidates;
    }
    candidates.retain(|extension| {
        extension.id.to_ascii_lowercase().contains(&normalized)
            || extension.name.to_ascii_lowercase().contains(&normalized)
            || extension
                .description
                .to_ascii_lowercase()
                .contains(&normalized)
            || extension
                .tags
                .iter()
                .any(|tag| tag.to_ascii_lowercase().contains(&normalized))
    });
    candidates
}

pub fn parse_plugin_manifest(raw: &Value) -> Result<PluginManifest, ExtensionError> {
    let object = raw.as_object().ok_or(ExtensionError::NotObject)?;
    let id = string_field(object.get("id"));
    let name = string_field(object.get("name"));
    Ok(PluginManifest {
        id,
        name,
        version: string_field(object.get("version")).or_else(default_version),
        description: string_field(object.get("description")),
        extension_type: parse_extension_type(object.get("extension_type"))?,
        installed: bool_field(object.get("installed"), true),
        enabled: bool_field(object.get("enabled"), true),
        risk_level: parse_risk_level(object.get("risk_level"))?,
        trust_level: parse_trust_level(object.get("trust_level"))?,
        tags: string_list_field(object.get("tags")),
        operations: string_list_field(object.get("operations")),
        external_effect: bool_field(object.get("external_effect"), false),
        requires_preview: bool_field(object.get("requires_preview"), false),
    })
}

fn validate_skill_manifest_fields(
    manifest: &SkillPackageManifest,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    if manifest.id.trim().is_empty() {
        errors.push("id is required".to_owned());
    }
    if !manifest
        .id
        .chars()
        .all(|item| item.is_ascii_alphanumeric() || matches!(item, '_' | '.' | '-'))
    {
        errors.push("skill id must use letters, numbers, dots, underscores, or hyphens".to_owned());
    }
    if manifest.name.trim().is_empty() {
        errors.push("name is required".to_owned());
    }
    if manifest.description.trim().is_empty() {
        errors.push("description is required".to_owned());
    }
    if manifest.external_effect && !manifest.requires_preview {
        errors.push("external_effect skills must require preview".to_owned());
    }
    if manifest.external_effect && !manifest.requires_human_approval {
        errors.push("external_effect skills must require human approval".to_owned());
    }
    let connectors = manifest
        .connectors
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for operation in &manifest.connector_operations {
        if operation.connector_id.trim().is_empty() || operation.operation_id.trim().is_empty() {
            errors.push("connector operations require connector_id and operation_id".to_owned());
        }
        if !connectors.contains(&operation.connector_id) {
            errors.push("connector_operations must reference declared connectors".to_owned());
        }
        if operation.external_effect && !operation.requires_preview {
            errors.push("external-effect connector operations must require preview".to_owned());
        }
        if operation.external_effect && !operation.requires_human_approval {
            errors.push(
                "external-effect connector operations must require human approval".to_owned(),
            );
        }
        if operation.external_effect && !manifest.external_effect {
            errors.push(
                "external-effect connector operations require manifest external_effect=true"
                    .to_owned(),
            );
        }
    }
    if manifest.external_effect && manifest.risk_level == SkillRiskLevel::Low {
        warnings.push("external_effect skill is declared low risk".to_owned());
    }
}

fn skill_summary_from_manifest(manifest: &SkillPackageManifest, enabled: bool) -> SkillSummary {
    SkillSummary {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        description: manifest.description.clone(),
        category: manifest.category.clone(),
        risk_level: manifest.risk_level,
        publisher: manifest.publisher.clone(),
        requires: manifest.requires.clone(),
        produces: manifest.produces.clone(),
        connectors: manifest.connectors.clone(),
        connector_operations: manifest.connector_operations.clone(),
        trust_level: manifest.trust_level,
        enabled,
        external_effect: manifest.external_effect,
        when_to_use: manifest.trigger_hints.clone(),
    }
}

fn extension_summary_from_plugin(plugin: &PluginManifest) -> ExtensionManifestSummary {
    ExtensionManifestSummary {
        id: plugin.id.clone(),
        name: plugin.name.clone(),
        version: plugin.version.clone(),
        description: plugin.description.clone(),
        extension_type: match plugin.extension_type {
            ExtensionType::Plugin => "plugin",
            ExtensionType::HarnessRuntime => "harness_runtime",
        }
        .to_owned(),
        installed: plugin.installed,
        enabled: plugin.enabled,
        risk_level: match plugin.risk_level {
            ExtensionRiskLevel::Low => "low",
            ExtensionRiskLevel::Medium => "medium",
            ExtensionRiskLevel::High => "high",
        }
        .to_owned(),
        trust_level: match plugin.trust_level {
            ExtensionTrustLevel::Official => "official",
            ExtensionTrustLevel::Verified => "verified",
            ExtensionTrustLevel::Community => "community",
            ExtensionTrustLevel::Local => "local",
            ExtensionTrustLevel::Untrusted => "untrusted",
        }
        .to_owned(),
        tags: plugin.tags.clone(),
    }
}

fn extension_summary_from_skill(skill: &SkillSummary) -> ExtensionManifestSummary {
    ExtensionManifestSummary {
        id: skill.id.clone(),
        name: skill.name.clone(),
        version: skill.version.clone(),
        description: skill.description.clone(),
        extension_type: "skill".to_owned(),
        installed: true,
        enabled: skill.enabled,
        risk_level: skill.risk_level.as_str().to_owned(),
        trust_level: skill.trust_level.as_str().to_owned(),
        tags: vec!["skill".to_owned(), skill.category.clone()],
    }
}

fn parse_skill_risk_level(value: Option<&Value>) -> Result<SkillRiskLevel, String> {
    match string_field(value).as_str() {
        "" | "low" => Ok(SkillRiskLevel::Low),
        "medium" => Ok(SkillRiskLevel::Medium),
        "high" => Ok(SkillRiskLevel::High),
        other => Err(format!("unsupported skill risk_level '{other}'")),
    }
}

fn parse_skill_trust_level(value: Option<&Value>) -> Result<SkillTrustLevel, String> {
    match string_field(value).as_str() {
        "" | "local" => Ok(SkillTrustLevel::Local),
        "official" => Ok(SkillTrustLevel::Official),
        "verified" => Ok(SkillTrustLevel::Verified),
        "community" => Ok(SkillTrustLevel::Community),
        "untrusted" => Ok(SkillTrustLevel::Untrusted),
        other => Err(format!("unsupported skill trust_level '{other}'")),
    }
}

fn parse_connector_operations(
    value: Option<&Value>,
) -> Result<Vec<ConnectorOperationSummary>, String> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut operations = Vec::new();
    for item in items {
        let object = item
            .as_object()
            .ok_or_else(|| "connector operation must be a JSON object".to_owned())?;
        operations.push(ConnectorOperationSummary {
            connector_id: string_field(object.get("connector_id")),
            operation_id: string_field(object.get("operation_id")),
            risk_level: parse_skill_risk_level(object.get("risk_level"))?,
            external_effect: bool_field(object.get("external_effect"), false),
            requires_preview: bool_field(object.get("requires_preview"), false),
            requires_human_approval: bool_field(object.get("requires_human_approval"), false),
            descriptor_sha256: string_option_field(object.get("descriptor_sha256")),
            package_sha256: string_option_field(object.get("package_sha256")),
        });
    }
    Ok(operations)
}

pub fn builtin_plugin_manifests() -> Vec<PluginManifest> {
    vec![
        PluginManifest {
            id: "command-runner".to_owned(),
            name: "Command Runner".to_owned(),
            description: "Runs approved local commands through CommandService.".to_owned(),
            operations: vec!["run_check".to_owned(), "sandbox_check".to_owned()],
            external_effect: true,
            requires_preview: true,
            tags: vec!["coding".to_owned(), "checks".to_owned()],
            ..PluginManifest::builtin_default()
        },
        PluginManifest {
            id: "filesystem-patch".to_owned(),
            name: "File Patch Service".to_owned(),
            description:
                "Creates patch previews, applies authorized patches, and rolls back snapshots."
                    .to_owned(),
            operations: vec![
                "patch_preview".to_owned(),
                "apply_patch".to_owned(),
                "rollback_patch".to_owned(),
            ],
            external_effect: true,
            requires_preview: true,
            tags: vec!["coding".to_owned(), "files".to_owned()],
            ..PluginManifest::builtin_default()
        },
    ]
}

impl PluginManifest {
    fn builtin_default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            version: default_version(),
            description: String::new(),
            extension_type: ExtensionType::Plugin,
            installed: true,
            enabled: true,
            risk_level: ExtensionRiskLevel::Low,
            trust_level: ExtensionTrustLevel::Local,
            tags: Vec::new(),
            operations: Vec::new(),
            external_effect: false,
            requires_preview: false,
        }
    }
}

fn parse_extension_type(value: Option<&Value>) -> Result<ExtensionType, ExtensionError> {
    match string_field(value).as_str() {
        "" | "plugin" => Ok(ExtensionType::Plugin),
        "harness_runtime" => Ok(ExtensionType::HarnessRuntime),
        other => Err(ExtensionError::UnsupportedExtensionType(other.to_owned())),
    }
}

fn parse_risk_level(value: Option<&Value>) -> Result<ExtensionRiskLevel, ExtensionError> {
    match string_field(value).as_str() {
        "" | "low" => Ok(ExtensionRiskLevel::Low),
        "medium" => Ok(ExtensionRiskLevel::Medium),
        "high" => Ok(ExtensionRiskLevel::High),
        other => Err(ExtensionError::UnsupportedRiskLevel(other.to_owned())),
    }
}

fn parse_trust_level(value: Option<&Value>) -> Result<ExtensionTrustLevel, ExtensionError> {
    match string_field(value).as_str() {
        "" | "local" => Ok(ExtensionTrustLevel::Local),
        "official" => Ok(ExtensionTrustLevel::Official),
        "verified" => Ok(ExtensionTrustLevel::Verified),
        "community" => Ok(ExtensionTrustLevel::Community),
        "untrusted" => Ok(ExtensionTrustLevel::Untrusted),
        other => Err(ExtensionError::UnsupportedTrustLevel(other.to_owned())),
    }
}

fn string_field(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_owned()
}

fn string_option_field(value: Option<&Value>) -> Option<String> {
    let text = string_field(value);
    (!text.is_empty()).then_some(text)
}

fn string_list_field(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn bool_field(value: Option<&Value>, default: bool) -> bool {
    value.and_then(Value::as_bool).unwrap_or(default)
}

fn default_version() -> String {
    "builtin".to_owned()
}

trait StringExt {
    fn or_else(self, fallback: impl FnOnce() -> String) -> String;
}

impl StringExt for String {
    fn or_else(self, fallback: impl FnOnce() -> String) -> String {
        if self.is_empty() {
            fallback()
        } else {
            self
        }
    }
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn valid_plugin_manifest_accepts_builtin_shape() {
        let validation = validate_plugin_manifest(&json!({
            "id": "filesystem-patch",
            "name": "File Patch Service",
            "operations": ["patch_preview", "apply_patch"],
            "external_effect": true,
            "requires_preview": true,
            "tags": ["coding", "files"]
        }));

        assert!(validation.ok);
        let manifest = validation.manifest.unwrap();
        assert_eq!(manifest.extension_type, ExtensionType::Plugin);
        assert_eq!(manifest.version, "builtin");
        assert!(manifest.installed);
        assert!(manifest.enabled);
    }

    #[test]
    fn external_effect_plugin_requires_preview() {
        let validation = validate_plugin_manifest(&json!({
            "id": "unsafe",
            "name": "Unsafe",
            "operations": ["publish"],
            "external_effect": true,
            "requires_preview": false
        }));

        assert!(!validation.ok);
        assert!(validation
            .errors
            .iter()
            .any(|error| error == "external_effect plugins must require preview"));
    }

    #[test]
    fn invalid_manifest_rejects_unknown_extension_type() {
        let validation = validate_plugin_manifest(&json!({
            "id": "x",
            "name": "x",
            "extension_type": "skill",
            "operations": ["op"]
        }));

        assert!(!validation.ok);
        assert!(validation.errors[0].contains("unsupported extension_type"));
    }

    #[test]
    fn builtin_plugins_have_stable_native_registry_contract() {
        let plugins = builtin_plugin_manifests();
        let ids = plugins
            .iter()
            .map(|plugin| plugin.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let command_runner = plugins
            .iter()
            .find(|plugin| plugin.id == "command-runner")
            .unwrap();
        assert!(ids.contains("command-runner"));
        assert!(ids.contains("filesystem-patch"));
        assert_eq!(ids.len(), 2);
        assert!(command_runner.external_effect);
        assert!(command_runner.requires_preview);
    }

    #[test]
    fn skill_manifest_validation_rejects_external_effect_without_approval_gates() {
        let validation = validate_skill_manifest(&json!({
            "id": "unsafe-skill",
            "name": "Unsafe Skill",
            "version": "0.1.0",
            "description": "Unsafe operation.",
            "category": "coding",
            "publisher": "local",
            "risk_level": "medium",
            "external_effect": true,
            "requires_preview": false,
            "requires_human_approval": false
        }));

        assert!(!validation.ok);
        assert!(validation
            .errors
            .iter()
            .any(|error| error == "external_effect skills must require preview"));
        assert!(validation
            .errors
            .iter()
            .any(|error| error == "external_effect skills must require human approval"));
    }

    #[test]
    fn skill_manifest_validation_accepts_safe_manifest() {
        let validation = validate_skill_manifest(&json!({
            "id": "docs.lookup",
            "name": "Docs Lookup",
            "version": "0.1.0",
            "description": "Find project docs.",
            "category": "knowledge",
            "publisher": "local",
            "risk_level": "low",
            "trust_level": "local",
            "trigger_hints": ["docs"]
        }));

        assert!(validation.ok);
        let manifest = validation.manifest.unwrap();
        assert_eq!(manifest.id, "docs.lookup");
        assert_eq!(manifest.risk_level, SkillRiskLevel::Low);
        assert!(manifest.enabled);
    }

    #[test]
    fn builtin_discover_payload_marks_installed_ids() {
        let installed = std::collections::BTreeSet::from(["coder.repo-review".to_owned()]);
        let payload = discover_skills_payload("builtin://skills", &installed);

        let repo_review = payload
            .skills
            .iter()
            .find(|skill| skill.entry.id == "coder.repo-review")
            .unwrap();
        assert_eq!(payload.registry.registry_version, "builtin-projection");
        assert!(repo_review.installed);
    }

    #[test]
    fn extension_search_includes_installed_skills() {
        let skill = remote_skill_summary(&builtin_remote_skill_entries()[0], true);
        let results = extension_search("review", &builtin_plugin_manifests(), &[skill]);

        assert!(results.iter().any(|item| item.extension_type == "skill"));
        assert!(results.iter().any(|item| item.id == "coder.repo-review"));
    }
}
