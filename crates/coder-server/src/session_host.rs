use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use coder_core::RunId;
use coder_events::{OutputEnvelope, OutputEvent, OutputPriority};
use coder_workflow::WorkflowRunControl;
use tokio::sync::{broadcast, watch};

use crate::api_types::{
    ConversationSessionCreateRequest, ConversationSessionResponse, ConversationTurnControlResponse,
    ConversationTurnRequest, ConversationTurnResponse,
};
use crate::capability_registry::{CapabilityKind, CapabilityRegistry};
use crate::code_task_runtime::CodeTaskRuntime;
use crate::conversation_runtime::ConversationRuntime;
use crate::output_hub::OutputHub;
use crate::{ApiError, ApiState, TaskRunRequest, TaskRunResponse};

#[derive(Debug, Clone, Copy)]
struct TaskTokenBudget {
    limit: u64,
    used: u64,
}

#[derive(Debug)]
struct TaskHandle {
    control: Option<watch::Sender<WorkflowRunControl>>,
    token_budget: Option<TaskTokenBudget>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SessionHost {
    conversations: ConversationRuntime,
    capabilities: CapabilityRegistry,
    code_tasks: CodeTaskRuntime,
    output_hub: OutputHub,
    tasks: Arc<Mutex<BTreeMap<String, TaskHandle>>>,
}

impl SessionHost {
    pub(crate) fn create_conversation(
        &self,
        state: &ApiState,
        request: ConversationSessionCreateRequest,
    ) -> Result<ConversationSessionResponse, ApiError> {
        let (response, evicted_session_ids) = self.conversations.create_session(state, request)?;
        for session_id in evicted_session_ids {
            self.output_hub.remove_session(&session_id);
        }
        self.output_hub
            .register_session(&response.session.session_id);
        self.publish_output(
            &response.session.session_id,
            None,
            "conversation",
            OutputPriority::Normal,
            OutputEvent::SessionStarted,
        );
        Ok(response)
    }

    pub(crate) fn get_conversation(
        &self,
        state: &ApiState,
        session_id: &str,
    ) -> Result<ConversationSessionResponse, ApiError> {
        let (response, evicted_session_ids) = self.conversations.get_session(state, session_id)?;
        for evicted_session_id in evicted_session_ids {
            self.output_hub.remove_session(&evicted_session_id);
        }
        self.output_hub.register_session(session_id);
        Ok(response)
    }

    pub(crate) async fn conversation_turn(
        &self,
        state: &ApiState,
        session_id: &str,
        request: ConversationTurnRequest,
    ) -> Result<ConversationTurnResponse, ApiError> {
        self.conversations.turn(state, session_id, request).await
    }

    pub(crate) fn interrupt_conversation_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<ConversationTurnControlResponse, ApiError> {
        self.conversations.interrupt(session_id, turn_id)
    }

    pub(crate) fn steer_conversation_turn(
        &self,
        session_id: &str,
        turn_id: &str,
        request: crate::ConversationSteerRequest,
    ) -> Result<ConversationTurnControlResponse, ApiError> {
        self.conversations.steer(session_id, turn_id, request)
    }

    pub(crate) async fn run_task(
        &self,
        state: ApiState,
        request: TaskRunRequest,
    ) -> Result<TaskRunResponse, ApiError> {
        match self
            .capabilities
            .resolve_task_profile(&request.config, &request.workflow_id)
        {
            Some(CapabilityKind::Code) => self.code_tasks.run(state, request).await,
            None => Err(ApiError::bad_request(format!(
                "task profile '{}' is not registered",
                request.workflow_id
            ))),
        }
    }

    pub(crate) fn register_task(
        &self,
        run_id: &RunId,
        control: watch::Sender<WorkflowRunControl>,
    ) -> Result<(), String> {
        let mut tasks = self
            .tasks
            .lock()
            .map_err(|_| "session host task lock poisoned".to_owned())?;
        let key = run_id.to_string();
        if tasks.get(&key).is_some_and(|task| task.control.is_some()) {
            return Err(format!("task '{}' is already active", run_id.as_str()));
        }
        let token_budget = tasks.get(&key).and_then(|task| task.token_budget);
        tasks.insert(
            key,
            TaskHandle {
                control: Some(control),
                token_budget,
            },
        );
        Ok(())
    }

    pub(crate) fn task_is_active(&self, run_id: &RunId) -> bool {
        self.tasks
            .lock()
            .ok()
            .and_then(|tasks| {
                tasks
                    .get(run_id.as_str())
                    .map(|task| task.control.is_some())
            })
            .unwrap_or(false)
    }

    pub(crate) fn signal_task(&self, run_id: &RunId, control: WorkflowRunControl) -> bool {
        self.tasks
            .lock()
            .ok()
            .and_then(|tasks| {
                tasks
                    .get(run_id.as_str())
                    .and_then(|task| task.control.as_ref())
                    .map(|sender| sender.send(control).is_ok())
            })
            .unwrap_or(false)
    }

    pub(crate) fn deactivate_task(&self, run_id: &RunId) {
        if let Ok(mut tasks) = self.tasks.lock() {
            if let Some(task) = tasks.get_mut(run_id.as_str()) {
                task.control = None;
                if task.token_budget.is_none() {
                    tasks.remove(run_id.as_str());
                }
            }
        }
    }

    pub(crate) fn initialize_token_budget(&self, run_id: &RunId, limit: u64) {
        if limit == 0 {
            return;
        }
        if let Ok(mut tasks) = self.tasks.lock() {
            let task = tasks.entry(run_id.to_string()).or_insert(TaskHandle {
                control: None,
                token_budget: None,
            });
            match task.token_budget.as_mut() {
                Some(budget) => budget.limit = budget.limit.min(limit),
                None => task.token_budget = Some(TaskTokenBudget { limit, used: 0 }),
            }
        }
    }

    pub(crate) fn token_budget(&self, run_id: &RunId) -> Option<(u64, u64)> {
        self.tasks
            .lock()
            .ok()?
            .get(run_id.as_str())?
            .token_budget
            .map(|budget| (budget.limit, budget.used))
    }

    pub(crate) fn charge_tokens(&self, run_id: &RunId, charge: u64) -> Option<(u64, u64)> {
        let mut tasks = self.tasks.lock().ok()?;
        let budget = tasks.get_mut(run_id.as_str())?.token_budget.as_mut()?;
        budget.used = budget.used.saturating_add(charge);
        Some((budget.limit, budget.used))
    }

    pub(crate) fn clear_token_budget(&self, run_id: &RunId) {
        if let Ok(mut tasks) = self.tasks.lock() {
            if let Some(task) = tasks.get_mut(run_id.as_str()) {
                task.token_budget = None;
                if task.control.is_none() {
                    tasks.remove(run_id.as_str());
                }
            }
        }
    }

    pub(crate) fn capability_ids(&self) -> Vec<&str> {
        self.capabilities.ids()
    }

    pub(crate) fn subscribe_output(
        &self,
        session_id: &str,
    ) -> Option<broadcast::Receiver<OutputEnvelope>> {
        self.output_hub.subscribe(session_id)
    }

    pub(crate) fn publish_output(
        &self,
        session_id: &str,
        turn_id: Option<String>,
        source: impl Into<String>,
        priority: OutputPriority,
        output: OutputEvent,
    ) -> Option<OutputEnvelope> {
        self.output_hub
            .publish(session_id, turn_id, source, priority, output)
    }
}
