use std::sync::Arc;

use coder_config::HarnessSpec;
use coder_core::RunId;
use coder_harness::{HarnessBackend, HarnessError, HarnessRunRequest, HarnessRunResult};
use coder_store::{RunStore, SubagentMetadata};
use serde_json::{json, Value};

use crate::subagent_context::{
    create_subagent_context, SubagentContextInput, SubagentContextTemplateInput,
    SubagentInvocationKind,
};

#[derive(Clone)]
pub struct SubagentRuntime {
    store: RunStore,
}

struct SubagentTerminalGuard {
    store: RunStore,
    run_id: RunId,
    agent_id: String,
    armed: bool,
}

impl SubagentTerminalGuard {
    fn new(store: RunStore, run_id: RunId, agent_id: String) -> Self {
        Self {
            store,
            run_id,
            agent_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SubagentTerminalGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(Some(mut metadata)) = self
            .store
            .read_subagent_metadata(&self.run_id, &self.agent_id)
        else {
            return;
        };
        if metadata
            .status
            .as_deref()
            .is_some_and(subagent_status_is_terminal)
        {
            return;
        }
        let reason = "subagent execution ended before recording a terminal result";
        let Ok(sequence) = self.store.append_subagent_transcript_record_next(
            &self.run_id,
            &self.agent_id,
            None,
            "subagent.lost",
            json!({"status": "lost", "reason": reason}),
        ) else {
            return;
        };
        metadata.status = Some("lost".to_owned());
        metadata.terminal_record_kind = Some("subagent.lost".to_owned());
        metadata.last_sequence = Some(sequence);
        metadata.error = Some(reason.to_owned());
        let _ = self
            .store
            .write_subagent_metadata(&self.run_id, &self.agent_id, &metadata);
    }
}

impl SubagentRuntime {
    pub fn new(store: RunStore) -> Self {
        Self { store }
    }

    pub async fn run(
        &self,
        input: SubagentRunInput<'_>,
    ) -> Result<SubagentRunOutput, HarnessError> {
        let selected_tools = selected_tools_from_backend_context(input.backend_context);
        let context = create_subagent_context(SubagentContextInput {
            template: SubagentContextTemplateInput {
                run_id: input.run_id,
                workflow_id: input.workflow_id,
                node_id: input.node_id,
                parent_agent_id: input.parent_agent_id,
                parent_harness_id: input.parent_harness_id,
                harness: input.harness,
                selected_tools: selected_tools.as_deref(),
            },
            agent_id: input.agent_id.clone(),
            subagent_name: input.subagent_name,
            is_built_in: input.is_built_in,
            invoking_request_id: input.invoking_request_id,
            invocation_kind: input.invocation_kind,
            parent_query_depth: input.parent_query_depth,
        });
        let agent_id = context
            .get("agent_id")
            .and_then(Value::as_str)
            .ok_or_else(|| HarnessError::Failed("subagent context missing agent_id".to_owned()))?
            .to_owned();
        let transcript_ref = format!(
            "subagent://runs/{}/subagents/agent-{agent_id}.jsonl",
            input.run_id.as_str()
        );
        let mut metadata = SubagentMetadata {
            agent_type: "subagent".to_owned(),
            parent_agent_id: input.parent_agent_id.to_owned(),
            parent_harness_id: input.parent_harness_id.to_owned(),
            invocation_kind: input.invocation_kind.as_str().to_owned(),
            status: Some("running".to_owned()),
            terminal_record_kind: None,
            last_sequence: None,
            error: None,
            description: input.subagent_name.map(str::to_owned),
            worktree_path: Some(input.repo_root.to_owned()),
            transcript_ref: Some(transcript_ref.clone()),
        };
        let metadata_ref = self
            .store
            .write_subagent_metadata(input.run_id, &agent_id, &metadata)
            .map_err(|error| HarnessError::Failed(error.to_string()))?;
        let mut terminal_guard =
            SubagentTerminalGuard::new(self.store.clone(), input.run_id.clone(), agent_id.clone());
        let child_backend_context = subagent_backend_context(
            input.backend_context,
            &context,
            &metadata_ref,
            &transcript_ref,
        );
        self.append_record(
            input.run_id,
            &agent_id,
            input.parent_sequence,
            "subagent.started",
            json!({
                "agent_id": agent_id,
                "subagent_name": input.subagent_name,
                "invocation_kind": input.invocation_kind.as_str(),
                "invoking_request_id": input.invoking_request_id,
                "context": context
            }),
        )?;
        self.append_record(
            input.run_id,
            &agent_id,
            None,
            "subagent.user",
            json!({
                "task": input.task,
                "repo_root": input.repo_root
            }),
        )?;

        let request = HarnessRunRequest {
            run_id: input.run_id.clone(),
            workflow_id: input.workflow_id.to_owned(),
            node_id: format!("{}::{}", input.node_id, agent_id),
            agent_id: agent_id.clone(),
            harness_id: input.parent_harness_id.to_owned(),
            repo_root: input.repo_root.to_owned(),
            task: input.task.to_owned(),
            backend_context: child_backend_context,
        };
        let result = match input.backend.run(request).await {
            Ok(result) => result,
            Err(error) => {
                let error_message = error.to_string();
                let terminal_sequence = self.append_record(
                    input.run_id,
                    &agent_id,
                    None,
                    "subagent.failed",
                    json!({
                        "status": "failed",
                        "error": error_message
                    }),
                )?;
                metadata.status = Some("failed".to_owned());
                metadata.terminal_record_kind = Some("subagent.failed".to_owned());
                metadata.last_sequence = Some(terminal_sequence);
                metadata.error = Some(error_message);
                self.store
                    .write_subagent_metadata(input.run_id, &agent_id, &metadata)
                    .map_err(|error| HarnessError::Failed(error.to_string()))?;
                terminal_guard.disarm();
                return Err(error);
            }
        };
        let terminal_sequence = self.record_backend_result(input.run_id, &agent_id, &result)?;
        metadata.status = Some(result.status.clone());
        metadata.terminal_record_kind = Some(terminal_record_kind(&result.status).to_owned());
        metadata.last_sequence = Some(terminal_sequence);
        metadata.error = None;
        self.store
            .write_subagent_metadata(input.run_id, &agent_id, &metadata)
            .map_err(|error| HarnessError::Failed(error.to_string()))?;
        terminal_guard.disarm();

        Ok(SubagentRunOutput {
            agent_id,
            metadata_ref,
            transcript_ref,
            result,
        })
    }

    pub fn record_cancelled(
        &self,
        run_id: &RunId,
        agent_id: &str,
        reason: &str,
    ) -> Result<(), HarnessError> {
        let transcript_ref = format!(
            "subagent://runs/{}/subagents/agent-{agent_id}.jsonl",
            run_id.as_str()
        );
        let mut metadata = self
            .store
            .read_subagent_metadata(run_id, agent_id)
            .map_err(|error| HarnessError::Failed(error.to_string()))?
            .unwrap_or_else(|| SubagentMetadata {
                agent_type: "subagent".to_owned(),
                parent_agent_id: "unknown".to_owned(),
                parent_harness_id: "unknown".to_owned(),
                invocation_kind: SubagentInvocationKind::Spawn.as_str().to_owned(),
                status: Some("running".to_owned()),
                terminal_record_kind: None,
                last_sequence: None,
                error: None,
                description: None,
                worktree_path: None,
                transcript_ref: Some(transcript_ref.clone()),
            });
        if matches!(
            metadata.status.as_deref(),
            Some("completed" | "blocked" | "failed" | "cancelled" | "canceled")
        ) {
            return Ok(());
        }
        let terminal_sequence = self.append_record(
            run_id,
            agent_id,
            None,
            "subagent.cancelled",
            json!({
                "status": "cancelled",
                "reason": reason
            }),
        )?;
        metadata.status = Some("cancelled".to_owned());
        metadata.terminal_record_kind = Some("subagent.cancelled".to_owned());
        metadata.last_sequence = Some(terminal_sequence);
        metadata.error = Some(reason.to_owned());
        metadata.transcript_ref = Some(transcript_ref);
        self.store
            .write_subagent_metadata(run_id, agent_id, &metadata)
            .map_err(|error| HarnessError::Failed(error.to_string()))?;
        Ok(())
    }

    pub fn record_lost(
        &self,
        run_id: &RunId,
        agent_id: &str,
        reason: &str,
    ) -> Result<(), HarnessError> {
        let transcript_ref = format!(
            "subagent://runs/{}/subagents/agent-{agent_id}.jsonl",
            run_id.as_str()
        );
        let mut metadata = self
            .store
            .read_subagent_metadata(run_id, agent_id)
            .map_err(|error| HarnessError::Failed(error.to_string()))?
            .unwrap_or_else(|| SubagentMetadata {
                agent_type: "subagent".to_owned(),
                parent_agent_id: "unknown".to_owned(),
                parent_harness_id: "unknown".to_owned(),
                invocation_kind: SubagentInvocationKind::Spawn.as_str().to_owned(),
                status: Some("running".to_owned()),
                terminal_record_kind: None,
                last_sequence: None,
                error: None,
                description: None,
                worktree_path: None,
                transcript_ref: Some(transcript_ref.clone()),
            });
        if matches!(
            metadata.status.as_deref(),
            Some("completed" | "blocked" | "failed" | "cancelled" | "canceled" | "lost")
        ) {
            return Ok(());
        }
        let terminal_sequence = self.append_record(
            run_id,
            agent_id,
            None,
            "subagent.lost",
            json!({
                "status": "lost",
                "reason": reason
            }),
        )?;
        metadata.status = Some("lost".to_owned());
        metadata.terminal_record_kind = Some("subagent.lost".to_owned());
        metadata.last_sequence = Some(terminal_sequence);
        metadata.error = Some(reason.to_owned());
        metadata.transcript_ref = Some(transcript_ref);
        self.store
            .write_subagent_metadata(run_id, agent_id, &metadata)
            .map_err(|error| HarnessError::Failed(error.to_string()))?;
        Ok(())
    }

    fn record_backend_result(
        &self,
        run_id: &RunId,
        agent_id: &str,
        result: &HarnessRunResult,
    ) -> Result<u64, HarnessError> {
        for event in &result.events {
            self.append_record(
                run_id,
                agent_id,
                None,
                "subagent.event",
                json!({
                    "kind": event.kind,
                    "payload": event.payload,
                    "refs": event.refs
                }),
            )?;
        }
        if let Some(report) = &result.report {
            self.append_record(
                run_id,
                agent_id,
                None,
                "subagent.report",
                json!({ "report": report }),
            )?;
        }
        let terminal_sequence = self.append_record(
            run_id,
            agent_id,
            None,
            terminal_record_kind(&result.status),
            json!({ "status": result.status }),
        )?;
        Ok(terminal_sequence)
    }

    fn append_record(
        &self,
        run_id: &RunId,
        agent_id: &str,
        parent_sequence: Option<u64>,
        kind: &'static str,
        payload: Value,
    ) -> Result<u64, HarnessError> {
        self.store
            .append_subagent_transcript_record_next(
                run_id,
                agent_id,
                parent_sequence,
                kind,
                payload,
            )
            .map_err(|error| HarnessError::Failed(error.to_string()))
    }
}

fn selected_tools_from_backend_context(parent_backend_context: &Value) -> Option<Vec<String>> {
    parent_backend_context
        .pointer("/coder/harness/selected_tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|tools| !tools.is_empty())
}

fn subagent_backend_context(
    parent_backend_context: &Value,
    context: &Value,
    metadata_ref: &str,
    transcript_ref: &str,
) -> Value {
    let inherited_tools = context
        .pointer("/tools/inherited")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let subagent = json!({
        "context": context,
        "metadata_ref": metadata_ref,
        "transcript_ref": transcript_ref
    });
    let mut coder = parent_backend_context
        .get("coder")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !coder.is_object() {
        coder = json!({});
    }
    if !coder.get("harness").is_some_and(Value::is_object) {
        coder["harness"] = json!({});
    }
    coder["harness"]["selected_tools"] = inherited_tools.clone();
    coder["subagent"] = subagent;
    json!({ "coder": coder })
}

pub struct SubagentRunInput<'a> {
    pub backend: Arc<dyn HarnessBackend>,
    pub run_id: &'a RunId,
    pub workflow_id: &'a str,
    pub node_id: &'a str,
    pub parent_agent_id: &'a str,
    pub parent_harness_id: &'a str,
    pub harness: &'a HarnessSpec,
    pub repo_root: &'a str,
    pub task: &'a str,
    pub backend_context: &'a Value,
    pub agent_id: Option<String>,
    pub subagent_name: Option<&'a str>,
    pub is_built_in: bool,
    pub invoking_request_id: Option<&'a str>,
    pub invocation_kind: SubagentInvocationKind,
    pub parent_query_depth: u32,
    pub parent_sequence: Option<u64>,
}

pub struct SubagentRunOutput {
    pub agent_id: String,
    pub metadata_ref: String,
    pub transcript_ref: String,
    pub result: HarnessRunResult,
}

fn terminal_record_kind(status: &str) -> &'static str {
    match status {
        "completed" | "finish" => "subagent.completed",
        "blocked" => "subagent.blocked",
        "failed" => "subagent.failed",
        "cancelled" | "canceled" => "subagent.cancelled",
        "lost" => "subagent.lost",
        _ => "subagent.stopped",
    }
}

fn subagent_status_is_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed" | "finish" | "blocked" | "failed" | "cancelled" | "canceled" | "lost"
    )
}

#[cfg(test)]
mod context_tests {
    use super::*;

    #[test]
    fn child_backend_context_keeps_one_parent_projection() {
        let parent = json!({
            "coder": {
                "plan_context": {"marker": "single-copy-marker"},
                "harness": {"selected_tools": ["repo_read_file", "agent_subagent"]}
            }
        });
        let context = json!({
            "agent_id": "agent-1",
            "tools": {"inherited": ["repo_read_file"]}
        });

        let child = subagent_backend_context(
            &parent,
            &context,
            "subagent://metadata",
            "subagent://transcript",
        );
        let serialized = child.to_string();

        assert_eq!(serialized.matches("single-copy-marker").count(), 1);
        assert!(child.get("parent_backend_context").is_none());
        assert!(child.get("coder_subagent").is_none());
        assert_eq!(
            child["coder"]["harness"]["selected_tools"],
            json!(["repo_read_file"])
        );
    }
}
