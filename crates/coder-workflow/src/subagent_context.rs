use coder_config::HarnessSpec;
use coder_core::RunId;
use serde_json::{json, Value};

pub const SUBAGENT_DISALLOWED_INHERITED_TOOLS: &[&str] = &[
    "agent_subagent",
    "agent",
    "task",
    "memory_read",
    "knowledge_retrieve",
    "search_workflow_memory",
    "search_project_memory",
    "inspect_memory",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentInvocationKind {
    Spawn,
    Resume,
}

impl SubagentInvocationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Resume => "resume",
        }
    }
}

pub struct SubagentContextTemplateInput<'a> {
    pub run_id: &'a RunId,
    pub workflow_id: &'a str,
    pub node_id: &'a str,
    pub parent_agent_id: &'a str,
    pub parent_harness_id: &'a str,
    pub harness: &'a HarnessSpec,
    pub selected_tools: Option<&'a [String]>,
}

pub struct SubagentContextInput<'a> {
    pub template: SubagentContextTemplateInput<'a>,
    pub agent_id: Option<String>,
    pub subagent_name: Option<&'a str>,
    pub is_built_in: bool,
    pub invoking_request_id: Option<&'a str>,
    pub invocation_kind: SubagentInvocationKind,
    pub parent_query_depth: u32,
}

pub fn create_subagent_context(input: SubagentContextInput<'_>) -> Value {
    let agent_id = input.agent_id.unwrap_or_else(|| {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        format!("a{}", &suffix[..16])
    });
    let parent_query_depth = input.parent_query_depth;
    let selected_tools = input
        .template
        .selected_tools
        .unwrap_or(&input.template.harness.tools);
    json!({
        "contract": "coder.subagent_context.v1",
        "agent_id": agent_id,
        "agent_type": "subagent",
        "subagent_name": input.subagent_name,
        "is_built_in": input.is_built_in,
        "parent": {
            "run_id": input.template.run_id.as_str(),
            "workflow_id": input.template.workflow_id,
            "node_id": input.template.node_id,
            "agent_id": input.template.parent_agent_id,
            "harness_id": input.template.parent_harness_id
        },
        "invocation": {
            "invoking_request_id": input.invoking_request_id,
            "kind": input.invocation_kind.as_str(),
            "emitted": false
        },
        "query_tracking": {
            "parent_depth": parent_query_depth,
            "depth": parent_query_depth.saturating_add(1)
        },
        "tools": {
            "inherited": subagent_inheritable_tools(selected_tools)
        }
    })
}

pub fn subagent_inheritable_tools(tools: &[String]) -> Vec<String> {
    tools
        .iter()
        .filter(|tool| !SUBAGENT_DISALLOWED_INHERITED_TOOLS.contains(&tool.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use coder_config::{MemoryAccess, PermissionPolicy};

    use super::*;

    #[test]
    fn subagent_context_keeps_only_runtime_fields_and_filtered_tools() {
        let harness = HarnessSpec {
            backend: "native-rust".to_owned(),
            tools: vec![
                "memory_read".to_owned(),
                "command_run".to_owned(),
                "knowledge_retrieve".to_owned(),
                "agent_subagent".to_owned(),
                "patch_apply".to_owned(),
            ],
            permissions: PermissionPolicy::default(),
            memory: MemoryAccess::default(),
            verification: Default::default(),
        };
        let context = create_subagent_context(SubagentContextInput {
            template: SubagentContextTemplateInput {
                run_id: &RunId::from_string("run-subagent"),
                workflow_id: "planner-led",
                node_id: "executor",
                parent_agent_id: "executor",
                parent_harness_id: "native-code-edit",
                harness: &harness,
                selected_tools: None,
            },
            agent_id: Some("acode-reviewer-0123456789abcdef".to_owned()),
            subagent_name: Some("code-reviewer"),
            is_built_in: true,
            invoking_request_id: Some("request-1"),
            invocation_kind: SubagentInvocationKind::Spawn,
            parent_query_depth: 2,
        });

        assert_eq!(context["agent_id"], "acode-reviewer-0123456789abcdef");
        assert_eq!(context["parent"]["run_id"], "run-subagent");
        assert_eq!(context["invocation"]["kind"], "spawn");
        assert_eq!(context["invocation"]["emitted"], false);
        assert_eq!(context["query_tracking"]["depth"], 3);
        let inherited = context["tools"]["inherited"].as_array().unwrap();
        assert!(!inherited.iter().any(|tool| tool == "memory_read"));
        assert!(!inherited.iter().any(|tool| tool == "knowledge_retrieve"));
        assert!(!inherited.iter().any(|tool| tool == "agent_subagent"));
        assert!(inherited.iter().any(|tool| tool == "command_run"));
        assert!(inherited.iter().any(|tool| tool == "patch_apply"));
        assert!(context.get("isolation").is_none());
        assert!(context.get("permission_recheck").is_none());
        assert!(context.get("sidechain_storage").is_none());
        assert!(context["tools"].get("disallowed").is_none());
    }

    #[test]
    fn subagent_context_records_resume_invocation_boundary() {
        let harness = HarnessSpec {
            backend: "native-rust".to_owned(),
            tools: vec!["terminal".to_owned()],
            permissions: PermissionPolicy::default(),
            memory: MemoryAccess::default(),
            verification: Default::default(),
        };

        let context = create_subagent_context(SubagentContextInput {
            template: SubagentContextTemplateInput {
                run_id: &RunId::from_string("run-subagent"),
                workflow_id: "planner-led",
                node_id: "executor",
                parent_agent_id: "executor",
                parent_harness_id: "native",
                harness: &harness,
                selected_tools: None,
            },
            agent_id: Some("aresume-0123456789abcdef".to_owned()),
            subagent_name: Some("worker"),
            is_built_in: false,
            invoking_request_id: Some("request-resume"),
            invocation_kind: SubagentInvocationKind::Resume,
            parent_query_depth: 0,
        });

        assert_eq!(context["invocation"]["kind"], "resume");
        assert_eq!(
            context["invocation"]["invoking_request_id"],
            "request-resume"
        );
        assert_eq!(context["query_tracking"]["depth"], 1);
    }
}
