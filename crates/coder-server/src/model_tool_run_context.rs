use coder_core::RunId;
use coder_store::{DurableJsonlPageOptions, RunStore};
use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub(crate) struct ModelToolRunContext {
    pub workflow_id: Option<String>,
    pub node_id: Option<String>,
    pub agent_id: Option<String>,
    pub harness_id: Option<String>,
    pub repo_root: Option<String>,
    pub plan_context: Option<Value>,
}

pub(crate) fn latest_run_context(store: &RunStore, run_id: &str) -> Option<ModelToolRunContext> {
    let run_id = RunId::from_string(run_id.to_owned());
    let page = store
        .read_events_page(&run_id, DurableJsonlPageOptions::tail(1000).ok()?)
        .ok()?;
    let mut context = ModelToolRunContext::default();
    for event in &page.records {
        if event.kind == "run.started" {
            context.workflow_id = context.workflow_id.or_else(|| {
                event
                    .payload
                    .get("workflow_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            context.repo_root = context.repo_root.or_else(|| {
                event
                    .payload
                    .get("repo_root")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            context.plan_context = context
                .plan_context
                .or_else(|| event.payload.get("plan_context").cloned());
        }
        if event.kind == "node.started" {
            context.node_id = event
                .payload
                .get("node_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            context.agent_id = event
                .payload
                .get("agent")
                .or_else(|| event.payload.get("agent_id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            context.harness_id = event
                .payload
                .get("harness")
                .or_else(|| event.payload.get("harness_id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
    }
    Some(context)
}
