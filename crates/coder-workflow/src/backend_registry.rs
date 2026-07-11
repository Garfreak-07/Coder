use std::sync::Arc;

use async_trait::async_trait;
use coder_config::ProjectConfig;
use coder_core::FinalReport;
use coder_harness::{
    HarnessBackend, HarnessError, HarnessRunEvent, HarnessRunRequest, HarnessRunResult,
};
use coder_store::RunStore;
use serde_json::{json, Value};

use crate::{
    workflow_control::workflow_planner_result, BrowserVerifierBackend, NativeMockBackend,
    NativeRustBackend,
};

#[derive(Clone)]
pub struct BackendRegistry {
    planner_model: Arc<dyn HarnessBackend>,
    native_rust: Arc<dyn HarnessBackend>,
    native_mock: Arc<dyn HarnessBackend>,
    browser_verifier: Arc<dyn HarnessBackend>,
}

impl BackendRegistry {
    pub fn native_only() -> Self {
        Self {
            planner_model: Arc::new(PlannerModelBackend),
            native_rust: Arc::new(NativeMockBackend::default()),
            native_mock: Arc::new(NativeMockBackend::default()),
            browser_verifier: Arc::new(BrowserVerifierBackend::default()),
        }
    }

    pub fn from_project_config(_config: &ProjectConfig, store: RunStore) -> Self {
        Self {
            planner_model: Arc::new(PlannerModelBackend),
            native_rust: Arc::new(NativeRustBackend::new(store.clone())),
            native_mock: Arc::new(NativeMockBackend::default()),
            browser_verifier: Arc::new(BrowserVerifierBackend::new(store.clone())),
        }
    }

    pub fn with_native_backend(mut self, backend: Arc<dyn HarnessBackend>) -> Self {
        self.native_rust = backend;
        self
    }

    pub fn with_planner_backend(mut self, backend: Arc<dyn HarnessBackend>) -> Self {
        self.planner_model = backend;
        self
    }

    pub fn with_browser_verifier_backend(mut self, backend: Arc<dyn HarnessBackend>) -> Self {
        self.browser_verifier = backend;
        self
    }

    pub fn backend_for(&self, backend: &str) -> Option<Arc<dyn HarnessBackend>> {
        match backend {
            "planner-model" => Some(Arc::clone(&self.planner_model)),
            "native-rust" => Some(Arc::clone(&self.native_rust)),
            "native_mock" | "mock" => Some(Arc::clone(&self.native_mock)),
            "browser-verifier" => Some(Arc::clone(&self.browser_verifier)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PlannerModelBackend;

#[async_trait]
impl HarnessBackend for PlannerModelBackend {
    async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        if request
            .backend_context
            .pointer("/coder/agent/output_contract")
            .and_then(Value::as_str)
            == Some("workflow_decision")
        {
            return Ok(workflow_planner_result(request));
        }
        let plan_goal = request
            .backend_context
            .pointer("/coder/plan_context/plan_draft/goal")
            .and_then(Value::as_str)
            .or_else(|| {
                request
                    .backend_context
                    .pointer("/coder/plan_context/original_user_request")
                    .and_then(Value::as_str)
            })
            .unwrap_or("Confirmed workflow plan");
        let mut report = FinalReport::completed(
            "Planner Conversation Harness accepted the confirmed plan without side effects.",
        );
        report.checks = vec![
            "planner-model harness: read-only boundary enforced".to_owned(),
            format!("plan_context: {plan_goal}"),
        ];
        Ok(HarnessRunResult {
            status: "ready".to_owned(),
            report: Some(report),
            events: vec![
                HarnessRunEvent::new(
                    "planner.message.completed",
                    json!({
                        "backend": "planner-model",
                        "node_id": request.node_id,
                        "agent_id": request.agent_id,
                        "harness_id": request.harness_id,
                        "side_effects": "none"
                    }),
                ),
                HarnessRunEvent::new(
                    "planner.plan.updated",
                    json!({
                        "backend": "planner-model",
                        "plan_context_summary": plan_goal
                    }),
                ),
                HarnessRunEvent::new(
                    "planner.readiness.changed",
                    json!({
                        "backend": "planner-model",
                        "readiness": "ready"
                    }),
                ),
            ],
        })
    }
}
