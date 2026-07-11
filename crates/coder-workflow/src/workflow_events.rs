use coder_config::WorkflowNodeSpec;
use coder_core::RunId;
use coder_events::CoderEvent;
use coder_harness::HarnessRunEvent;
use serde_json::{json, Value};

use crate::{WorkflowError, WorkflowRunner};

pub(super) struct BackendEventContext<'a> {
    pub(super) run_id: &'a RunId,
    pub(super) sequence: &'a mut u64,
    pub(super) round: u32,
    pub(super) node: &'a WorkflowNodeSpec,
}

pub(super) struct BackendSelectionEvent<'a> {
    pub(super) backend: &'a str,
    pub(super) requested_backend: &'a str,
    pub(super) fallback_for: Option<&'a str>,
    pub(super) fallback_allowed: bool,
}

pub(super) struct BackendBlockedEvent<'a> {
    pub(super) backend: &'a str,
    pub(super) reason: &'a str,
    pub(super) fallback_allowed: bool,
}

pub(super) struct NodeOutcomeEvent<'a> {
    pub(super) round: u32,
    pub(super) node: &'a WorkflowNodeSpec,
    pub(super) kind: &'a str,
    pub(super) status: &'a str,
    pub(super) reason: Option<&'a str>,
}

impl WorkflowRunner {
    pub(super) fn emit(
        &self,
        run_id: &RunId,
        sequence: &mut u64,
        kind: &str,
        payload: Value,
    ) -> Result<(), WorkflowError> {
        let event = CoderEvent::new(run_id.clone(), *sequence, kind, payload);
        self.store.append_event(run_id, &event)?;
        *sequence += 1;
        Ok(())
    }

    pub(super) fn emit_harness_event(
        &self,
        run_id: &RunId,
        sequence: &mut u64,
        backend_event: HarnessRunEvent,
    ) -> Result<(), WorkflowError> {
        let mut event = CoderEvent::new(
            run_id.clone(),
            *sequence,
            backend_event.kind,
            backend_event.payload,
        );
        for reference in backend_event.refs {
            event = event.with_ref(reference.label, reference.uri);
        }
        self.store.append_event(run_id, &event)?;
        *sequence += 1;
        Ok(())
    }

    pub(super) fn emit_backend_selected(
        &self,
        context: BackendEventContext<'_>,
        selection: BackendSelectionEvent<'_>,
    ) -> Result<(), WorkflowError> {
        let mut payload = json!({
            "round": context.round,
            "node_id": context.node.id,
            "agent_id": context.node.agent,
            "harness_id": context.node.harness,
            "backend": selection.backend,
            "requested_backend": selection.requested_backend,
            "status": "selected",
            "fallback_allowed": selection.fallback_allowed,
            "summary": backend_selection_summary(selection.backend, selection.fallback_for)
        });
        if let Some(fallback_for) = selection.fallback_for {
            payload["fallback_for"] = json!(fallback_for);
        }
        self.emit(
            context.run_id,
            context.sequence,
            "backend.selected",
            payload,
        )
    }

    pub(super) fn emit_backend_blocked(
        &self,
        context: BackendEventContext<'_>,
        blocked: BackendBlockedEvent<'_>,
    ) -> Result<(), WorkflowError> {
        self.emit(
            context.run_id,
            context.sequence,
            "backend.blocked",
            json!({
                "round": context.round,
                "node_id": context.node.id,
                "agent_id": context.node.agent,
                "harness_id": context.node.harness,
                "backend": blocked.backend,
                "status": "blocked",
                "fallback_allowed": blocked.fallback_allowed,
                "reason": blocked.reason,
                "summary": backend_blocked_summary(
                    blocked.backend,
                    blocked.reason,
                    blocked.fallback_allowed
                )
            }),
        )
    }

    pub(super) fn emit_node_outcome(
        &self,
        run_id: &RunId,
        sequence: &mut u64,
        outcome: NodeOutcomeEvent<'_>,
    ) -> Result<(), WorkflowError> {
        let mut payload = json!({
            "round": outcome.round,
            "node_id": outcome.node.id,
            "status": outcome.status
        });
        if let Some(reason) = outcome.reason {
            payload["reason"] = json!(reason);
        }
        self.emit(run_id, sequence, outcome.kind, payload)
    }
}

fn backend_selection_summary(backend: &str, fallback_for: Option<&str>) -> String {
    if fallback_for.is_some() {
        return "Executor backend: native fallback".to_owned();
    }
    format!("Executor backend: {}", backend_display_name(backend))
}

fn backend_blocked_summary(backend: &str, reason: &str, fallback_allowed: bool) -> String {
    let suffix = if fallback_allowed {
        " Native fallback is enabled."
    } else {
        ""
    };
    format!(
        "Executor backend: blocked - {}. {}{}",
        backend_display_name(backend),
        reason,
        suffix
    )
}

fn backend_display_name(backend: &str) -> &'static str {
    match backend {
        "native-rust" | "native_mock" | "mock" => "Native",
        "planner-model" => "Planner",
        "browser-verifier" => "Browser verifier",
        _ => "unknown",
    }
}
