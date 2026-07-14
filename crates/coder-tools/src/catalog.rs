use serde_json::{json, Value};

pub const MODEL_MAX_FILE_EDITS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPermission {
    None,
    ReadFiles,
    WriteFiles,
    RunCommands,
    ChildHarnessPermissions,
}

impl ToolPermission {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ReadFiles => "read_files",
            Self::WriteFiles => "write_files",
            Self::RunCommands => "run_commands",
            Self::ChildHarnessPermissions => "child_harness_permissions",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolConcurrencyClass {
    Concurrent,
    Exclusive,
}

#[derive(Clone, Copy)]
pub struct BuiltinToolDefinition {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub permission: ToolPermission,
    pub concurrency: ToolConcurrencyClass,
    model_spec: Option<fn() -> Value>,
}

impl std::fmt::Debug for BuiltinToolDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BuiltinToolDefinition")
            .field("name", &self.name)
            .field("aliases", &self.aliases)
            .field("permission", &self.permission)
            .field("concurrency", &self.concurrency)
            .field("model_visible", &self.model_spec.is_some())
            .finish()
    }
}

impl BuiltinToolDefinition {
    pub fn model_spec(self) -> Option<Value> {
        self.model_spec.map(|build| build())
    }

    pub fn matches(self, name: &str) -> bool {
        self.name == name || self.aliases.contains(&name)
    }
}

macro_rules! tool {
    ($name:literal, [$($alias:literal),* $(,)?], $permission:ident, $concurrency:ident, $spec:expr) => {
        BuiltinToolDefinition {
            name: $name,
            aliases: &[$($alias),*],
            permission: ToolPermission::$permission,
            concurrency: ToolConcurrencyClass::$concurrency,
            model_spec: $spec,
        }
    };
}

static BUILTIN_TOOLS: &[BuiltinToolDefinition] = &[
    tool!(
        "repo_find_files",
        ["find_files", "repo_files", "search_files"],
        ReadFiles,
        Concurrent,
        Some(repo_find_files_spec)
    ),
    tool!(
        "repo_search_text",
        ["repo_search", "search_text"],
        ReadFiles,
        Concurrent,
        Some(repo_search_text_spec)
    ),
    tool!(
        "repo_read_file",
        ["read_file"],
        ReadFiles,
        Concurrent,
        Some(repo_read_file_spec)
    ),
    tool!(
        "repo_read_file_range",
        ["read_file_range"],
        ReadFiles,
        Concurrent,
        Some(repo_read_file_range_spec)
    ),
    tool!(
        "git_status",
        [],
        ReadFiles,
        Concurrent,
        Some(git_status_spec)
    ),
    tool!(
        "git_diff",
        ["inspect_git_diff"],
        ReadFiles,
        Concurrent,
        Some(git_diff_spec)
    ),
    tool!(
        "command_preview",
        ["preview_command"],
        None,
        Concurrent,
        None
    ),
    tool!(
        "command_run",
        ["run_command", "run_command_sandbox"],
        RunCommands,
        Exclusive,
        Some(command_run_spec)
    ),
    tool!(
        "command_background",
        ["bash_background"],
        RunCommands,
        Exclusive,
        Some(command_background_spec)
    ),
    tool!(
        "read_command_output",
        [],
        ReadFiles,
        Concurrent,
        Some(read_command_output_spec)
    ),
    tool!(
        "write_stdin",
        [],
        RunCommands,
        Exclusive,
        Some(write_stdin_spec)
    ),
    tool!(
        "cancel_command_background",
        [],
        RunCommands,
        Exclusive,
        Some(cancel_command_background_spec)
    ),
    tool!(
        "patch_preview",
        ["preview_patch", "propose_patch"],
        WriteFiles,
        Exclusive,
        None
    ),
    tool!(
        "patch_file_apply",
        ["apply_patch_sandbox"],
        WriteFiles,
        Exclusive,
        None
    ),
    tool!(
        "apply_patch",
        ["patch_apply"],
        WriteFiles,
        Exclusive,
        Some(apply_patch_spec)
    ),
    tool!(
        "agent_subagent",
        ["Agent", "agent", "Task", "task", "subagent"],
        ChildHarnessPermissions,
        Exclusive,
        Some(agent_subagent_spec)
    ),
    tool!(
        "read_subagent_status",
        [],
        ReadFiles,
        Concurrent,
        Some(read_subagent_status_spec)
    ),
    tool!(
        "cancel_subagent_background",
        [],
        ChildHarnessPermissions,
        Exclusive,
        Some(cancel_subagent_background_spec)
    ),
    tool!(
        "skill",
        ["Skill", "SkillTool", "skill_tool"],
        ReadFiles,
        Exclusive,
        Some(skill_spec)
    ),
    tool!(
        "sleep",
        ["Sleep", "sleep_tool", "SleepTool"],
        None,
        Concurrent,
        None
    ),
    tool!(
        "edit_text_file",
        ["edit_file"],
        WriteFiles,
        Exclusive,
        Some(edit_text_file_spec)
    ),
    tool!(
        "write_text_file",
        ["write_file", "file_write"],
        WriteFiles,
        Exclusive,
        Some(write_text_file_spec)
    ),
    tool!(
        "finish",
        ["final", "final_report"],
        None,
        Exclusive,
        Some(finish_spec)
    ),
];

pub fn builtin_tools() -> &'static [BuiltinToolDefinition] {
    BUILTIN_TOOLS
}

pub fn builtin_tool(name: &str) -> Option<&'static BuiltinToolDefinition> {
    BUILTIN_TOOLS.iter().find(|tool| tool.matches(name))
}

pub fn canonical_builtin_tool_name(name: &str) -> Option<&'static str> {
    builtin_tool(name).map(|tool| tool.name)
}

fn function_spec(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters
        }
    })
}

fn repo_find_files_spec() -> Value {
    function_spec(
        "repo_find_files",
        "List repo files. Omit query to list all files; query is a literal case-insensitive path substring, not a glob. Use extensions for suffix filtering.",
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Optional literal path substring. Omit it to list all files; glob characters have no special meaning."},
                "extensions": {"type": "array", "items": {"type": "string"}, "description": "Optional file extensions such as [\"rs\", \"md\"]."},
                "max_results": {"type": "integer", "minimum": 1, "maximum": 200}
            },
            "additionalProperties": false
        }),
    )
}

fn repo_search_text_spec() -> Value {
    function_spec(
        "repo_search_text",
        "Search bounded repository text.",
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "max_matches": {"type": "integer", "minimum": 1, "maximum": 50}
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    )
}

fn repo_read_file_spec() -> Value {
    function_spec(
        "repo_read_file",
        "Read a bounded UTF-8 repo file.",
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "max_file_bytes": {"type": "integer", "minimum": 1, "maximum": 65536}
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    )
}

fn repo_read_file_range_spec() -> Value {
    function_spec(
        "repo_read_file_range",
        "Read a bounded line range from a repo file.",
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "start_line": {"type": "integer", "minimum": 1},
                "max_lines": {"type": "integer", "minimum": 1, "maximum": 200},
                "max_chars": {"type": "integer", "minimum": 1, "maximum": 100000}
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    )
}

fn git_status_spec() -> Value {
    function_spec(
        "git_status",
        "Read bounded git status.",
        json!({"type": "object", "properties": {}, "additionalProperties": false}),
    )
}

fn git_diff_spec() -> Value {
    function_spec(
        "git_diff",
        "Read bounded git diff.",
        json!({
            "type": "object",
            "properties": {
                "max_output_bytes": {"type": "integer", "minimum": 1, "maximum": 65536}
            },
            "additionalProperties": false
        }),
    )
}

fn command_run_spec() -> Value {
    function_spec(
        "command_run",
        "Run a bounded command in the repo after permission and approval checks. Long commands are automatically backgrounded on timeout.",
        command_parameters(true),
    )
}

fn command_background_spec() -> Value {
    function_spec(
        "command_background",
        "Start a bounded background command in the repo after permission and approval checks.",
        command_parameters(false),
    )
}

fn command_parameters(include_foreground_timeout: bool) -> Value {
    let mut properties = serde_json::Map::from_iter([
        (
            "argv".to_owned(),
            json!({"type": "array", "items": {"type": "string"}, "minItems": 1}),
        ),
        ("cwd".to_owned(), json!({"type": "string"})),
        (
            "timeout_seconds".to_owned(),
            json!({"type": "integer", "minimum": 1, "maximum": 600}),
        ),
        (
            "max_output_bytes".to_owned(),
            json!({"type": "integer", "minimum": 1, "maximum": 1048576}),
        ),
        ("interactive".to_owned(), json!({"type": "boolean"})),
    ]);
    if include_foreground_timeout {
        properties.insert(
            "foreground_timeout_seconds".to_owned(),
            json!({"type": "integer", "minimum": 1, "maximum": 600}),
        );
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": ["argv"],
        "additionalProperties": false
    })
}

fn task_status_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "task_id": {"type": "string"},
            "timeout": {"type": "integer", "minimum": 0, "maximum": 600000, "default": 30000},
            "block": {"type": "boolean", "default": false}
        },
        "required": ["task_id"],
        "additionalProperties": false
    })
}

fn task_cancel_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {"task_id": {"type": "string"}},
        "required": ["task_id"],
        "additionalProperties": false
    })
}

fn read_command_output_spec() -> Value {
    function_spec(
        "read_command_output",
        "Read or wait for a background command task.",
        command_status_parameters(),
    )
}

fn write_stdin_spec() -> Value {
    function_spec(
        "write_stdin",
        "Write input to an interactive command process, optionally close stdin, then read or wait for new output.",
        json!({
            "type": "object",
            "properties": {
                "task_id": {"type": "string"},
                "input": {"type": "string", "default": ""},
                "close_stdin": {"type": "boolean", "default": false},
                "cursor": {"type": "integer", "minimum": 0},
                "timeout": {"type": "integer", "minimum": 0, "maximum": 300000, "default": 5000},
                "block": {"type": "boolean", "default": true}
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
    )
}

fn command_status_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "task_id": {"type": "string"},
            "cursor": {"type": "integer", "minimum": 0},
            "timeout": {"type": "integer", "minimum": 0, "maximum": 300000, "default": 5000},
            "block": {"type": "boolean", "default": true}
        },
        "required": ["task_id"],
        "additionalProperties": false
    })
}

fn cancel_command_background_spec() -> Value {
    function_spec(
        "cancel_command_background",
        "Cancel a background command task.",
        task_cancel_parameters(),
    )
}

fn apply_patch_spec() -> Value {
    function_spec(
        "apply_patch",
        "Use apply_patch to edit files atomically. Send one Codex patch in the patch field; do not edit files through command tools or create a temporary patch file.",
        json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "string",
                    "description": format!("Patch must match this Lark grammar:\n{}\nFor Add File, every content line, including empty lines, starts with '+'. The final line must be exactly '*** End Patch'.", crate::APPLY_PATCH_LARK_GRAMMAR)
                },
                "max_patch_bytes": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": crate::DEFAULT_MAX_PATCH_BYTES
                }
            },
            "required": ["patch"],
            "additionalProperties": false
        }),
    )
}

fn agent_subagent_spec() -> Value {
    function_spec(
        "agent_subagent",
        "Run a scoped child agent. It is synchronous unless run_in_background=true; a synchronous result is final and needs no status lookup.",
        json!({
            "type": "object",
            "properties": {
                "task": {"type": "string"},
                "subagent_type": {"type": "string"},
                "subagent_name": {"type": "string"},
                "run_in_background": {"type": "boolean"}
            },
            "required": ["task"],
            "additionalProperties": false
        }),
    )
}

fn read_subagent_status_spec() -> Value {
    function_spec(
        "read_subagent_status",
        "Read or wait for a background subagent only. Use background_task.task_id returned by agent_subagent, never agent_id.",
        task_status_parameters(),
    )
}

fn cancel_subagent_background_spec() -> Value {
    function_spec(
        "cancel_subagent_background",
        "Cancel a background subagent task.",
        task_cancel_parameters(),
    )
}

fn skill_spec() -> Value {
    function_spec(
        "skill",
        "Invoke an installed or built-in skill through the shared skill tool runtime.",
        json!({
            "type": "object",
            "properties": {
                "skill": {"type": "string"},
                "name": {"type": "string"},
                "command": {"type": "string"}
            },
            "additionalProperties": false
        }),
    )
}

fn edit_text_file_spec() -> Value {
    function_spec(
        "edit_text_file",
        "Replace exact strings in one existing repo-relative UTF-8 file. Provide old_string/new_string for one edit or edits for multiple sequential atomic edits. Each old_string must be unique unless replace_all=true.",
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "old_string": {"type": "string", "minLength": 1},
                "new_string": {"type": "string"},
                "replace_all": {"type": "boolean"},
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MODEL_MAX_FILE_EDITS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": {"type": "string", "minLength": 1},
                            "new_string": {"type": "string"},
                            "replace_all": {"type": "boolean"}
                        },
                        "required": ["old_string", "new_string"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    )
}

fn write_text_file_spec() -> Value {
    function_spec(
        "write_text_file",
        "Write full UTF-8 text content to a new repo-relative file or deliberately replace a whole file.",
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }),
    )
}

fn finish_spec() -> Value {
    function_spec(
        "finish",
        "Finish after tool work is complete or blocked.",
        json!({
            "type": "object",
            "properties": {
                "status": {"type": "string", "enum": ["completed", "blocked"]},
                "summary": {"type": "string"},
                "checks": {"type": "array", "items": {"type": "string"}},
                "blockers": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["status", "summary"],
            "additionalProperties": false
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_to_one_canonical_definition() {
        assert_eq!(
            canonical_builtin_tool_name("read_file"),
            Some("repo_read_file")
        );
        assert_eq!(
            canonical_builtin_tool_name("apply_patch_sandbox"),
            Some("patch_file_apply")
        );
        assert_eq!(
            canonical_builtin_tool_name("patch_apply"),
            Some("apply_patch")
        );
        assert_eq!(canonical_builtin_tool_name("final_report"), Some("finish"));
        assert_eq!(canonical_builtin_tool_name("unknown"), None);
    }

    #[test]
    fn model_specs_use_the_canonical_name() {
        for tool in builtin_tools().iter().copied() {
            let Some(spec) = tool.model_spec() else {
                continue;
            };
            assert_eq!(
                spec.pointer("/function/name").and_then(Value::as_str),
                Some(tool.name)
            );
        }
    }
}
