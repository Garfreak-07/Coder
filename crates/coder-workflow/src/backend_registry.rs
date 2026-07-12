use std::sync::Arc;

use async_trait::async_trait;
use coder_core::FinalReport;
use coder_harness::{
    HarnessBackend, HarnessError, HarnessRunEvent, HarnessRunRequest, HarnessRunResult,
};
#[cfg(test)]
use coder_store::RunStore;
use serde_json::{json, Value};

#[cfg(test)]
use crate::DeterministicNativeBackend;
use crate::{workflow_control::workflow_planner_result, NativeMockBackend};

#[derive(Clone)]
pub struct BackendRegistry {
    planner_model: Arc<dyn HarnessBackend>,
    native_rust: Arc<dyn HarnessBackend>,
    native_mock: Arc<dyn HarnessBackend>,
}

impl BackendRegistry {
    pub fn for_host() -> Self {
        Self {
            planner_model: Arc::new(UnavailableHostBackend("planner-model")),
            native_rust: Arc::new(UnavailableHostBackend("native-rust")),
            native_mock: Arc::new(NativeMockBackend::default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_deterministic_tests(store: RunStore) -> Self {
        Self {
            planner_model: Arc::new(PlannerModelBackend),
            native_rust: Arc::new(DeterministicNativeBackend::new(store)),
            native_mock: Arc::new(NativeMockBackend::default()),
        }
    }

    pub fn native_only() -> Self {
        Self {
            planner_model: Arc::new(PlannerModelBackend),
            native_rust: Arc::new(NativeMockBackend::default()),
            native_mock: Arc::new(NativeMockBackend::default()),
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

    pub fn backend_for(&self, backend: &str) -> Option<Arc<dyn HarnessBackend>> {
        match backend {
            "planner-model" => Some(Arc::clone(&self.planner_model)),
            "native-rust" => Some(Arc::clone(&self.native_rust)),
            "native_mock" | "mock" => Some(Arc::clone(&self.native_mock)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct UnavailableHostBackend(&'static str);

#[async_trait]
impl HarnessBackend for UnavailableHostBackend {
    async fn run(&self, _request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        Err(HarnessError::Failed(format!(
            "backend '{}' must be injected by the runtime host",
            self.0
        )))
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
        let plan_goal = request.task.as_str();
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
