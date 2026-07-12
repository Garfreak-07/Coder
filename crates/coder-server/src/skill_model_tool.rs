use coder_extensions::{builtin_remote_skill_entries, RemoteSkillEntry};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path as FsPath, PathBuf},
};

use crate::{ApiError, ApiState};

const LOCAL_SKILL_MAX_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct LoadedModelSkill {
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) skill_path: String,
    pub(crate) base_dir: Option<String>,
    pub(crate) content: String,
    pub(crate) origin: &'static str,
    pub(crate) frontmatter: Value,
    pub(crate) execution_policy: SkillExecutionPolicy,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SkillExecutionPolicy {
    pub(crate) disable_model_invocation: bool,
    pub(crate) allowed_tools: Vec<String>,
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<Value>,
    pub(crate) context: String,
    pub(crate) agent: Option<String>,
    pub(crate) user_invocable: bool,
    pub(crate) unsupported_frontmatter_fields: Vec<String>,
    pub(crate) ignored_trust_boundary_fields: Vec<String>,
}

impl Default for SkillExecutionPolicy {
    fn default() -> Self {
        Self {
            disable_model_invocation: false,
            allowed_tools: Vec::new(),
            model: None,
            effort: None,
            context: "inline".to_owned(),
            agent: None,
            user_invocable: true,
            unsupported_frontmatter_fields: Vec::new(),
            ignored_trust_boundary_fields: Vec::new(),
        }
    }
}

pub(crate) fn load_model_skill(
    state: &ApiState,
    skill_name: &str,
    session_id: &str,
    tool_use_id: &str,
) -> Result<Option<LoadedModelSkill>, ApiError> {
    if let Some(skill) = find_local_skill_for_model_tool(state, skill_name, session_id)? {
        return Ok(Some(skill));
    }
    match find_installed_skill_for_model_tool(state, skill_name)? {
        InstalledSkillLookup::Found(skill) => return Ok(Some(*skill)),
        InstalledSkillLookup::Disabled => return Ok(None),
        InstalledSkillLookup::Missing => {}
    }
    Ok(
        find_builtin_skill_for_model_tool(skill_name).map(|entry| LoadedModelSkill {
            id: entry.id.clone(),
            display_name: entry.name.clone(),
            skill_path: entry.package_url.clone(),
            base_dir: Some(entry.package_url.clone()),
            content: builtin_skill_model_content(&entry),
            origin: "builtin",
            frontmatter: json!({
                "id": entry.id,
                "name": entry.name,
                "version": entry.version,
                "description": entry.description,
                "category": entry.category,
                "publisher": entry.publisher,
                "risk_level": entry.risk_level.as_str(),
                "trust_level": entry.trust_level.as_str(),
                "external_effect": entry.external_effect,
                "tool_use_id": tool_use_id
            }),
            execution_policy: SkillExecutionPolicy::default(),
        }),
    )
}

enum InstalledSkillLookup {
    Found(Box<LoadedModelSkill>),
    Disabled,
    Missing,
}

fn find_installed_skill_for_model_tool(
    state: &ApiState,
    skill_name: &str,
) -> Result<InstalledSkillLookup, ApiError> {
    let lookup = normalize_skill_lookup_key(skill_name);
    let installed = state
        .installed_skills
        .lock()
        .map_err(|_| ApiError::internal("installed skills lock poisoned"))?;
    let Some(record) = installed.values().find(|record| {
        skill_lookup_matches(&record.summary.id, &lookup)
            || skill_lookup_matches(&record.summary.name, &lookup)
    }) else {
        return Ok(InstalledSkillLookup::Missing);
    };
    if !record.summary.enabled {
        return Ok(InstalledSkillLookup::Disabled);
    }
    let skill_path = record
        .source_url
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("installed://skills/{}", record.summary.id));
    Ok(InstalledSkillLookup::Found(Box::new(LoadedModelSkill {
        id: record.summary.id.clone(),
        display_name: record.summary.name.clone(),
        skill_path: skill_path.clone(),
        base_dir: Some(skill_path.clone()),
        content: installed_skill_model_content(&record.summary, &skill_path),
        origin: "installed_skill",
        frontmatter: json!({
            "id": record.summary.id,
            "name": record.summary.name,
            "version": record.summary.version,
            "description": record.summary.description,
            "category": record.summary.category,
            "publisher": record.summary.publisher,
            "risk_level": record.summary.risk_level.as_str(),
            "trust_level": record.summary.trust_level.as_str(),
            "external_effect": record.summary.external_effect,
            "source_url": record.source_url,
            "source": "installed_skill_summary_projection"
        }),
        execution_policy: SkillExecutionPolicy {
            ignored_trust_boundary_fields: vec![
                "hooks".to_owned(),
                "mcpServers".to_owned(),
                "permissionMode".to_owned(),
            ],
            ..SkillExecutionPolicy::default()
        },
    })))
}

fn find_local_skill_for_model_tool(
    state: &ApiState,
    skill_name: &str,
    session_id: &str,
) -> Result<Option<LoadedModelSkill>, ApiError> {
    let roots = state
        .skill_extra_roots
        .lock()
        .map_err(|_| ApiError::internal("skill extra roots lock poisoned"))?
        .clone();
    for root in roots
        .iter()
        .filter(|root| root.enabled && !root.path.trim().is_empty())
    {
        let root_path = PathBuf::from(root.path.trim());
        for skill_file in local_skill_file_candidates(&root_path) {
            if let Some(skill) = load_local_skill_file_for_model_tool(
                &skill_file,
                skill_name,
                session_id,
                root.scope.as_str(),
            )? {
                return Ok(Some(skill));
            }
        }
    }
    Ok(None)
}

fn local_skill_file_candidates(root: &FsPath) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let direct = root.join("SKILL.md");
    if direct.is_file() {
        candidates.push(direct);
    }

    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let candidate = entry.path().join("SKILL.md");
            if candidate.is_file() {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

fn load_local_skill_file_for_model_tool(
    skill_file: &FsPath,
    requested_skill: &str,
    session_id: &str,
    scope: &str,
) -> Result<Option<LoadedModelSkill>, ApiError> {
    let lookup = normalize_skill_lookup_key(requested_skill);
    let skill_dir = skill_file.parent().unwrap_or_else(|| FsPath::new(""));
    let dir_name = skill_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("skill")
        .to_owned();
    let dir_matches = skill_lookup_matches(&dir_name, &lookup);
    let metadata = match fs::metadata(skill_file) {
        Ok(metadata) => metadata,
        Err(_) if dir_matches => {
            return Err(ApiError::bad_request(format!(
                "Skill file is not readable: {}",
                normalize_skill_path_for_model(skill_file)
            )));
        }
        Err(_) => return Ok(None),
    };
    if metadata.len() > LOCAL_SKILL_MAX_BYTES {
        if dir_matches {
            return Err(ApiError::bad_request(format!(
                "Skill file exceeds {} bytes: {}",
                LOCAL_SKILL_MAX_BYTES,
                normalize_skill_path_for_model(skill_file)
            )));
        }
        return Ok(None);
    }

    let raw = fs::read(skill_file)
        .map_err(|error| ApiError::bad_request(format!("Failed to read skill file: {error}")))?;
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (frontmatter, body) = parse_skill_frontmatter(&raw);
    let display_name = yaml_string_field(&frontmatter, &["name", "displayName", "display_name"])
        .unwrap_or_else(|| dir_name.clone());
    let id = slugify_skill_lookup_key(
        &yaml_string_field(&frontmatter, &["id", "skill", "skill_name"])
            .unwrap_or_else(|| dir_name.clone()),
    );
    let matches_requested = dir_matches
        || skill_lookup_matches(&id, &lookup)
        || skill_lookup_matches(&display_name, &lookup);
    if !matches_requested {
        return Ok(None);
    }

    let base_dir = normalize_skill_path_for_model(skill_dir);
    let content = finalize_local_skill_content(&body, &base_dir, session_id);
    let execution_policy = local_skill_execution_policy(&frontmatter);
    let frontmatter = local_skill_frontmatter_json(frontmatter, scope);
    Ok(Some(LoadedModelSkill {
        id,
        display_name,
        skill_path: normalize_skill_path_for_model(skill_file),
        base_dir: Some(base_dir),
        content,
        origin: "local_extra_root",
        frontmatter,
        execution_policy,
    }))
}

fn skill_lookup_matches(candidate: &str, lookup: &str) -> bool {
    normalize_skill_lookup_key(candidate) == lookup || slugify_skill_lookup_key(candidate) == lookup
}

fn parse_skill_frontmatter(raw: &str) -> (BTreeMap<String, serde_yaml::Value>, String) {
    let mut lines = raw.split_inclusive('\n');
    let Some(first_line) = lines.next() else {
        return (BTreeMap::new(), String::new());
    };
    if !is_frontmatter_fence(first_line) {
        return (BTreeMap::new(), raw.to_owned());
    }

    let mut yaml = String::new();
    let mut consumed = first_line.len();
    for line in lines {
        consumed += line.len();
        if is_frontmatter_fence(line) {
            let body = raw.get(consumed..).unwrap_or("").to_owned();
            return (parse_skill_frontmatter_yaml(&yaml), body);
        }
        yaml.push_str(line);
    }

    (BTreeMap::new(), raw.to_owned())
}

fn is_frontmatter_fence(line: &str) -> bool {
    line.trim_matches(|character| character == '\r' || character == '\n')
        .trim()
        == "---"
}

fn parse_skill_frontmatter_yaml(yaml: &str) -> BTreeMap<String, serde_yaml::Value> {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(yaml) else {
        return BTreeMap::new();
    };
    let serde_yaml::Value::Mapping(mapping) = value else {
        return BTreeMap::new();
    };
    mapping
        .into_iter()
        .filter_map(|(key, value)| key.as_str().map(|key| (key.to_owned(), value)))
        .collect()
}

fn yaml_string_field(
    frontmatter: &BTreeMap<String, serde_yaml::Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| {
            frontmatter.get(*key).and_then(|value| match value {
                serde_yaml::Value::String(value) => Some(value.trim().to_owned()),
                serde_yaml::Value::Number(value) => Some(value.to_string()),
                serde_yaml::Value::Bool(value) => Some(value.to_string()),
                _ => None,
            })
        })
        .filter(|value| !value.is_empty())
}

fn local_skill_frontmatter_json(
    frontmatter: BTreeMap<String, serde_yaml::Value>,
    scope: &str,
) -> Value {
    let mut value = serde_json::to_value(frontmatter).unwrap_or_else(|_| json!({}));
    if let Value::Object(object) = &mut value {
        object.insert("scope".to_owned(), json!(scope));
    }
    value
}

fn local_skill_execution_policy(
    frontmatter: &BTreeMap<String, serde_yaml::Value>,
) -> SkillExecutionPolicy {
    let model = yaml_string_field(frontmatter, &["model"])
        .filter(|model| !model.eq_ignore_ascii_case("inherit"));
    let context = yaml_string_field(frontmatter, &["context"])
        .filter(|context| context == "fork")
        .unwrap_or_else(|| "inline".to_owned());
    SkillExecutionPolicy {
        disable_model_invocation: yaml_bool_field(frontmatter, "disable-model-invocation"),
        allowed_tools: yaml_string_list_field(frontmatter, "allowed-tools"),
        model,
        effort: yaml_effort_field(frontmatter, "effort"),
        context,
        agent: yaml_string_field(frontmatter, &["agent"]),
        user_invocable: frontmatter
            .get("user-invocable")
            .map(|_| yaml_bool_field(frontmatter, "user-invocable"))
            .unwrap_or(true),
        unsupported_frontmatter_fields: present_frontmatter_fields(frontmatter, &["hooks"]),
        ignored_trust_boundary_fields: present_frontmatter_fields(
            frontmatter,
            &["mcpServers", "permissionMode"],
        ),
    }
}

fn present_frontmatter_fields(
    frontmatter: &BTreeMap<String, serde_yaml::Value>,
    keys: &[&str],
) -> Vec<String> {
    keys.iter()
        .filter(|key| frontmatter.contains_key(**key))
        .map(|key| (*key).to_owned())
        .collect()
}

fn yaml_bool_field(frontmatter: &BTreeMap<String, serde_yaml::Value>, key: &str) -> bool {
    match frontmatter.get(key) {
        Some(serde_yaml::Value::Bool(value)) => *value,
        Some(serde_yaml::Value::String(value)) => value.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn yaml_string_list_field(
    frontmatter: &BTreeMap<String, serde_yaml::Value>,
    key: &str,
) -> Vec<String> {
    match frontmatter.get(key) {
        Some(serde_yaml::Value::Sequence(items)) => items
            .iter()
            .filter_map(yaml_scalar_string)
            .flat_map(|value| split_frontmatter_list(&value))
            .collect(),
        Some(value) => yaml_scalar_string(value)
            .map(|value| split_frontmatter_list(&value))
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

fn yaml_effort_field(
    frontmatter: &BTreeMap<String, serde_yaml::Value>,
    key: &str,
) -> Option<Value> {
    match frontmatter.get(key) {
        Some(serde_yaml::Value::Number(number)) => Some(json!(number.to_string())),
        Some(serde_yaml::Value::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(json!(trimmed))
            }
        }
        Some(serde_yaml::Value::Bool(value)) => Some(json!(value.to_string())),
        _ => None,
    }
}

fn yaml_scalar_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(value) => Some(value.trim().to_owned()),
        serde_yaml::Value::Number(value) => Some(value.to_string()),
        serde_yaml::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
    .filter(|value| !value.is_empty())
}

fn split_frontmatter_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn finalize_local_skill_content(body: &str, base_dir: &str, session_id: &str) -> String {
    format!("Base directory for this skill: {base_dir}\n\n{body}")
        .replace("${CLAUDE_SKILL_DIR}", base_dir)
        .replace("${CLAUDE_SESSION_ID}", session_id)
}

fn normalize_skill_path_for_model(path: &FsPath) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
        .replace('\\', "/")
}

pub(crate) fn model_tool_skill_name(input: &Value) -> Option<String> {
    model_tool_string(
        input,
        &[
            "skill",
            "skill_name",
            "skillName",
            "command",
            "command_name",
            "commandName",
            "name",
        ],
    )
    .map(|value| value.trim().trim_start_matches('/').trim().to_owned())
    .filter(|value| !value.is_empty())
}

fn model_tool_string(input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        input
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn find_builtin_skill_for_model_tool(skill_name: &str) -> Option<RemoteSkillEntry> {
    let lookup = normalize_skill_lookup_key(skill_name);
    builtin_remote_skill_entries().into_iter().find(|entry| {
        normalize_skill_lookup_key(&entry.id) == lookup
            || normalize_skill_lookup_key(&entry.name) == lookup
            || slugify_skill_lookup_key(&entry.name) == lookup
    })
}

fn normalize_skill_lookup_key(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('/')
        .trim()
        .to_ascii_lowercase()
}

fn slugify_skill_lookup_key(value: &str) -> String {
    normalize_skill_lookup_key(value)
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '.' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn builtin_skill_model_content(entry: &RemoteSkillEntry) -> String {
    format!(
        "# {}\n\n\
Base directory for this skill: {}\n\n\
Id: {}\n\
Version: {}\n\
Category: {}\n\
Publisher: {}\n\
Risk: {}\n\
Trust: {}\n\
External effect: {}\n\n\
{}\n\n\
This Coder builtin skill is loaded through the model-facing Skill tool. \
Record this invocation so compaction can restore the skill context.",
        entry.name,
        entry.package_url,
        entry.id,
        entry.version,
        entry.category,
        entry.publisher,
        entry.risk_level.as_str(),
        entry.trust_level.as_str(),
        entry.external_effect,
        entry.description
    )
}

fn installed_skill_model_content(
    skill: &coder_extensions::SkillSummary,
    skill_path: &str,
) -> String {
    format!(
        "# {}\n\n\
Base directory for this skill: {}\n\n\
Id: {}\n\
Version: {}\n\
Category: {}\n\
Publisher: {}\n\
Risk: {}\n\
Trust: {}\n\
External effect: {}\n\
Requires: {}\n\
Produces: {}\n\
Connectors: {}\n\n\
{}\n\n\
This installed skill is represented by Coder's installed skill summary. \
Package hooks, MCP servers, and permission mode fields are not executed from \
this summary projection; Coder records this invocation so compaction can \
restore the skill context.",
        skill.name,
        skill_path,
        skill.id,
        skill.version,
        skill.category,
        skill.publisher,
        skill.risk_level.as_str(),
        skill.trust_level.as_str(),
        skill.external_effect,
        skill.requires.join(", "),
        skill.produces.join(", "),
        skill.connectors.join(", "),
        skill.description
    )
}
