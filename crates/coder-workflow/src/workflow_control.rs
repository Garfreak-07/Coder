use coder_config::WorkflowNodeSpec;
use coder_core::{FinalReport, RunStatus};
use coder_harness::{HarnessRunEvent, HarnessRunRequest, HarnessRunResult};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkflowSignal {
    Ready,
    Completed,
    Blocked,
    Failed,
    Cancelled,
    Continue,
    Finish,
}

impl WorkflowSignal {
    pub(crate) fn from_status(status: &str) -> Option<Self> {
        match status {
            "ready" => Some(Self::Ready),
            "completed" => Some(Self::Completed),
            "blocked" => Some(Self::Blocked),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "continue" => Some(Self::Continue),
            "finish" => Some(Self::Finish),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Continue => "continue",
            Self::Finish => "finish",
        }
    }

    pub(crate) fn node_event_kind(self) -> &'static str {
        match self {
            Self::Ready | Self::Completed | Self::Continue | Self::Finish => "node.completed",
            Self::Blocked => "node.blocked",
            Self::Failed => "node.failed",
            Self::Cancelled => "node.cancelled",
        }
    }

    pub(crate) fn terminal_status(self) -> Option<RunStatus> {
        match self {
            Self::Completed | Self::Finish => Some(RunStatus::Completed),
            Self::Blocked => Some(RunStatus::Blocked),
            Self::Failed => Some(RunStatus::Failed),
            Self::Cancelled => Some(RunStatus::Cancelled),
            Self::Ready | Self::Continue => None,
        }
    }
}

pub(crate) fn workflow_feedback_value(
    source_node: &WorkflowNodeSpec,
    signal: WorkflowSignal,
    checks: &[String],
    blockers: &[String],
) -> Value {
    json!({
        "source_node_id": source_node.id,
        "source_agent_id": source_node.agent,
        "source_harness_id": source_node.harness,
        "signal": signal.as_str(),
        "loop_contract": {
            "lifecycle": ["diagnose", "plan", "act", "verify", "recover_or_finish"],
            "required_decision": workflow_required_decision(signal),
            "finish_requires_executor_evidence": true,
            "blocked_requires_external_dependency": true,
            "repair_when_feedback_is_actionable": true
        },
        "evidence_policy": {
            "checks_present": !checks.is_empty(),
            "blockers_present": !blockers.is_empty(),
            "checks_limit": 12,
            "blockers_limit": 12
        },
        "checks": checks.iter().take(12).cloned().collect::<Vec<_>>(),
        "blockers": blockers.iter().take(12).cloned().collect::<Vec<_>>()
    })
}

fn workflow_required_decision(signal: WorkflowSignal) -> &'static str {
    match signal {
        WorkflowSignal::Completed => "finish_or_verify",
        WorkflowSignal::Failed | WorkflowSignal::Blocked => "continue_or_blocked",
        WorkflowSignal::Continue => "continue",
        WorkflowSignal::Ready => "start_execution",
        WorkflowSignal::Finish => "finish",
        WorkflowSignal::Cancelled => "blocked",
    }
}

pub(crate) fn workflow_planner_task_from_feedback(
    original_task: &str,
    source_node: &WorkflowNodeSpec,
    signal: WorkflowSignal,
    checks: &[String],
    blockers: &[String],
) -> String {
    let feedback = if blockers.is_empty() {
        concise_join(checks, 1000)
    } else {
        concise_join(blockers, 1000)
    };
    format!(
        "Decide the next workflow control signal after node '{}' returned '{}'. Return finish only when the executor's action and verification evidence prove the task is done. Return continue when repair is needed. Return blocked only when user input or external state is required.\n\nPrevious feedback:\n{}\n\nOriginal task:\n{}",
        source_node.id,
        signal.as_str(),
        feedback,
        original_task
    )
}

pub(crate) fn repair_task_from_feedback(
    original_task: &str,
    source_node: &WorkflowNodeSpec,
    signal: WorkflowSignal,
    checks: &[String],
    blockers: &[String],
) -> String {
    let feedback = if blockers.is_empty() {
        concise_join(checks, 800)
    } else {
        concise_join(blockers, 800)
    };
    format!(
        "Continue the same task and repair the implementation based on the previous {} result from node '{}'. The listed feedback is the scope of this round: address it and only directly related checks. Do not restart broad planning or review, rewrite unrelated files, or delegate to a subagent unless the feedback explicitly requires it. Do not ask the user for implementation details. Stop as soon as the implementation is complete and verified, then finish with evidence.\n\nPrevious feedback:\n{}\n\nOriginal task:\n{}",
        signal.as_str(),
        source_node.id,
        feedback,
        original_task
    )
}

pub(crate) fn concise_join(items: &[String], max_chars: usize) -> String {
    let joined = items
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if joined.chars().count() <= max_chars {
        joined
    } else {
        joined.chars().take(max_chars).collect::<String>()
    }
}

pub(crate) fn workflow_planner_result(request: HarnessRunRequest) -> HarnessRunResult {
    let feedback = request
        .backend_context
        .pointer("/coder/plan_context/workflow_feedback");
    let decision = workflow_planner_decision_from_feedback(feedback);
    let source_node = decision.source_node_id.as_deref().unwrap_or("");
    let signal = decision.source_signal.as_deref().unwrap_or("");
    let feedback_summary = workflow_feedback_summary(feedback);
    let mut report = if decision.decision == "blocked" {
        FinalReport::blocked(decision.summary, decision.blocker.as_deref().unwrap_or(""))
    } else {
        FinalReport::completed(decision.summary)
    };
    report.checks = vec![
        format!("workflow planner decision: {}", decision.decision),
        feedback_summary.clone(),
    ];
    if let Some(error) = decision.validation_error.as_deref() {
        report
            .checks
            .push(format!("workflow planner validation: {error}"));
    }
    let readiness = if decision.decision == "finish" {
        "finished"
    } else {
        decision.decision
    };
    HarnessRunResult {
        status: decision.decision.to_owned(),
        report: Some(report),
        events: vec![
            HarnessRunEvent::new(
                "planner.workflow_decision",
                json!({
                    "backend": "planner-model",
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "decision": decision.decision,
                    "source_node_id": source_node,
                    "source_signal": signal,
                    "summary": decision.summary,
                    "feedback": feedback.cloned().unwrap_or(Value::Null),
                    "validation_status": decision.validation_status,
                    "validation_error": decision.validation_error
                }),
            ),
            HarnessRunEvent::new(
                "planner.readiness.changed",
                json!({
                    "backend": "planner-model",
                    "readiness": readiness
                }),
            ),
        ],
    }
}

struct WorkflowPlannerDecision {
    decision: &'static str,
    summary: &'static str,
    validation_status: &'static str,
    validation_error: Option<String>,
    blocker: Option<String>,
    source_node_id: Option<String>,
    source_signal: Option<String>,
}

fn workflow_planner_decision_from_feedback(feedback: Option<&Value>) -> WorkflowPlannerDecision {
    let Some(feedback) = feedback else {
        return WorkflowPlannerDecision {
            decision: "ready",
            summary: "Workflow planner released the executor to start work.",
            validation_status: "initial_no_feedback",
            validation_error: None,
            blocker: None,
            source_node_id: None,
            source_signal: None,
        };
    };
    let source_node_id = feedback
        .get("source_node_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let source_signal = feedback
        .get("signal")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    let Some(source_node) = source_node_id.as_deref() else {
        return invalid_workflow_planner_feedback(
            "workflow_feedback.source_node_id is required",
            source_node_id,
            source_signal,
        );
    };
    let Some(signal_text) = source_signal.as_deref() else {
        return invalid_workflow_planner_feedback(
            "workflow_feedback.signal is required",
            source_node_id,
            source_signal,
        );
    };
    let Some(signal) = WorkflowSignal::from_status(signal_text) else {
        return invalid_workflow_planner_feedback(
            &format!("workflow_feedback.signal '{signal_text}' is unsupported"),
            source_node_id,
            source_signal,
        );
    };

    match signal {
        WorkflowSignal::Completed
            if source_node == "executor"
                && feedback
                    .pointer("/evidence_policy/checks_present")
                    .and_then(Value::as_bool)
                    .unwrap_or(false) =>
        {
            WorkflowPlannerDecision {
            decision: "finish",
            summary: "Workflow planner accepted the executor's evidence and finished the run.",
            validation_status: "valid_feedback",
            validation_error: None,
            blocker: None,
            source_node_id,
            source_signal,
            }
        }
        WorkflowSignal::Completed => invalid_workflow_planner_feedback(
            &format!(
                "workflow planner cannot finish from source node '{source_node}' without executor evidence"
            ),
            source_node_id,
            source_signal,
        ),
        WorkflowSignal::Blocked if workflow_feedback_requires_external_state(feedback) => {
            let blocker = workflow_feedback_blockers(feedback)
                .into_iter()
                .next()
                .unwrap_or_else(|| "workflow execution requires external state".to_owned());
            WorkflowPlannerDecision {
                decision: "blocked",
                summary: "Workflow planner stopped because execution requires external state rather than a code repair.",
                validation_status: "valid_external_blocker",
                validation_error: None,
                blocker: Some(blocker),
                source_node_id,
                source_signal,
            }
        }
        WorkflowSignal::Failed | WorkflowSignal::Blocked | WorkflowSignal::Continue => {
            WorkflowPlannerDecision {
                decision: "continue",
                summary: "Workflow planner routed execution feedback back to the executor for repair.",
                validation_status: "valid_feedback",
                validation_error: None,
                blocker: None,
                source_node_id,
                source_signal,
            }
        }
        WorkflowSignal::Cancelled => invalid_workflow_planner_feedback(
            &format!("source node '{source_node}' was cancelled"),
            source_node_id,
            source_signal,
        ),
        WorkflowSignal::Ready | WorkflowSignal::Finish => invalid_workflow_planner_feedback(
            &format!("workflow_feedback.signal '{signal_text}' is not a valid planner input"),
            source_node_id,
            source_signal,
        ),
    }
}

fn workflow_feedback_requires_external_state(feedback: &Value) -> bool {
    workflow_feedback_blockers(feedback).iter().any(|blocker| {
        let blocker = blocker.to_ascii_lowercase();
        [
            "not configured",
            "credential",
            "api key",
            "permission denied",
            "requires approval",
            "network unavailable",
            "external dependency",
        ]
        .iter()
        .any(|marker| blocker.contains(marker))
    })
}

fn workflow_feedback_blockers(feedback: &Value) -> Vec<String> {
    feedback
        .get("blockers")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn invalid_workflow_planner_feedback(
    reason: &str,
    source_node_id: Option<String>,
    source_signal: Option<String>,
) -> WorkflowPlannerDecision {
    WorkflowPlannerDecision {
        decision: "blocked",
        summary: "Workflow planner blocked because workflow feedback failed validation.",
        validation_status: "invalid_feedback",
        validation_error: Some(reason.to_owned()),
        blocker: Some(reason.to_owned()),
        source_node_id,
        source_signal,
    }
}

fn workflow_feedback_summary(feedback: Option<&Value>) -> String {
    let Some(feedback) = feedback else {
        return "workflow feedback: initial planner pass".to_owned();
    };
    let source = feedback
        .get("source_node_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let signal = feedback
        .get("signal")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let blockers = feedback
        .get("blockers")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let checks = feedback
        .get("checks")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let details = if blockers.is_empty() {
        concise_join(&checks, 360)
    } else {
        concise_join(&blockers, 360)
    };
    if details.is_empty() {
        format!("workflow feedback: {source} returned {signal}")
    } else {
        format!("workflow feedback: {source} returned {signal}; {details}")
    }
}
